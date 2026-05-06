#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::{
    capabilities::{PulseServerKind, resolve_linux_plan_with_readiness},
    core::{
        converter::FrameReblocker,
        manager::{
            BoundaryKind, InputStatusHandle, ManagerError, MultiInputManager, SessionBoundary,
            StorageBudget, default_block_frames_for_rate,
        },
        ordering::{self, Failure, Overflow, Stop},
        planner::{CapturePlanReport, CaptureRequest},
        reads::{Cancellation, PartialReason, ReadError, ReadResult},
        routing::RouteRollbackGuard,
        schema::{
            BackendId, ByteOrder, CaptureTarget, Channel, CombineResolution, DefaultDeviceMode,
            Format, Layout, MembershipDescriptor, MixedBlockMetadata, OnFormatChange, OnRootExit,
            ParticipationState, ProcessIdentity, ProcessScope, SampleType, Stamp, TargetKind,
        },
        traits::{
            AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
            TimingRequirement, TopologyControllerBackend,
        },
    },
    discovery::{
        PlatformDefaultInputObserver, PlatformDefaultOutputObserver, collect_pulse_state,
        query_linux_default_sink_now, query_linux_default_source_now,
        spawn_pulse_new_sink_input_watcher,
    },
    loader::{LinuxClientPaths, probe_pipewire_server, probe_pulse_server},
    pipewire_stream::{
        PlatformTopologyController, open_pipewire_point, open_pipewire_source_point,
        start_pipewire_topology_controller,
    },
    procfs::{
        collect_proc_stat_table, inspect_process_executable, inspect_process_in_proc_root,
        pid_belongs_to_target,
    },
    pulse_stream::{PlatformStream, open_pulse_point, open_pulse_source_point},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering::SeqCst},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_FRAMES_PER_SLOT: usize = 4096;
const PA_PROC_QUANTUM_FRAMES: usize = 480;

fn fallback_working_format() -> Result<Arc<Format>, CapabilityError> {
    Format::new(
        SampleType::F32,
        ByteOrder::Little,
        48_000,
        2,
        Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
    )
    .map(Arc::new)
    .map_err(|_| CapabilityError::UnsupportedFormat)
}

fn clean_target_device_id(target: &CaptureTarget) -> Option<String> {
    target.clean_device_id().map(str::to_owned)
}

fn resolve_pinned_linux_target(
    target: &mut CaptureTarget,
    backend: BackendId,
    paths: &LinuxClientPaths,
) {
    if target.default_device_mode() != DefaultDeviceMode::Pin {
        return;
    }
    let resolved = match target.kind() {
        TargetKind::DefaultOutput => {
            let out_id = query_linux_default_sink_now(backend, paths);
            target.resolve_pinned_endpoint(out_id.as_deref(), None)
        }
        TargetKind::DefaultInput => {
            let in_id = query_linux_default_source_now(backend, paths);
            target.resolve_pinned_endpoint(None, in_id.as_deref())
        }
        _ => return,
    };
    *target = resolved;
}

fn validate_linux_process_targets(targets: &[CaptureTarget]) -> Result<(), CapabilityError> {
    if targets.is_empty() {
        return Err(CapabilityError::InvalidTarget);
    }
    for target in targets {
        target.validate()?;
        if matches!(target.kind(), TargetKind::Process | TargetKind::Application) {
            let pid = target.pid().ok_or(CapabilityError::InvalidTarget)?;
            let expected_gen = target
                .process_start_time()
                .ok_or(CapabilityError::InvalidTarget)?;
            let (stat, _) = inspect_process_in_proc_root(Path::new("/proc"), pid)?;
            if expected_gen != 0 && stat.starttime_ticks != expected_gen {
                return Err(CapabilityError::PidReused);
            }
        }
    }
    Ok(())
}

struct SoftwareMixInput {
    target_idx: usize,
    backend: BackendId,
    stream: PlatformStream,
    last_active: Option<Instant>,
    sub_reblocker: Option<FrameReblocker>,
    last_stamp: Option<Stamp>,
    eof: bool,
}

impl SoftwareMixInput {
    fn new_direct(target_idx: usize, backend: BackendId, stream: PlatformStream) -> Self {
        Self {
            target_idx,
            backend,
            stream,
            last_active: Some(Instant::now()),
            sub_reblocker: None,
            last_stamp: None,
            eof: false,
        }
    }

    fn new_pulse_substream(
        target_idx: usize,
        backend: BackendId,
        stream: PlatformStream,
    ) -> Result<Self, CapabilityError> {
        let reblocker = FrameReblocker::new(2, PA_PROC_QUANTUM_FRAMES, MAX_FRAMES_PER_SLOT * 4)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        Ok(Self {
            target_idx,
            backend,
            stream,
            last_active: Some(Instant::now()),
            sub_reblocker: Some(reblocker),
            last_stamp: None,
            eof: false,
        })
    }
}

struct PaSubWatch {
    dirty: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl PaSubWatch {
    fn spawn(paths: &LinuxClientPaths) -> Self {
        let dirty = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let join = spawn_pulse_new_sink_input_watcher(paths, dirty.clone(), stop.clone());
        Self { dirty, stop, join }
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for PaSubWatch {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[allow(clippy::large_enum_variant)]
enum PlatformExecutionEngine {
    PassThrough {
        stream: Option<PlatformStream>,
        topology: Option<PlatformTopologyController>,
    },
    SoftwareMix {
        block_frames: usize,
        multi: Box<MultiInputManager>,
        inputs: Vec<SoftwareMixInput>,
        sub_topologies: Vec<(usize, PlatformTopologyController)>,
        opened_sink_inputs: BTreeSet<(usize, u32)>,
        pa_sub_watch: Option<PaSubWatch>,
        last_sink_input_scan: Instant,
        started_at: Instant,
        in_bytes_scratch: Vec<u8>,
        pulse_sub_f32: Vec<f32>,
        pulse_sum_f32: Vec<f32>,
        pulse_sum_bytes: Vec<u8>,
        drained_blocks: u64,
    },
}

fn build_software_mix_multi(
    request: CaptureRequest,
    per_input_formats: Vec<(BackendId, Arc<Format>)>,
    target_active: &[bool],
    shared_budget: StorageBudget,
) -> Result<(usize, Box<MultiInputManager>), CapabilityError> {
    let max_rate = per_input_formats
        .iter()
        .map(|(_, f)| f.rate())
        .max()
        .unwrap_or(48_000);
    let block_frames = default_block_frames_for_rate(max_rate, MAX_FRAMES_PER_SLOT);
    let multi = MultiInputManager::new_heterogeneous_with_budget(
        request,
        per_input_formats,
        block_frames,
        MAX_FRAMES_PER_SLOT,
        shared_budget,
    )
    .map_err(CapabilityError::from)?;

    for (t_idx, &is_act) in target_active.iter().enumerate() {
        let st = if is_act {
            ParticipationState::Active
        } else {
            ParticipationState::Idle
        };
        let _ = multi.set_input_state(t_idx, st);
    }
    Ok((block_frames, Box::new(multi)))
}

/// Supports pass-through, shared PipeWire graph mixing, and cross-backend software mixing.
/// Steady-state `read_into` uses no heap allocations; the output pool shares the storage budget.
pub struct PlatformManager {
    request: CaptureRequest,
    paths: LinuxClientPaths,
    plan: CapturePlanReport,
    policy: Overflow,
    timing: TimingRequirement,
    pulse_server_kind: Option<PulseServerKind>,
    budget: StorageBudget,
    cleanup_timeout: Duration,
    engine: PlatformExecutionEngine,
    route_guard: RouteRollbackGuard,
    default_cancel: Cancellation,
    last_metadata: Option<MixedBlockMetadata>,
    pending_boundary: Option<SessionBoundary>,
    opened_default_device: Option<String>,
    default_observer: Option<PlatformDefaultOutputObserver>,
    opened_default_source: Option<String>,
    default_source_observer: Option<PlatformDefaultInputObserver>,
    last_default_check: Instant,
    session_counter: u64,
    segment_counter: u64,
    stopped: bool,
    terminal_error: Option<ManagerError>,
    read_lock: Mutex<()>,
}

impl PlatformManager {
    fn open_impl(
        request: CaptureRequest,
        paths: LinuxClientPaths,
        policy: Overflow,
        timing: TimingRequirement,
        observed: bool,
    ) -> Result<Self, CapabilityError> {
        let cleanup_timeout = Duration::from_nanos(request.budgets().cleanup_timeout_ns);
        if cleanup_timeout < Duration::from_millis(50) || cleanup_timeout > Duration::from_secs(30)
        {
            return Err(CapabilityError::InvalidTarget);
        }

        validate_linux_process_targets(request.targets())?;

        let pw_ready = probe_pipewire_server(&paths).map(|_| ());
        let pa_probe = probe_pulse_server(&paths);
        let pa_ready = pa_probe.as_ref().map(|_| ()).map_err(Clone::clone);
        if pw_ready.is_err() && pa_ready.is_err() {
            crate::permissions::ensure_linux_socket_permissions(request.backends(), &paths)?;
        }

        let plan = resolve_linux_plan_with_readiness(
            &request,
            timing,
            pw_ready.clone(),
            pa_ready.clone(),
        )?;

        let any_pulse = plan
            .target_plans
            .iter()
            .any(|p| p.resolved_backend == BackendId::PulseAudio);
        let pulse_server_kind = if any_pulse {
            pa_probe.as_ref().ok().map(|info| info.server_kind)
        } else {
            None
        };

        let mut resolved_targets = request.targets().to_vec();
        for (idx, target) in resolved_targets.iter_mut().enumerate() {
            resolve_pinned_linux_target(target, plan.target_plans[idx].resolved_backend, &paths);
        }

        let follow_out_backend = resolved_targets
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.kind() == TargetKind::DefaultOutput
                    && t.default_device_mode() == DefaultDeviceMode::Follow
            })
            .map(|(idx, _)| plan.target_plans[idx].resolved_backend);
        let follow_in_backend = resolved_targets
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.kind() == TargetKind::DefaultInput
                    && t.default_device_mode() == DefaultDeviceMode::Follow
            })
            .map(|(idx, _)| plan.target_plans[idx].resolved_backend);

        let initial_default_sink =
            follow_out_backend.and_then(|b| query_linux_default_sink_now(b, &paths));
        let initial_default_source =
            follow_in_backend.and_then(|b| query_linux_default_source_now(b, &paths));

        let budget = StorageBudget::new(
            request.budgets().max_active_streams.max(2),
            request.budgets().max_total_pcm_memory,
        )
        .map_err(|_| CapabilityError::CapacityExceeded)?;

        let route_guard = RouteRollbackGuard::new(request.allow_reroute());
        let fallback_pcm = fallback_working_format()?;
        let final_request = request.with_targets(resolved_targets);

        let engine = match plan.combine_resolution {
            CombineResolution::PassThrough(b) => {
                let target = &final_request.targets()[0];
                match (b, target.kind()) {
                    (BackendId::PipeWire, TargetKind::Device | TargetKind::DefaultOutput) => {
                        let dev = clean_target_device_id(target)
                            .or_else(|| query_linux_default_sink_now(BackendId::PipeWire, &paths));
                        let stream = open_pipewire_point(
                            &paths,
                            dev.as_deref(),
                            true,
                            policy,
                            timing,
                            &budget,
                            observed,
                        )?;
                        PlatformExecutionEngine::PassThrough {
                            stream: Some(PlatformStream::PipeWire(stream)),
                            topology: None,
                        }
                    }
                    (BackendId::PipeWire, TargetKind::InputDevice | TargetKind::DefaultInput) => {
                        let src = clean_target_device_id(target);
                        let stream = open_pipewire_source_point(
                            &paths,
                            src.as_deref(),
                            None,
                            policy,
                            timing,
                            &budget,
                            observed,
                        )?;
                        PlatformExecutionEngine::PassThrough {
                            stream: Some(PlatformStream::PipeWire(stream)),
                            topology: None,
                        }
                    }
                    (BackendId::PipeWire, TargetKind::Process | TargetKind::Application) => {
                        let topo = start_pipewire_topology_controller(
                            &paths,
                            final_request.targets().to_vec(),
                            final_request.budgets().max_native_graph_links,
                        )?;
                        let stream = match open_pipewire_point(
                            &paths,
                            Some(topo.sink_name()),
                            true,
                            policy,
                            timing,
                            &budget,
                            observed,
                        ) {
                            Ok(s) => s,
                            Err(err) => {
                                topo.shutdown();
                                return Err(err);
                            }
                        };
                        PlatformExecutionEngine::PassThrough {
                            stream: Some(PlatformStream::PipeWire(stream)),
                            topology: Some(topo),
                        }
                    }
                    (BackendId::PulseAudio, TargetKind::Device | TargetKind::DefaultOutput) => {
                        let dev = clean_target_device_id(target);
                        let stream = open_pulse_point(
                            &paths,
                            dev.as_deref(),
                            None,
                            policy,
                            timing,
                            &budget,
                            observed,
                        )?;
                        PlatformExecutionEngine::PassThrough {
                            stream: Some(PlatformStream::Pulse(stream)),
                            topology: None,
                        }
                    }
                    (BackendId::PulseAudio, TargetKind::InputDevice | TargetKind::DefaultInput) => {
                        let src = clean_target_device_id(target);
                        let stream = open_pulse_source_point(
                            &paths,
                            src.as_deref(),
                            None,
                            policy,
                            timing,
                            &budget,
                            observed,
                        )?;
                        PlatformExecutionEngine::PassThrough {
                            stream: Some(PlatformStream::Pulse(stream)),
                            topology: None,
                        }
                    }
                    _ => return Err(CapabilityError::NotImplemented),
                }
            }
            CombineResolution::SharedNativeGraphMix(BackendId::PipeWire) => {
                let topo = start_pipewire_topology_controller(
                    &paths,
                    final_request.targets().to_vec(),
                    final_request.budgets().max_native_graph_links,
                )?;
                let stream = match open_pipewire_point(
                    &paths,
                    Some(topo.sink_name()),
                    true,
                    policy,
                    timing,
                    &budget,
                    observed,
                ) {
                    Ok(s) => s,
                    Err(err) => {
                        topo.shutdown();
                        return Err(err);
                    }
                };
                PlatformExecutionEngine::PassThrough {
                    stream: Some(PlatformStream::PipeWire(stream)),
                    topology: Some(topo),
                }
            }
            CombineResolution::CspaMixer => {
                let mut inputs: Vec<SoftwareMixInput> = Vec::new();
                let mut sub_topologies: Vec<(usize, PlatformTopologyController)> = Vec::new();
                let mut opened_sink_inputs: BTreeSet<(usize, u32)> = BTreeSet::new();
                let mut per_input_formats: Vec<(BackendId, Arc<Format>)> =
                    Vec::with_capacity(final_request.targets().len());
                let mut target_initially_active = vec![false; final_request.targets().len()];

                let need_pa_proc_scan =
                    final_request.targets().iter().enumerate().any(|(idx, t)| {
                        plan.target_plans[idx].resolved_backend == BackendId::PulseAudio
                            && matches!(t.kind(), TargetKind::Process | TargetKind::Application)
                    });
                let (sink_inputs, proc_map) = if need_pa_proc_scan {
                    let (_devs, _pids, si) = collect_pulse_state(&paths)?;
                    let pm = collect_proc_stat_table(Path::new("/proc"))?;
                    (si, pm)
                } else {
                    (Vec::new(), BTreeMap::new())
                };

                let open_outcome: Result<(), CapabilityError> = (|| {
                    for (t_idx, target) in final_request.targets().iter().enumerate() {
                        let t_backend = plan.target_plans[t_idx].resolved_backend;
                        match (t_backend, target.kind()) {
                            (
                                BackendId::PipeWire,
                                TargetKind::Device | TargetKind::DefaultOutput,
                            ) => {
                                let dev = clean_target_device_id(target).or_else(|| {
                                    query_linux_default_sink_now(BackendId::PipeWire, &paths)
                                });
                                let stream = open_pipewire_point(
                                    &paths,
                                    dev.as_deref(),
                                    true,
                                    policy,
                                    timing,
                                    &budget,
                                    observed,
                                )?;
                                per_input_formats.push((t_backend, stream.format().clone()));
                                target_initially_active[t_idx] = true;
                                inputs.push(SoftwareMixInput::new_direct(
                                    t_idx,
                                    t_backend,
                                    PlatformStream::PipeWire(stream),
                                ));
                            }
                            (
                                BackendId::PipeWire,
                                TargetKind::InputDevice | TargetKind::DefaultInput,
                            ) => {
                                let src = clean_target_device_id(target);
                                let stream = open_pipewire_source_point(
                                    &paths,
                                    src.as_deref(),
                                    None,
                                    policy,
                                    timing,
                                    &budget,
                                    observed,
                                )?;
                                per_input_formats.push((t_backend, stream.format().clone()));
                                target_initially_active[t_idx] = true;
                                inputs.push(SoftwareMixInput::new_direct(
                                    t_idx,
                                    t_backend,
                                    PlatformStream::PipeWire(stream),
                                ));
                            }
                            (
                                BackendId::PipeWire,
                                TargetKind::Process | TargetKind::Application,
                            ) => {
                                let single_topo = start_pipewire_topology_controller(
                                    &paths,
                                    vec![target.clone()],
                                    final_request.budgets().max_native_graph_links,
                                )?;
                                let stream = match open_pipewire_point(
                                    &paths,
                                    Some(single_topo.sink_name()),
                                    true,
                                    policy,
                                    timing,
                                    &budget,
                                    observed,
                                ) {
                                    Ok(s) => s,
                                    Err(err) => {
                                        single_topo.shutdown();
                                        return Err(err);
                                    }
                                };
                                per_input_formats.push((t_backend, stream.format().clone()));
                                target_initially_active[t_idx] = true;
                                sub_topologies.push((t_idx, single_topo));
                                inputs.push(SoftwareMixInput::new_direct(
                                    t_idx,
                                    t_backend,
                                    PlatformStream::PipeWire(stream),
                                ));
                            }
                            (
                                BackendId::PulseAudio,
                                TargetKind::Device | TargetKind::DefaultOutput,
                            ) => {
                                let dev = clean_target_device_id(target);
                                let stream = open_pulse_point(
                                    &paths,
                                    dev.as_deref(),
                                    None,
                                    policy,
                                    timing,
                                    &budget,
                                    observed,
                                )?;
                                per_input_formats.push((t_backend, stream.format().clone()));
                                target_initially_active[t_idx] = true;
                                inputs.push(SoftwareMixInput::new_direct(
                                    t_idx,
                                    t_backend,
                                    PlatformStream::Pulse(stream),
                                ));
                            }
                            (
                                BackendId::PulseAudio,
                                TargetKind::InputDevice | TargetKind::DefaultInput,
                            ) => {
                                let src = clean_target_device_id(target);
                                let stream = open_pulse_source_point(
                                    &paths,
                                    src.as_deref(),
                                    None,
                                    policy,
                                    timing,
                                    &budget,
                                    observed,
                                )?;
                                per_input_formats.push((t_backend, stream.format().clone()));
                                target_initially_active[t_idx] = true;
                                inputs.push(SoftwareMixInput::new_direct(
                                    t_idx,
                                    t_backend,
                                    PlatformStream::Pulse(stream),
                                ));
                            }
                            (
                                BackendId::PulseAudio,
                                TargetKind::Process | TargetKind::Application,
                            ) => {
                                let root_pid =
                                    target.pid().ok_or(CapabilityError::InvalidTarget)?;
                                let root_id = ProcessIdentity {
                                    pid: root_pid,
                                    generation: target.process_start_time().unwrap_or(0),
                                };
                                let exact_only = target.scope() == ProcessScope::Exact;
                                let mut matched_any = false;
                                for si in &sink_inputs {
                                    if let Some(si_pid) = si.pid
                                        && pid_belongs_to_target(
                                            &proc_map, root_id, exact_only, si_pid,
                                        )?
                                    {
                                        let stream = open_pulse_point(
                                            &paths,
                                            None,
                                            Some(si.index),
                                            policy,
                                            timing,
                                            &budget,
                                            observed,
                                        )?;
                                        opened_sink_inputs.insert((t_idx, si.index));
                                        inputs.push(SoftwareMixInput::new_pulse_substream(
                                            t_idx,
                                            t_backend,
                                            PlatformStream::Pulse(stream),
                                        )?);
                                        matched_any = true;
                                    }
                                }
                                target_initially_active[t_idx] = matched_any;
                                per_input_formats.push((t_backend, fallback_pcm.clone()));
                            }
                            _ => return Err(CapabilityError::InvalidBackend),
                        }
                    }
                    Ok(())
                })();

                if let Err(err) = open_outcome {
                    for (_, topo) in sub_topologies {
                        topo.shutdown();
                    }
                    for inp in inputs {
                        inp.stream.stop(Stop::Abort);
                        let _ = inp.stream.wait_cleanup(cleanup_timeout);
                    }
                    return Err(err);
                }

                let (block_frames, multi) = match build_software_mix_multi(
                    final_request.clone(),
                    per_input_formats,
                    &target_initially_active,
                    budget.clone(),
                ) {
                    Ok(pair) => pair,
                    Err(err) => {
                        for (_, topo) in sub_topologies {
                            topo.shutdown();
                        }
                        for inp in inputs {
                            inp.stream.stop(Stop::Abort);
                            let _ = inp.stream.wait_cleanup(cleanup_timeout);
                        }
                        return Err(err);
                    }
                };

                let ready_now = Instant::now();
                for inp in &mut inputs {
                    inp.last_active = Some(ready_now);
                }

                let max_in_stride = inputs
                    .iter()
                    .map(|inp| inp.stream.format().bytes_per_frame())
                    .max()
                    .unwrap_or(8)
                    .max(8);
                let pa_sub_watch = need_pa_proc_scan.then(|| PaSubWatch::spawn(&paths));

                PlatformExecutionEngine::SoftwareMix {
                    block_frames,
                    multi,
                    inputs,
                    sub_topologies,
                    opened_sink_inputs,
                    pa_sub_watch,
                    last_sink_input_scan: Instant::now(),
                    started_at: Instant::now(),
                    in_bytes_scratch: vec![0u8; MAX_FRAMES_PER_SLOT * max_in_stride],
                    pulse_sub_f32: vec![0.0_f32; MAX_FRAMES_PER_SLOT * 2],
                    pulse_sum_f32: vec![0.0_f32; MAX_FRAMES_PER_SLOT * 2],
                    pulse_sum_bytes: vec![0u8; MAX_FRAMES_PER_SLOT * 8],
                    drained_blocks: 0,
                }
            }
            _ => return Err(CapabilityError::NotImplemented),
        };

        let final_plan =
            resolve_linux_plan_with_readiness(&final_request, timing, pw_ready, pa_ready)
                .unwrap_or(plan);
        let default_observer = follow_out_backend
            .map(|b| PlatformDefaultOutputObserver::acquire_for_backend(Some(b), paths.clone()));
        let default_source_observer = follow_in_backend
            .map(|b| PlatformDefaultInputObserver::acquire_for_backend(Some(b), paths.clone()));

        Ok(Self {
            request: final_request,
            paths,
            plan: final_plan,
            policy,
            timing,
            pulse_server_kind,
            budget,
            cleanup_timeout,
            engine,
            route_guard,
            default_cancel: Cancellation::default(),
            last_metadata: None,
            pending_boundary: None,
            opened_default_device: initial_default_sink,
            default_observer,
            opened_default_source: initial_default_source,
            default_source_observer,
            last_default_check: Instant::now(),
            session_counter: 1,
            segment_counter: 1,
            stopped: false,
            terminal_error: None,
            read_lock: Mutex::new(()),
        })
    }

    pub fn budget(&self) -> &StorageBudget {
        &self.budget
    }

    pub fn topology_controller(&self) -> Option<&PlatformTopologyController> {
        match &self.engine {
            PlatformExecutionEngine::PassThrough { topology, .. } => topology.as_ref(),
            PlatformExecutionEngine::SoftwareMix { .. } => None,
        }
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn open_with_paths(
        request: CaptureRequest,
        paths: LinuxClientPaths,
        observed: bool,
    ) -> Result<Self, CapabilityError> {
        Self::open_impl(
            request,
            paths,
            Overflow::DropOldest,
            TimingRequirement::Raw,
            observed,
        )
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn pulse_server_kind(&self) -> Option<PulseServerKind> {
        self.pulse_server_kind
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn from_synthetic_streams(
        request: CaptureRequest,
        streams: Vec<(BackendId, PlatformStream)>,
        shared_budget: StorageBudget,
    ) -> Result<Self, CapabilityError> {
        let plan =
            resolve_linux_plan_with_readiness(&request, TimingRequirement::Raw, Ok(()), Ok(()))?;
        let cleanup_timeout = Duration::from_nanos(request.budgets().cleanup_timeout_ns);
        let route_guard = RouteRollbackGuard::new(request.allow_reroute());
        let engine = if matches!(plan.combine_resolution, CombineResolution::PassThrough(_))
            && streams.len() == 1
        {
            let (_b, st) = streams.into_iter().next().unwrap();
            PlatformExecutionEngine::PassThrough {
                stream: Some(st),
                topology: None,
            }
        } else {
            let per_input_formats: Vec<(BackendId, Arc<Format>)> = streams
                .iter()
                .map(|(b, s)| (*b, s.format().clone()))
                .collect();
            let active = vec![true; streams.len()];
            let max_in_stride = streams
                .iter()
                .map(|(_, s)| s.format().bytes_per_frame())
                .max()
                .unwrap_or(8)
                .max(8);
            let inputs: Vec<SoftwareMixInput> = streams
                .into_iter()
                .enumerate()
                .map(|(idx, (b, s))| SoftwareMixInput::new_direct(idx, b, s))
                .collect();
            let (block_frames, multi) = match build_software_mix_multi(
                request.clone(),
                per_input_formats,
                &active,
                shared_budget.clone(),
            ) {
                Ok(pair) => pair,
                Err(err) => {
                    for inp in inputs {
                        inp.stream.stop(Stop::Abort);
                        let _ = inp.stream.wait_cleanup(cleanup_timeout);
                    }
                    return Err(err);
                }
            };
            PlatformExecutionEngine::SoftwareMix {
                block_frames,
                multi,
                inputs,
                sub_topologies: Vec::new(),
                opened_sink_inputs: BTreeSet::new(),
                pa_sub_watch: None,
                last_sink_input_scan: Instant::now(),
                started_at: Instant::now(),
                in_bytes_scratch: vec![0u8; MAX_FRAMES_PER_SLOT * max_in_stride],
                pulse_sub_f32: vec![0.0_f32; MAX_FRAMES_PER_SLOT * 2],
                pulse_sum_f32: vec![0.0_f32; MAX_FRAMES_PER_SLOT * 2],
                pulse_sum_bytes: vec![0u8; MAX_FRAMES_PER_SLOT * 8],
                drained_blocks: 0,
            }
        };
        Ok(Self {
            request,
            paths: LinuxClientPaths::default(),
            plan,
            policy: Overflow::Fail,
            timing: TimingRequirement::Raw,
            pulse_server_kind: None,
            budget: shared_budget,
            cleanup_timeout,
            engine,
            route_guard,
            default_cancel: Cancellation::default(),
            last_metadata: None,
            pending_boundary: None,
            opened_default_device: None,
            default_observer: None,
            opened_default_source: None,
            default_source_observer: None,
            last_default_check: Instant::now(),
            session_counter: 1,
            segment_counter: 1,
            stopped: false,
            terminal_error: None,
            read_lock: Mutex::new(()),
        })
    }

    #[cfg(test)]
    fn apply_opened_default_stream_for_test(
        &mut self,
        expected_kind: TargetKind,
        next_stream: PlatformStream,
    ) -> Result<bool, CapabilityError> {
        if self.stopped || self.terminal_error.is_some() {
            return Err(CapabilityError::Stopped);
        }
        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                let target = self
                    .request
                    .targets()
                    .first()
                    .ok_or(CapabilityError::InvalidTarget)?;
                if target.kind() != expected_kind
                    || target.default_device_mode() != DefaultDeviceMode::Follow
                {
                    return Err(CapabilityError::InvalidTarget);
                }
                let old_pcm = stream.as_ref().map(|s| s.format().clone());
                if let Some(old) = stream.take() {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }
                if old_pcm
                    .as_ref()
                    .is_some_and(|prev| prev != next_stream.format())
                    && self.request.on_format_change() == OnFormatChange::Fail
                {
                    next_stream.stop(Stop::Abort);
                    let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(ManagerError::Unsupported);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                let next_fmt = next_stream.format().clone();
                *stream = Some(next_stream);
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::DeviceChanged,
                    format: next_fmt,
                    device_id: Some("synth-switched".into()),
                    identity: None,
                });
                Ok(true)
            }
            PlatformExecutionEngine::SoftwareMix {
                block_frames,
                multi,
                inputs,
                in_bytes_scratch,
                ..
            } => {
                let idx = self
                    .request
                    .targets()
                    .iter()
                    .position(|t| {
                        t.kind() == expected_kind
                            && t.default_device_mode() == DefaultDeviceMode::Follow
                    })
                    .ok_or(CapabilityError::InvalidTarget)?;
                let old_pcm = inputs
                    .iter()
                    .find(|inp| inp.target_idx == idx)
                    .map(|inp| inp.stream.format().clone())
                    .or_else(|| {
                        multi
                            .input_handles()
                            .get(idx)
                            .map(|h| h.native_format().clone())
                    });
                if old_pcm
                    .as_ref()
                    .is_some_and(|prev| prev != next_stream.format())
                    && self.request.on_format_change() == OnFormatChange::Fail
                {
                    next_stream.stop(Stop::Abort);
                    let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(ManagerError::Unsupported);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                let stride = next_stream.format().bytes_per_frame();
                if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * stride {
                    in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * stride, 0);
                }
                if let Some(inp) = inputs.iter_mut().find(|inp| inp.target_idx == idx) {
                    inp.stream.stop(Stop::Drain);
                    let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                    inp.stream = next_stream;
                    inp.last_active = Some(Instant::now());
                    inp.eof = false;
                }
                let _ = multi.set_input_state(idx, ParticipationState::Active);
                let fallback_fmt = multi.working_format().clone();
                let per_input: Vec<(BackendId, Arc<Format>)> = (0..self.request.targets().len())
                    .map(|t_idx| {
                        let b = self.plan.target_plans[t_idx].resolved_backend;
                        let fmt = inputs
                            .iter()
                            .find(|inp| inp.target_idx == t_idx)
                            .map(|inp| inp.stream.format().clone())
                            .unwrap_or_else(|| fallback_fmt.clone());
                        (b, fmt)
                    })
                    .collect();
                match multi.reconfigure_targets_with_formats(
                    self.request.targets().to_vec(),
                    per_input.clone(),
                ) {
                    Ok(_) => {}
                    Err(ManagerError::WorkingFormatChanged) => {
                        let active: Vec<bool> = (0..self.request.targets().len())
                            .map(|t_idx| {
                                inputs.iter().any(|inp| inp.target_idx == t_idx && !inp.eof)
                            })
                            .collect();
                        let (new_bf, new_multi) = build_software_mix_multi(
                            self.request.clone(),
                            per_input,
                            &active,
                            self.budget.clone(),
                        )?;
                        *block_frames = new_bf;
                        *multi = new_multi;
                    }
                    Err(err) => {
                        self.terminal_error = Some(err);
                        return Err(CapabilityError::from(err));
                    }
                }
                self.segment_counter = self.segment_counter.saturating_add(1);
                Ok(true)
            }
        }
    }

    fn refresh_default_observers(&mut self) {
        let follow_out = self
            .request
            .targets()
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.kind() == TargetKind::DefaultOutput
                    && t.default_device_mode() == DefaultDeviceMode::Follow
            })
            .map(|(idx, _)| self.plan.target_plans[idx].resolved_backend);
        if let Some(b) = follow_out {
            if self.default_observer.is_none() {
                self.default_observer = Some(PlatformDefaultOutputObserver::acquire_for_backend(
                    Some(b),
                    self.paths.clone(),
                ));
            }
        } else {
            self.default_observer = None;
        }

        let follow_in = self
            .request
            .targets()
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.kind() == TargetKind::DefaultInput
                    && t.default_device_mode() == DefaultDeviceMode::Follow
            })
            .map(|(idx, _)| self.plan.target_plans[idx].resolved_backend);
        if let Some(b) = follow_in {
            if self.default_source_observer.is_none() {
                self.default_source_observer = Some(
                    PlatformDefaultInputObserver::acquire_for_backend(Some(b), self.paths.clone()),
                );
            }
        } else {
            self.default_source_observer = None;
        }
    }

    fn update_targets_impl(
        &mut self,
        mut new_targets: Vec<CaptureTarget>,
    ) -> Result<u64, CapabilityError> {
        if self.stopped || self.terminal_error.is_some() {
            return Err(CapabilityError::Stopped);
        }
        validate_linux_process_targets(&new_targets)?;

        let pw_ready = probe_pipewire_server(&self.paths).map(|_| ());
        let pa_probe = probe_pulse_server(&self.paths);
        let pa_ready = pa_probe.as_ref().map(|_| ()).map_err(Clone::clone);
        let candidate_req = self.request.clone().with_targets(new_targets.clone());
        let new_plan = resolve_linux_plan_with_readiness(
            &candidate_req,
            self.timing,
            pw_ready.clone(),
            pa_ready.clone(),
        )?;

        if let PlatformExecutionEngine::PassThrough {
            topology: Some(topo),
            ..
        } = &mut self.engine
            && matches!(
                new_plan.combine_resolution,
                CombineResolution::SharedNativeGraphMix(BackendId::PipeWire)
                    | CombineResolution::PassThrough(BackendId::PipeWire)
            )
            && new_targets
                .iter()
                .all(|t| matches!(t.kind(), TargetKind::Process | TargetKind::Application))
            && self.request.output_converter().is_none()
        {
            topo.update_targets(new_targets.clone())?;
            self.request = self.request.clone().with_targets(new_targets);
            self.plan = new_plan;
            self.segment_counter = self.segment_counter.saturating_add(1);
            return Ok(self.segment_counter);
        }

        if let PlatformExecutionEngine::SoftwareMix {
            block_frames,
            multi,
            inputs,
            sub_topologies,
            opened_sink_inputs,
            pa_sub_watch,
            in_bytes_scratch,
            ..
        } = &mut self.engine
            && new_plan.combine_resolution == CombineResolution::CspaMixer
        {
            for (idx, t) in new_targets.iter_mut().enumerate() {
                resolve_pinned_linux_target(
                    t,
                    new_plan.target_plans[idx].resolved_backend,
                    &self.paths,
                );
            }

            let old_targets = self.request.targets();
            let old_handles = multi.input_handles();
            let mut old_used = vec![false; old_targets.len()];
            let mut planned_old_slot: Vec<Option<usize>> = Vec::with_capacity(new_targets.len());
            for (new_idx, t) in new_targets.iter().enumerate() {
                let want_backend = new_plan.target_plans[new_idx].resolved_backend;
                let mut found = None;
                for (old_idx, old_t) in old_targets.iter().enumerate() {
                    let old_alive = old_handles
                        .get(old_idx)
                        .and_then(InputStatusHandle::snapshot)
                        .is_none_or(|s| {
                            !matches!(
                                s.participation_state,
                                ParticipationState::Exited | ParticipationState::Failed(_)
                            )
                        });
                    if !old_used[old_idx]
                        && old_alive
                        && old_t.id() == t.id()
                        && old_t.scope() == t.scope()
                        && self.plan.target_plans[old_idx].resolved_backend == want_backend
                    {
                        old_used[old_idx] = true;
                        found = Some(old_idx);
                        break;
                    }
                }
                planned_old_slot.push(found);
            }

            let fallback_fmt = multi.working_format().clone();
            let any_new_pa_proc = new_targets.iter().enumerate().any(|(idx, t)| {
                new_plan.target_plans[idx].resolved_backend == BackendId::PulseAudio
                    && matches!(t.kind(), TargetKind::Process | TargetKind::Application)
            });
            let need_pa_proc_scan = new_targets.iter().enumerate().any(|(idx, t)| {
                planned_old_slot[idx].is_none()
                    && new_plan.target_plans[idx].resolved_backend == BackendId::PulseAudio
                    && matches!(t.kind(), TargetKind::Process | TargetKind::Application)
            });
            let (sink_inputs, proc_map) = if need_pa_proc_scan {
                let (_devs, _pids, si) = collect_pulse_state(&self.paths)?;
                let pm = collect_proc_stat_table(Path::new("/proc"))?;
                (si, pm)
            } else {
                (Vec::new(), BTreeMap::new())
            };

            let mut newly_opened_inputs: Vec<SoftwareMixInput> = Vec::new();
            let mut newly_opened_topos: Vec<(usize, PlatformTopologyController)> = Vec::new();
            let mut newly_opened_si: BTreeSet<(usize, u32)> = BTreeSet::new();
            let mut new_per_input_formats: Vec<(BackendId, Arc<Format>)> =
                Vec::with_capacity(new_targets.len());
            let mut new_slot_active: Vec<bool> = vec![false; new_targets.len()];

            for (new_idx, target) in new_targets.iter().enumerate() {
                let t_backend = new_plan.target_plans[new_idx].resolved_backend;
                if let Some(old_idx) = planned_old_slot[new_idx] {
                    let old_fmt = inputs
                        .iter()
                        .find(|inp| inp.target_idx == old_idx)
                        .map(|inp| inp.stream.format().clone())
                        .or_else(|| {
                            multi
                                .input_handles()
                                .get(old_idx)
                                .map(|h| h.native_format().clone())
                        })
                        .unwrap_or_else(|| fallback_fmt.clone());
                    new_per_input_formats.push((t_backend, old_fmt));
                    new_slot_active[new_idx] = inputs
                        .iter()
                        .any(|inp| inp.target_idx == old_idx && !inp.eof);
                    continue;
                }

                let open_res: Result<Arc<Format>, CapabilityError> = (|| {
                    let _ticket = multi
                        .acquire_pending_open()
                        .map_err(CapabilityError::from)?;
                    match (t_backend, target.kind()) {
                        (BackendId::PipeWire, TargetKind::Device | TargetKind::DefaultOutput) => {
                            let dev = clean_target_device_id(target).or_else(|| {
                                query_linux_default_sink_now(BackendId::PipeWire, &self.paths)
                            });
                            let stream = open_pipewire_point(
                                &self.paths,
                                dev.as_deref(),
                                true,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?;
                            let fmt = stream.format().clone();
                            new_slot_active[new_idx] = true;
                            newly_opened_inputs.push(SoftwareMixInput::new_direct(
                                new_idx,
                                t_backend,
                                PlatformStream::PipeWire(stream),
                            ));
                            Ok(fmt)
                        }
                        (
                            BackendId::PipeWire,
                            TargetKind::InputDevice | TargetKind::DefaultInput,
                        ) => {
                            let src = clean_target_device_id(target);
                            let stream = open_pipewire_source_point(
                                &self.paths,
                                src.as_deref(),
                                None,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?;
                            let fmt = stream.format().clone();
                            new_slot_active[new_idx] = true;
                            newly_opened_inputs.push(SoftwareMixInput::new_direct(
                                new_idx,
                                t_backend,
                                PlatformStream::PipeWire(stream),
                            ));
                            Ok(fmt)
                        }
                        (BackendId::PipeWire, TargetKind::Process | TargetKind::Application) => {
                            let single_topo = start_pipewire_topology_controller(
                                &self.paths,
                                vec![target.clone()],
                                self.request.budgets().max_native_graph_links,
                            )?;
                            let stream = match open_pipewire_point(
                                &self.paths,
                                Some(single_topo.sink_name()),
                                true,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            ) {
                                Ok(s) => s,
                                Err(err) => {
                                    single_topo.shutdown();
                                    return Err(err);
                                }
                            };
                            let fmt = stream.format().clone();
                            new_slot_active[new_idx] = true;
                            newly_opened_topos.push((new_idx, single_topo));
                            newly_opened_inputs.push(SoftwareMixInput::new_direct(
                                new_idx,
                                t_backend,
                                PlatformStream::PipeWire(stream),
                            ));
                            Ok(fmt)
                        }
                        (BackendId::PulseAudio, TargetKind::Device | TargetKind::DefaultOutput) => {
                            let dev = clean_target_device_id(target);
                            let stream = open_pulse_point(
                                &self.paths,
                                dev.as_deref(),
                                None,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?;
                            let fmt = stream.format().clone();
                            new_slot_active[new_idx] = true;
                            newly_opened_inputs.push(SoftwareMixInput::new_direct(
                                new_idx,
                                t_backend,
                                PlatformStream::Pulse(stream),
                            ));
                            Ok(fmt)
                        }
                        (
                            BackendId::PulseAudio,
                            TargetKind::InputDevice | TargetKind::DefaultInput,
                        ) => {
                            let src = clean_target_device_id(target);
                            let stream = open_pulse_source_point(
                                &self.paths,
                                src.as_deref(),
                                None,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?;
                            let fmt = stream.format().clone();
                            new_slot_active[new_idx] = true;
                            newly_opened_inputs.push(SoftwareMixInput::new_direct(
                                new_idx,
                                t_backend,
                                PlatformStream::Pulse(stream),
                            ));
                            Ok(fmt)
                        }
                        (BackendId::PulseAudio, TargetKind::Process | TargetKind::Application) => {
                            let root_pid = target.pid().ok_or(CapabilityError::InvalidTarget)?;
                            let root_id = ProcessIdentity {
                                pid: root_pid,
                                generation: target.process_start_time().unwrap_or(0),
                            };
                            let exact_only = target.scope() == ProcessScope::Exact;
                            let mut matched_any = false;
                            for si in &sink_inputs {
                                if let Some(si_pid) = si.pid
                                    && pid_belongs_to_target(
                                        &proc_map, root_id, exact_only, si_pid,
                                    )?
                                {
                                    let stream = open_pulse_point(
                                        &self.paths,
                                        None,
                                        Some(si.index),
                                        self.policy,
                                        self.timing,
                                        multi.storage_budget(),
                                        false,
                                    )?;
                                    newly_opened_si.insert((new_idx, si.index));
                                    newly_opened_inputs.push(
                                        SoftwareMixInput::new_pulse_substream(
                                            new_idx,
                                            t_backend,
                                            PlatformStream::Pulse(stream),
                                        )?,
                                    );
                                    matched_any = true;
                                }
                            }
                            new_slot_active[new_idx] = matched_any;
                            Ok(fallback_fmt.clone())
                        }
                        _ => Err(CapabilityError::InvalidBackend),
                    }
                })();

                match open_res {
                    Ok(fmt) => new_per_input_formats.push((t_backend, fmt)),
                    Err(err) => {
                        for (_, topo) in newly_opened_topos {
                            topo.shutdown();
                        }
                        for inp in newly_opened_inputs {
                            inp.stream.stop(Stop::Abort);
                            let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                        }
                        return Err(err);
                    }
                }
            }

            let mut format_promoted = false;
            let resolved_slot_map = match multi.reconfigure_targets_with_formats(
                new_targets.clone(),
                new_per_input_formats.clone(),
            ) {
                Ok(mapping) => mapping,
                Err(ManagerError::WorkingFormatChanged) => {
                    if self.request.on_format_change() == OnFormatChange::Fail {
                        for (_, topo) in newly_opened_topos {
                            topo.shutdown();
                        }
                        for inp in newly_opened_inputs {
                            inp.stream.stop(Stop::Abort);
                            let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                        }
                        return Err(CapabilityError::FormatChangeRejected);
                    }
                    let rebuilt_req = self.request.clone().with_targets(new_targets.clone());
                    match build_software_mix_multi(
                        rebuilt_req,
                        new_per_input_formats,
                        &new_slot_active,
                        self.budget.clone(),
                    ) {
                        Ok((new_bf, new_multi)) => {
                            *block_frames = new_bf;
                            *multi = new_multi;
                            self.session_counter = self.session_counter.saturating_add(1);
                            format_promoted = true;
                            planned_old_slot
                        }
                        Err(err) => {
                            for (_, topo) in newly_opened_topos {
                                topo.shutdown();
                            }
                            for inp in newly_opened_inputs {
                                inp.stream.stop(Stop::Abort);
                                let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                            }
                            return Err(err);
                        }
                    }
                }
                Err(err) => {
                    for (_, topo) in newly_opened_topos {
                        topo.shutdown();
                    }
                    for inp in newly_opened_inputs {
                        inp.stream.stop(Stop::Abort);
                        let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                    }
                    return Err(CapabilityError::from(err));
                }
            };

            let mut old_to_new: BTreeMap<usize, usize> = BTreeMap::new();
            for (new_idx, mapped_old) in resolved_slot_map.iter().enumerate() {
                if let Some(old_idx) = *mapped_old {
                    old_to_new.insert(old_idx, new_idx);
                }
            }

            let mut next_inputs = Vec::new();
            for mut inp in inputs.drain(..) {
                if let Some(&new_idx) = old_to_new.get(&inp.target_idx) {
                    inp.target_idx = new_idx;
                    next_inputs.push(inp);
                } else {
                    inp.stream.stop(Stop::Drain);
                    let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                }
            }
            let ready_now = Instant::now();
            for inp in &mut newly_opened_inputs {
                inp.last_active = Some(ready_now);
            }
            next_inputs.extend(newly_opened_inputs);
            let max_in_stride = next_inputs
                .iter()
                .map(|inp| inp.stream.format().bytes_per_frame())
                .max()
                .unwrap_or(8)
                .max(8);
            if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * max_in_stride {
                in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * max_in_stride, 0);
            }
            *inputs = next_inputs;

            if any_new_pa_proc {
                if pa_sub_watch.is_none() {
                    *pa_sub_watch = Some(PaSubWatch::spawn(&self.paths));
                }
            } else if let Some(mut watch) = pa_sub_watch.take() {
                watch.stop_and_join();
            }

            let mut next_topos = Vec::new();
            for (old_idx, topo) in sub_topologies.drain(..) {
                if let Some(&new_idx) = old_to_new.get(&old_idx) {
                    next_topos.push((new_idx, topo));
                } else {
                    topo.shutdown();
                }
            }
            next_topos.extend(newly_opened_topos);
            *sub_topologies = next_topos;

            let mut next_sink_inputs = BTreeSet::new();
            for &(old_idx, si_idx) in opened_sink_inputs.iter() {
                if let Some(&new_idx) = old_to_new.get(&old_idx) {
                    next_sink_inputs.insert((new_idx, si_idx));
                }
            }
            next_sink_inputs.extend(newly_opened_si);
            *opened_sink_inputs = next_sink_inputs;

            for (new_idx, &is_act) in new_slot_active.iter().enumerate() {
                let st = if is_act {
                    ParticipationState::Active
                } else {
                    ParticipationState::Idle
                };
                let _ = multi.set_input_state(new_idx, st);
            }

            let next_out_fmt = multi.output_format().clone();
            let next_req = self.request.clone().with_targets(new_targets);
            self.plan =
                resolve_linux_plan_with_readiness(&next_req, self.timing, pw_ready, pa_ready)
                    .unwrap_or(new_plan);
            self.request = next_req;
            self.refresh_default_observers();
            self.segment_counter = self.segment_counter.saturating_add(1);
            if format_promoted {
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::FormatChanged,
                    format: next_out_fmt,
                    device_id: None,
                    identity: None,
                });
            }
            return Ok(self.segment_counter);
        }

        let candidate_req = self.request.clone().with_targets(new_targets);
        let mut replacement = Self::open_impl(
            candidate_req,
            self.paths.clone(),
            self.policy,
            self.timing,
            false,
        )?;
        std::mem::swap(&mut self.engine, &mut replacement.engine);
        replacement.stop(Stop::Drain);
        let _ = replacement.wait_cleanup(self.cleanup_timeout);
        self.request = replacement.request.clone();
        self.plan = replacement.plan.clone();
        self.pulse_server_kind = replacement.pulse_server_kind;
        self.default_observer = replacement.default_observer.take();
        self.opened_default_device = replacement.opened_default_device.take();
        self.default_source_observer = replacement.default_source_observer.take();
        self.opened_default_source = replacement.opened_default_source.take();
        self.session_counter = self.session_counter.saturating_add(1);
        self.segment_counter = self.segment_counter.saturating_add(1);
        if let PlatformExecutionEngine::PassThrough {
            stream: Some(st), ..
        } = &self.engine
        {
            self.pending_boundary = Some(SessionBoundary {
                session: self.session_counter,
                segment: self.segment_counter,
                kind: BoundaryKind::ConfigurationChanged,
                format: st.format().clone(),
                device_id: self
                    .request
                    .targets()
                    .first()
                    .and_then(|t| t.device_id().cloned()),
                identity: self
                    .request
                    .targets()
                    .first()
                    .and_then(CaptureTarget::process_identity),
            });
        }
        Ok(self.segment_counter)
    }

    fn notify_default_flow_changed(
        &mut self,
        expected_kind: TargetKind,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        if self.stopped || self.terminal_error.is_some() {
            return Err(CapabilityError::Stopped);
        }
        let is_input = expected_kind == TargetKind::DefaultInput;
        let follow_idx = self
            .request
            .targets()
            .iter()
            .position(|t| {
                t.kind() == expected_kind && t.default_device_mode() == DefaultDeviceMode::Follow
            })
            .ok_or(CapabilityError::InvalidTarget)?;
        let primary_backend = self.plan.target_plans[follow_idx].resolved_backend;

        let resolved_id = match new_device_id {
            Some(id) => id.to_owned(),
            None if is_input => query_linux_default_source_now(primary_backend, &self.paths)
                .ok_or(CapabilityError::ServerUnavailable)?,
            None => query_linux_default_sink_now(primary_backend, &self.paths)
                .ok_or(CapabilityError::ServerUnavailable)?,
        };

        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                if self.request.targets().len() != 1
                    || self.request.targets()[0].kind() != expected_kind
                {
                    return Err(CapabilityError::InvalidTarget);
                }
                let t_backend = self.plan.target_plans[0].resolved_backend;
                let old_pcm = stream.as_ref().map(|s| s.format().clone());
                if let Some(old) = stream.take() {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }

                let next_stream = match (t_backend, is_input) {
                    (BackendId::PipeWire, false) => PlatformStream::PipeWire(open_pipewire_point(
                        &self.paths,
                        Some(&resolved_id),
                        true,
                        self.policy,
                        self.timing,
                        &self.budget,
                        false,
                    )?),
                    (BackendId::PipeWire, true) => {
                        PlatformStream::PipeWire(open_pipewire_source_point(
                            &self.paths,
                            Some(&resolved_id),
                            None,
                            self.policy,
                            self.timing,
                            &self.budget,
                            false,
                        )?)
                    }
                    (BackendId::PulseAudio, false) => PlatformStream::Pulse(open_pulse_point(
                        &self.paths,
                        Some(&resolved_id),
                        None,
                        self.policy,
                        self.timing,
                        &self.budget,
                        false,
                    )?),
                    (BackendId::PulseAudio, true) => {
                        PlatformStream::Pulse(open_pulse_source_point(
                            &self.paths,
                            Some(&resolved_id),
                            None,
                            self.policy,
                            self.timing,
                            &self.budget,
                            false,
                        )?)
                    }
                    _ => return Err(CapabilityError::InvalidBackend),
                };

                if old_pcm
                    .as_ref()
                    .is_some_and(|prev| prev != next_stream.format())
                    && self.request.on_format_change() == OnFormatChange::Fail
                {
                    next_stream.stop(Stop::Abort);
                    let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(ManagerError::Unsupported);
                    return Err(CapabilityError::FormatChangeRejected);
                }

                let next_fmt = next_stream.format().clone();
                *stream = Some(next_stream);
                if is_input {
                    self.opened_default_source = Some(resolved_id.clone());
                } else {
                    self.opened_default_device = Some(resolved_id.clone());
                }
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::DeviceChanged,
                    format: next_fmt,
                    device_id: Some(resolved_id.into()),
                    identity: None,
                });
                Ok(true)
            }
            PlatformExecutionEngine::SoftwareMix {
                block_frames,
                multi,
                inputs,
                in_bytes_scratch,
                ..
            } => {
                let mut switched = false;
                for inp in inputs.iter_mut() {
                    let Some(target) = self.request.targets().get(inp.target_idx) else {
                        continue;
                    };
                    if target.kind() != expected_kind
                        || target.default_device_mode() != DefaultDeviceMode::Follow
                    {
                        continue;
                    }
                    let old_pcm = inp.stream.format().clone();
                    inp.stream.stop(Stop::Drain);
                    let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                    let _ticket = multi
                        .acquire_pending_open()
                        .map_err(CapabilityError::from)?;
                    let next_stream = match (inp.backend, is_input) {
                        (BackendId::PipeWire, false) => {
                            PlatformStream::PipeWire(open_pipewire_point(
                                &self.paths,
                                Some(&resolved_id),
                                true,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?)
                        }
                        (BackendId::PipeWire, true) => {
                            PlatformStream::PipeWire(open_pipewire_source_point(
                                &self.paths,
                                Some(&resolved_id),
                                None,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?)
                        }
                        (BackendId::PulseAudio, false) => PlatformStream::Pulse(open_pulse_point(
                            &self.paths,
                            Some(&resolved_id),
                            None,
                            self.policy,
                            self.timing,
                            multi.storage_budget(),
                            false,
                        )?),
                        (BackendId::PulseAudio, true) => {
                            PlatformStream::Pulse(open_pulse_source_point(
                                &self.paths,
                                Some(&resolved_id),
                                None,
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?)
                        }
                        _ => return Err(CapabilityError::InvalidBackend),
                    };
                    if &old_pcm != next_stream.format()
                        && self.request.on_format_change() == OnFormatChange::Fail
                    {
                        next_stream.stop(Stop::Abort);
                        let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                        self.terminal_error = Some(ManagerError::Unsupported);
                        return Err(CapabilityError::FormatChangeRejected);
                    }
                    let stride = next_stream.format().bytes_per_frame();
                    if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * stride {
                        in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * stride, 0);
                    }
                    inp.stream = next_stream;
                    inp.last_active = Some(Instant::now());
                    inp.eof = false;
                    let _ = multi.set_input_state(inp.target_idx, ParticipationState::Active);
                    switched = true;
                }
                if switched {
                    let fallback_fmt = multi.working_format().clone();
                    let per_input: Vec<(BackendId, Arc<Format>)> =
                        (0..self.request.targets().len())
                            .map(|idx| {
                                let b = self.plan.target_plans[idx].resolved_backend;
                                let fmt = inputs
                                    .iter()
                                    .find(|inp| inp.target_idx == idx)
                                    .map(|inp| inp.stream.format().clone())
                                    .unwrap_or_else(|| fallback_fmt.clone());
                                (b, fmt)
                            })
                            .collect();
                    match multi.reconfigure_targets_with_formats(
                        self.request.targets().to_vec(),
                        per_input.clone(),
                    ) {
                        Ok(_) => {}
                        Err(ManagerError::WorkingFormatChanged) => {
                            let active: Vec<bool> = (0..self.request.targets().len())
                                .map(|idx| {
                                    inputs.iter().any(|inp| inp.target_idx == idx && !inp.eof)
                                })
                                .collect();
                            let (new_bf, new_multi) = build_software_mix_multi(
                                self.request.clone(),
                                per_input,
                                &active,
                                self.budget.clone(),
                            )?;
                            *block_frames = new_bf;
                            *multi = new_multi;
                        }
                        Err(err) => {
                            self.terminal_error = Some(err);
                            return Err(CapabilityError::from(err));
                        }
                    }
                    if is_input {
                        self.opened_default_source = Some(resolved_id);
                    } else {
                        self.opened_default_device = Some(resolved_id);
                    }
                    self.segment_counter = self.segment_counter.saturating_add(1);
                }
                Ok(switched)
            }
        }
    }

    fn reopen_relaunched_target_impl(
        &mut self,
        target_index: usize,
        new_pid: u32,
    ) -> Result<(), CapabilityError> {
        if self.stopped || self.terminal_error.is_some() {
            return Err(CapabilityError::Stopped);
        }
        let target = self
            .request
            .targets()
            .get(target_index)
            .cloned()
            .ok_or(CapabilityError::InvalidTarget)?;
        let OnRootExit::Wait { .. } = target.on_root_exit() else {
            return Err(CapabilityError::InvalidTarget);
        };
        let old_id = target
            .process_identity()
            .ok_or(CapabilityError::InvalidTarget)?;
        let (cand_id, cand_exe) = inspect_process_executable(new_pid)?;
        if cand_id == old_id || cand_id.generation <= old_id.generation {
            return Err(CapabilityError::PidReused);
        }
        if let Some(expected_exe) = target.executable_path()
            && expected_exe.as_ref() != cand_exe.as_str()
        {
            return Err(CapabilityError::InvalidTarget);
        }
        let updated_target = target.with_process_identity(cand_id)?;
        let mut next_targets = self.request.targets().to_vec();
        next_targets[target_index] = updated_target;
        let _ = self.update_targets_impl(next_targets)?;
        Ok(())
    }

    fn poll_default_device_follow(&mut self) {
        if self.stopped
            || self.terminal_error.is_some()
            || self.pending_boundary.is_some()
            || self.last_default_check.elapsed() < Duration::from_millis(200)
        {
            return;
        }
        self.last_default_check = Instant::now();
        let next_sink = self
            .default_observer
            .as_ref()
            .and_then(|obs| obs.changed_from(self.opened_default_device.as_deref()));
        if let Some(current_default) = next_sink {
            let _ =
                self.notify_default_flow_changed(TargetKind::DefaultOutput, Some(&current_default));
        }
        let next_src = self
            .default_source_observer
            .as_ref()
            .and_then(|obs| obs.changed_from(self.opened_default_source.as_deref()));
        if let Some(current_src) = next_src {
            let _ = self.notify_default_flow_changed(TargetKind::DefaultInput, Some(&current_src));
        }
    }

