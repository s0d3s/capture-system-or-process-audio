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

    fn drain_software_mix_inputs(&mut self, timeout: Duration, cancel: &Cancellation) -> u64 {
        let req_ref = &self.request;
        let plan_ref = &self.plan;
        let paths_ref = &self.paths;
        let policy = self.policy;
        let timing = self.timing;
        let PlatformExecutionEngine::SoftwareMix {
            block_frames,
            multi,
            inputs,
            opened_sink_inputs,
            pa_sub_watch,
            last_sink_input_scan,
            started_at,
            in_bytes_scratch,
            pulse_sub_f32,
            pulse_sum_f32,
            pulse_sum_bytes,
            drained_blocks,
            ..
        } = &mut self.engine
        else {
            return 0;
        };

        for inp in inputs.iter() {
            inp.stream.arm();
        }

        let should_scan_pa = match pa_sub_watch.as_ref() {
            Some(watch) if watch.join.is_some() => watch.dirty.swap(false, SeqCst),
            Some(_) => last_sink_input_scan.elapsed() >= Duration::from_millis(150),
            None => false,
        };
        if should_scan_pa {
            *last_sink_input_scan = Instant::now();
            if let Ok((_devs, _pids, sink_inputs)) = collect_pulse_state(paths_ref)
                && let Ok(proc_map) = collect_proc_stat_table(Path::new("/proc"))
            {
                for (t_idx, target) in req_ref.targets().iter().enumerate() {
                    if plan_ref.target_plans[t_idx].resolved_backend == BackendId::PulseAudio
                        && matches!(target.kind(), TargetKind::Process | TargetKind::Application)
                        && let Some(root_pid) = target.pid()
                    {
                        let root_id = ProcessIdentity {
                            pid: root_pid,
                            generation: target.process_start_time().unwrap_or(0),
                        };
                        let exact_only = target.scope() == ProcessScope::Exact;
                        for si in &sink_inputs {
                            if !opened_sink_inputs.contains(&(t_idx, si.index))
                                && let Some(si_pid) = si.pid
                                && pid_belongs_to_target(&proc_map, root_id, exact_only, si_pid)
                                    == Ok(true)
                                && let Ok(stream) = open_pulse_point(
                                    paths_ref,
                                    None,
                                    Some(si.index),
                                    policy,
                                    timing,
                                    multi.storage_budget(),
                                    false,
                                )
                                && let Ok(inp) = SoftwareMixInput::new_pulse_substream(
                                    t_idx,
                                    BackendId::PulseAudio,
                                    PlatformStream::Pulse(stream),
                                )
                            {
                                opened_sink_inputs.insert((t_idx, si.index));
                                let _ = multi.set_input_state(t_idx, ParticipationState::Active);
                                inputs.push(inp);
                            }
                        }
                    }
                }
            }
        }

        let working_rate = u64::from(multi.working_format().rate().max(1));
        let mix_quantum = *block_frames;
        let align_wait = Duration::from_nanos(req_ref.mixer_config().alignment_window_ns)
            .min(Duration::from_millis(20));
        let call_started = Instant::now();

        loop {
            let any_recently_active = inputs.iter().any(|inp| {
                !inp.eof
                    && inp
                        .last_active
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
            });
            let mut waited_on_idle_leader = false;
            let mut pushed_any = false;

            for (t_idx, target) in req_ref.targets().iter().enumerate() {
                let already_queued = multi
                    .input_handles()
                    .get(t_idx)
                    .and_then(|h| h.snapshot())
                    .map_or(0, |s| s.queued_packets);
                if already_queued > 0 {
                    pushed_any = true;
                    continue;
                }

                let is_pa_proc_target = plan_ref.target_plans[t_idx].resolved_backend
                    == BackendId::PulseAudio
                    && matches!(target.kind(), TargetKind::Process | TargetKind::Application);

                if !is_pa_proc_target {
                    let Some(inp) = inputs
                        .iter_mut()
                        .find(|inp| inp.target_idx == t_idx && !inp.eof)
                    else {
                        let _ = multi.set_input_state(t_idx, ParticipationState::Idle);
                        continue;
                    };
                    let bpf = inp.stream.format().bytes_per_frame();
                    let stream_rate = u64::from(inp.stream.format().rate().max(1));
                    let is_resampled = stream_rate != working_rate;
                    let in_quantum = ((stream_rate * mix_quantum as u64).div_ceil(working_rate)
                        as usize)
                        .clamp(1, MAX_FRAMES_PER_SLOT);
                    let dst_bytes = in_quantum * bpf;
                    if in_bytes_scratch.len() < dst_bytes {
                        in_bytes_scratch.resize(dst_bytes, 0);
                    }
                    let max_passes = if is_resampled { 3 } else { 2 };

                    for pass in 0..max_passes {
                        let is_this_active = inp
                            .last_active
                            .is_some_and(|t| t.elapsed() < Duration::from_millis(250));
                        let stream_timeout = if pass > 0 {
                            timeout.min(Duration::from_millis(12))
                        } else if is_this_active {
                            timeout.min(Duration::from_millis(25))
                        } else if !any_recently_active && !pushed_any && !waited_on_idle_leader {
                            waited_on_idle_leader = true;
                            timeout.min(Duration::from_millis(10))
                        } else {
                            Duration::ZERO
                        };

                        let mut got_res = None;
                        for _attempt in 0..16 {
                            if let Ok(res) = inp.stream.read_into_with_cancel(
                                &mut in_bytes_scratch[..dst_bytes],
                                in_quantum,
                                stream_timeout,
                                cancel.clone(),
                            ) {
                                if res.frames > 0 || res.reason != PartialReason::Boundary {
                                    got_res = Some(res);
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                        if let Some(res) = got_res {
                            if res.frames > 0 {
                                inp.last_active = Some(Instant::now());
                                let now_ns = started_at.elapsed().as_nanos() as u64;
                                let stamp = res.first_packet.and_then(|a| a.header.timestamp);
                                let _ = multi.push_raw_input_bytes(
                                    t_idx,
                                    &in_bytes_scratch[..res.frames * bpf],
                                    stamp,
                                    now_ns,
                                );
                                if res.reason == PartialReason::Eof {
                                    inp.eof = true;
                                }
                                let now_queued = multi
                                    .input_handles()
                                    .get(t_idx)
                                    .and_then(|h| h.snapshot())
                                    .map_or(0, |s| s.queued_packets);
                                if now_queued > 0 {
                                    pushed_any = true;
                                    break;
                                }
                            } else {
                                if res.reason == PartialReason::Eof {
                                    inp.eof = true;
                                }
                                if pass == 0
                                    && (inp.eof
                                        || inp.last_active.is_none_or(|t| {
                                            t.elapsed() >= Duration::from_millis(500)
                                        }))
                                {
                                    let _ = multi.set_input_state(t_idx, ParticipationState::Idle);
                                }
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                } else {
                    let live_count = inputs
                        .iter()
                        .filter(|inp| {
                            inp.target_idx == t_idx
                                && (!inp.eof
                                    || inp
                                        .sub_reblocker
                                        .as_ref()
                                        .is_some_and(|r| r.buffered_frames() > 0))
                        })
                        .count();
                    if live_count == 0 {
                        let _ = multi.set_input_state(t_idx, ParticipationState::Idle);
                        continue;
                    }

                    for inp in inputs.iter_mut() {
                        if inp.target_idx != t_idx || inp.eof {
                            continue;
                        }
                        let buffered = inp
                            .sub_reblocker
                            .as_ref()
                            .map_or(0, FrameReblocker::buffered_frames);
                        if buffered >= PA_PROC_QUANTUM_FRAMES {
                            continue;
                        }
                        let is_this_active = inp
                            .last_active
                            .is_some_and(|t| t.elapsed() < Duration::from_millis(250));
                        let mut rem_wait = if is_this_active {
                            timeout.min(Duration::from_millis(25))
                        } else if !any_recently_active && !pushed_any && !waited_on_idle_leader {
                            waited_on_idle_leader = true;
                            timeout.min(Duration::from_millis(10))
                        } else {
                            Duration::ZERO
                        };

                        while !inp.eof {
                            let cur_buf = inp
                                .sub_reblocker
                                .as_ref()
                                .map_or(0, FrameReblocker::buffered_frames);
                            if cur_buf >= PA_PROC_QUANTUM_FRAMES {
                                break;
                            }
                            let need = PA_PROC_QUANTUM_FRAMES - cur_buf;
                            let step_start = Instant::now();
                            let mut got_res = None;
                            for _attempt in 0..16 {
                                if let Ok(res) = inp.stream.read_into_with_cancel(
                                    &mut in_bytes_scratch[..need * 8],
                                    need,
                                    rem_wait,
                                    cancel.clone(),
                                ) {
                                    if res.frames > 0 || res.reason != PartialReason::Boundary {
                                        got_res = Some(res);
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            rem_wait = rem_wait.saturating_sub(step_start.elapsed());
                            if let Some(res) = got_res {
                                if res.frames > 0 {
                                    inp.last_active = Some(Instant::now());
                                    if let Some(ts) =
                                        res.first_packet.and_then(|a| a.header.timestamp)
                                    {
                                        inp.last_stamp = Some(ts);
                                    }
                                    let samp_cnt = res.frames * 2;
                                    for (k, b) in in_bytes_scratch[..res.frames * 8]
                                        .as_chunks::<4>()
                                        .0
                                        .iter()
                                        .enumerate()
                                    {
                                        pulse_sub_f32[k] =
                                            f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                                    }
                                    if let Some(reblocker) = inp.sub_reblocker.as_mut() {
                                        let _ = reblocker.push_samples(&pulse_sub_f32[..samp_cnt]);
                                    }
                                    if res.reason == PartialReason::Eof {
                                        inp.eof = true;
                                        break;
                                    }
                                    if rem_wait.is_zero() {
                                        break;
                                    }
                                } else {
                                    if res.reason == PartialReason::Eof {
                                        inp.eof = true;
                                    }
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }

                    // Bound peer alignment when a substream has a full quantum.
                    let any_ready = inputs.iter().any(|inp| {
                        inp.target_idx == t_idx
                            && inp
                                .sub_reblocker
                                .as_ref()
                                .is_some_and(|r| r.buffered_frames() >= PA_PROC_QUANTUM_FRAMES)
                    });
                    if any_ready && live_count > 1 {
                        let align_started = Instant::now();
                        for inp in inputs.iter_mut() {
                            if inp.target_idx != t_idx || inp.eof {
                                continue;
                            }
                            let allow_bounded_wait = *drained_blocks == 0
                                || inp
                                    .last_active
                                    .is_none_or(|t| t.elapsed() < Duration::from_millis(500));
                            while !inp.eof {
                                let cur_buf = inp
                                    .sub_reblocker
                                    .as_ref()
                                    .map_or(0, FrameReblocker::buffered_frames);
                                if cur_buf >= PA_PROC_QUANTUM_FRAMES {
                                    break;
                                }
                                let rem = if allow_bounded_wait {
                                    align_wait.saturating_sub(align_started.elapsed())
                                } else {
                                    Duration::ZERO
                                };
                                let need = PA_PROC_QUANTUM_FRAMES - cur_buf;
                                let mut got_res = None;
                                for _attempt in 0..16 {
                                    if let Ok(res) = inp.stream.read_into_with_cancel(
                                        &mut in_bytes_scratch[..need * 8],
                                        need,
                                        rem,
                                        cancel.clone(),
                                    ) {
                                        if res.frames > 0 || res.reason != PartialReason::Boundary {
                                            got_res = Some(res);
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                                if let Some(res) = got_res {
                                    if res.frames > 0 {
                                        inp.last_active = Some(Instant::now());
                                        if let Some(ts) =
                                            res.first_packet.and_then(|a| a.header.timestamp)
                                        {
                                            inp.last_stamp = Some(ts);
                                        }
                                        let samp_cnt = res.frames * 2;
                                        for (k, b) in in_bytes_scratch[..res.frames * 8]
                                            .as_chunks::<4>()
                                            .0
                                            .iter()
                                            .enumerate()
                                        {
                                            pulse_sub_f32[k] =
                                                f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                                        }
                                        if let Some(reblocker) = inp.sub_reblocker.as_mut() {
                                            let _ =
                                                reblocker.push_samples(&pulse_sub_f32[..samp_cnt]);
                                        }
                                        if res.reason == PartialReason::Eof {
                                            inp.eof = true;
                                            break;
                                        }
                                        if rem.is_zero() {
                                            break;
                                        }
                                    } else {
                                        if res.reason == PartialReason::Eof {
                                            inp.eof = true;
                                        }
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                        }
                    }

                    let any_quantum_ready = inputs.iter().any(|inp| {
                        inp.target_idx == t_idx
                            && inp
                                .sub_reblocker
                                .as_ref()
                                .is_some_and(|r| r.buffered_frames() >= PA_PROC_QUANTUM_FRAMES)
                    });
                    let max_eof_tail = if !any_quantum_ready {
                        inputs
                            .iter()
                            .filter(|inp| inp.target_idx == t_idx && inp.eof)
                            .filter_map(|inp| {
                                inp.sub_reblocker.as_ref().map(|r| r.buffered_frames())
                            })
                            .max()
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let emit_frames = if any_quantum_ready {
                        PA_PROC_QUANTUM_FRAMES
                    } else {
                        max_eof_tail
                    };

                    if emit_frames > 0 {
                        let emit_samples = emit_frames * 2;
                        pulse_sum_f32[..emit_samples].fill(0.0);
                        let mut group_stamp = None;
                        for inp in inputs.iter_mut() {
                            if inp.target_idx != t_idx {
                                continue;
                            }
                            let Some(reblocker) = inp.sub_reblocker.as_mut() else {
                                continue;
                            };
                            if any_quantum_ready
                                && reblocker.buffered_frames() >= PA_PROC_QUANTUM_FRAMES
                            {
                                if reblocker
                                    .pop_block_into(&mut pulse_sub_f32[..emit_samples])
                                    .unwrap_or(false)
                                {
                                    if group_stamp.is_none() {
                                        group_stamp = inp.last_stamp;
                                    }
                                    for k in 0..emit_samples {
                                        let val = pulse_sub_f32[k];
                                        if val.is_finite() {
                                            pulse_sum_f32[k] += val;
                                        } else {
                                            pulse_sum_f32[k] = f32::NAN;
                                        }
                                    }
                                }
                            } else if inp.eof && reblocker.buffered_frames() > 0 {
                                let tail_frames = reblocker.buffered_frames().min(emit_frames);
                                let tail_samples = tail_frames * 2;
                                if reblocker
                                    .peek_into(0, &mut pulse_sub_f32[..tail_samples])
                                    .unwrap_or(false)
                                {
                                    reblocker.clear();
                                    if group_stamp.is_none() {
                                        group_stamp = inp.last_stamp;
                                    }
                                    for k in 0..tail_samples {
                                        let val = pulse_sub_f32[k];
                                        if val.is_finite() {
                                            pulse_sum_f32[k] += val;
                                        } else {
                                            pulse_sum_f32[k] = f32::NAN;
                                        }
                                    }
                                }
                            }
                        }

                        for (k, &s) in pulse_sum_f32[..emit_samples].iter().enumerate() {
                            pulse_sum_bytes[k * 4..k * 4 + 4].copy_from_slice(&s.to_le_bytes());
                        }
                        let now_ns = started_at.elapsed().as_nanos() as u64;
                        let _ = multi.push_raw_input_bytes(
                            t_idx,
                            &pulse_sum_bytes[..emit_samples * 4],
                            group_stamp,
                            now_ns,
                        );
                        *drained_blocks = drained_blocks.saturating_add(1);
                        pushed_any = true;
                    } else {
                        let all_idle =
                            inputs
                                .iter()
                                .filter(|inp| inp.target_idx == t_idx)
                                .all(|inp| {
                                    inp.eof
                                        || inp.last_active.is_none_or(|t| {
                                            t.elapsed() >= Duration::from_millis(500)
                                        })
                                });
                        if all_idle {
                            let _ = multi.set_input_state(t_idx, ParticipationState::Idle);
                        }
                    }
                }
            }

            if pushed_any
                || req_ref.mixer_config().silence_on_idle
                || timeout.is_zero()
                || call_started.elapsed() >= timeout
                || cancel.is_cancelled()
            {
                break;
            }
            thread::park_timeout(
                Duration::from_millis(5).min(timeout.saturating_sub(call_started.elapsed())),
            );
        }

        started_at.elapsed().as_nanos() as u64
    }
}

impl CaptureManagerBackend for PlatformManager {
    fn open_with_options(
        request: CaptureRequest,
        overflow: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError> {
        Self::open_impl(
            request,
            LinuxClientPaths::default(),
            overflow,
            timing,
            false,
        )
    }

    #[inline]
    fn plan(&self) -> &CapturePlanReport {
        &self.plan
    }

    fn selected_backend(&self) -> BackendId {
        self.plan.selected_backend.unwrap_or_else(|| {
            self.plan
                .target_plans
                .first()
                .map_or(BackendId::PipeWire, |p| p.resolved_backend)
        })
    }

    #[inline]
    fn is_software_mix(&self) -> bool {
        matches!(self.engine, PlatformExecutionEngine::SoftwareMix { .. })
    }

    fn working_format(&self) -> Option<&Arc<Format>> {
        match &self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                stream.as_ref().map(AudioStreamBackend::format)
            }
            PlatformExecutionEngine::SoftwareMix { multi, .. } => Some(multi.working_format()),
        }
    }

    fn output_format(&self) -> Option<&Arc<Format>> {
        match &self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                stream.as_ref().map(AudioStreamBackend::format)
            }
            PlatformExecutionEngine::SoftwareMix { multi, .. } => Some(multi.output_format()),
        }
    }

    #[inline]
    fn targets(&self) -> &[CaptureTarget] {
        self.request.targets()
    }

    fn input_handles(&self) -> &[InputStatusHandle] {
        match &self.engine {
            PlatformExecutionEngine::SoftwareMix { multi, .. } => multi.input_handles(),
            PlatformExecutionEngine::PassThrough { .. } => &[],
        }
    }

    fn membership_descriptor(&self) -> Option<MembershipDescriptor> {
        match &self.engine {
            PlatformExecutionEngine::SoftwareMix { multi, .. } => {
                Some(multi.membership_descriptor())
            }
            PlatformExecutionEngine::PassThrough { .. } => None,
        }
    }

    #[inline]
    fn last_block_metadata(&self) -> Option<MixedBlockMetadata> {
        self.last_metadata
    }

    #[inline]
    fn session_id(&self) -> u64 {
        self.session_counter
    }

    #[inline]
    fn segment_id(&self) -> u64 {
        self.segment_counter
    }

    #[inline]
    fn on_format_change(&self) -> OnFormatChange {
        self.request.on_format_change()
    }

    #[inline]
    fn route_guard(&self) -> Option<&RouteRollbackGuard> {
        Some(&self.route_guard)
    }

    fn update_targets(&mut self, new_targets: Vec<CaptureTarget>) -> Result<u64, CapabilityError> {
        self.update_targets_impl(new_targets)
    }

    fn notify_default_device_changed(
        &mut self,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        self.notify_default_flow_changed(TargetKind::DefaultOutput, new_device_id)
    }

    fn notify_default_input_changed(
        &mut self,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        self.notify_default_flow_changed(TargetKind::DefaultInput, new_device_id)
    }

    fn reopen_relaunched_target(
        &mut self,
        target_index: usize,
        new_pid: u32,
    ) -> Result<(), CapabilityError> {
        self.reopen_relaunched_target_impl(target_index, new_pid)
    }

    #[inline]
    fn default_cancel(&self) -> Cancellation {
        self.default_cancel.clone()
    }

    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        if frames == 0 {
            return Err(ReadError::InvalidRequest);
        }
        self.poll_default_device_follow();
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;

        if let Some(boundary) = self.pending_boundary.take() {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Boundary,
                control: Some(ordering::Control {
                    sequence: 0,
                    generation: boundary.session,
                    segment: boundary.segment,
                    format: boundary.format,
                    kind: ordering::ControlKind::Recovered,
                    losses: ordering::Losses::default(),
                    records: 1,
                }),
                copies: Default::default(),
            });
        }
        if self.stopped {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Eof,
                control: None,
                copies: Default::default(),
            });
        }
        if self.terminal_error.is_some() {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Failure(Failure::Backend),
                control: None,
                copies: Default::default(),
            });
        }
        if cancel.is_cancelled() {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Cancelled,
                control: None,
                copies: Default::default(),
            });
        }
        drop(_guard);

        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                let Some(active) = stream.as_mut() else {
                    return Ok(ReadResult {
                        frames: 0,
                        chunks: Vec::new(),
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Eof,
                        control: None,
                        copies: Default::default(),
                    });
                };
                let res = active.read_with_cancel(frames, timeout, cancel)?;
                if res.frames > 0 {
                    self.last_metadata = Some(MixedBlockMetadata {
                        membership_version: self.segment_counter,
                        contributing_mask: 1,
                        late_inputs: 0,
                        failed_inputs: 0,
                        nonfinite_inputs: 0,
                        synthesized_silence: false,
                        clipped_samples: 0,
                        nonfinite_samples: 0,
                        drift_slips: 0,
                        skew_estimate_ns: Some(0),
                        skew_uncertainty_ns: Some(0),
                    });
                }
                Ok(res)
            }
            PlatformExecutionEngine::SoftwareMix { multi, .. } => {
                if let Some((res, meta)) = multi.try_pop_pending_owned(frames)? {
                    self.last_metadata = Some(meta);
                    return Ok(res);
                }
                let now_ns = self.drain_software_mix_inputs(timeout, &cancel);
                if cancel.is_cancelled() {
                    return Ok(ReadResult {
                        frames: 0,
                        chunks: Vec::new(),
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Cancelled,
                        control: None,
                        copies: Default::default(),
                    });
                }
                let align_ns = self.request.mixer_config().alignment_window_ns;
                let PlatformExecutionEngine::SoftwareMix { multi, .. } = &self.engine else {
                    unreachable!()
                };
                let (res, meta) = multi.read_mixed_owned(frames, now_ns, align_ns)?;
                self.last_metadata = Some(meta);
                Ok(res)
            }
        }
    }

    fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        if frames == 0 {
            return Err(ReadError::InvalidRequest);
        }
        self.poll_default_device_follow();
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;

        if let Some(boundary) = self.pending_boundary.take() {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Boundary,
                control: Some(ordering::Control {
                    sequence: 0,
                    generation: boundary.session,
                    segment: boundary.segment,
                    format: boundary.format,
                    kind: ordering::ControlKind::Recovered,
                    losses: ordering::Losses::default(),
                    records: 1,
                }),
                copies: Default::default(),
            });
        }
        if self.stopped {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Eof,
                control: None,
                copies: Default::default(),
            });
        }
        if self.terminal_error.is_some() {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Failure(Failure::Backend),
                control: None,
                copies: Default::default(),
            });
        }
        if cancel.is_cancelled() {
            return Ok(ReadResult {
                frames: 0,
                chunks: Vec::new(),
                first_packet: None,
                last_packet: None,
                reason: PartialReason::Cancelled,
                control: None,
                copies: Default::default(),
            });
        }
        drop(_guard);

        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                let Some(active) = stream.as_mut() else {
                    return Ok(ReadResult {
                        frames: 0,
                        chunks: Vec::new(),
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Eof,
                        control: None,
                        copies: Default::default(),
                    });
                };
                let res = active.read_into_with_cancel(destination, frames, timeout, cancel)?;
                if res.frames > 0 {
                    self.last_metadata = Some(MixedBlockMetadata {
                        membership_version: self.segment_counter,
                        contributing_mask: 1,
                        late_inputs: 0,
                        failed_inputs: 0,
                        nonfinite_inputs: 0,
                        synthesized_silence: false,
                        clipped_samples: 0,
                        nonfinite_samples: 0,
                        drift_slips: 0,
                        skew_estimate_ns: Some(0),
                        skew_uncertainty_ns: Some(0),
                    });
                }
                Ok(res)
            }
            PlatformExecutionEngine::SoftwareMix { multi, .. } => {
                let out_stride = multi.output_format().bytes_per_frame();
                if destination.len() < frames.saturating_mul(out_stride) {
                    return Err(ReadError::InvalidDestination);
                }
                if let Some((res, meta)) = multi.try_pop_pending_into(destination, frames)? {
                    self.last_metadata = Some(meta);
                    return Ok(res);
                }
                let now_ns = self.drain_software_mix_inputs(timeout, &cancel);
                if cancel.is_cancelled() {
                    return Ok(ReadResult {
                        frames: 0,
                        chunks: Vec::new(),
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Cancelled,
                        control: None,
                        copies: Default::default(),
                    });
                }
                let align_ns = self.request.mixer_config().alignment_window_ns;
                let PlatformExecutionEngine::SoftwareMix { multi, .. } = &self.engine else {
                    unreachable!()
                };
                let (res, meta) = multi.read_mixed_into(destination, frames, now_ns, align_ns)?;
                self.last_metadata = Some(meta);
                Ok(res)
            }
        }
    }

    fn stop(&mut self, mode: Stop) {
        self.stopped = true;
        self.default_observer = None;
        self.default_source_observer = None;
        if mode == Stop::Abort {
            self.pending_boundary = None;
        }
        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, topology } => {
                if let Some(active) = stream.as_ref() {
                    active.stop(mode);
                }
                if let Some(topo) = topology {
                    topo.shutdown();
                }
            }
            PlatformExecutionEngine::SoftwareMix {
                multi,
                inputs,
                sub_topologies,
                pa_sub_watch,
                ..
            } => {
                if mode == Stop::Abort {
                    multi.clear_pending_output();
                }
                if let Some(mut watch) = pa_sub_watch.take() {
                    watch.stop_and_join();
                }
                for inp in inputs {
                    inp.stream.stop(mode);
                }
                for (_, topo) in sub_topologies {
                    topo.shutdown();
                }
            }
        }
    }

    fn wait_cleanup(&mut self, timeout: Duration) -> bool {
        let started = Instant::now();
        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, topology } => {
                if let Some(topo) = topology {
                    topo.shutdown();
                }
                if let Some(active) = stream.as_ref() {
                    let rem = timeout.saturating_sub(started.elapsed());
                    let _ = active.wait_cleanup(rem);
                }
            }
            PlatformExecutionEngine::SoftwareMix {
                inputs,
                sub_topologies,
                pa_sub_watch,
                ..
            } => {
                if let Some(mut watch) = pa_sub_watch.take() {
                    watch.stop_and_join();
                }
                for (_, topo) in sub_topologies {
                    topo.shutdown();
                }
                for inp in inputs {
                    let rem = timeout.saturating_sub(started.elapsed());
                    let _ = inp.stream.wait_cleanup(rem);
                }
            }
        }
        self.resources_done()
    }

    fn resources_done(&self) -> bool {
        match &self.engine {
            PlatformExecutionEngine::PassThrough { stream, topology } => {
                stream
                    .as_ref()
                    .is_none_or(AudioStreamBackend::resources_done)
                    && topology
                        .as_ref()
                        .is_none_or(PlatformTopologyController::is_finished)
            }
            PlatformExecutionEngine::SoftwareMix {
                inputs,
                sub_topologies,
                pa_sub_watch,
                ..
            } => {
                pa_sub_watch.is_none()
                    && inputs
                        .iter()
                        .all(|inp| AudioStreamBackend::resources_done(&inp.stream))
                    && sub_topologies.iter().all(|(_, topo)| topo.is_finished())
            }
        }
    }
}

impl Drop for PlatformManager {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
        let _ = self.wait_cleanup(self.cleanup_timeout.min(Duration::from_millis(500)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CaptureManager,
        core::schema::{CombineMode, InputConversionPolicy},
        pipewire_stream::PipeWireStream,
    };

    fn make_tone_packets_i16_mono(
        rate_hz: u32,
        freq: f64,
        packets: usize,
        frames_per_pkt: usize,
    ) -> Vec<(Vec<u8>, usize, u64)> {
        let mut out = Vec::with_capacity(packets);
        let step_ns = ((frames_per_pkt as u64) * 1_000_000_000) / u64::from(rate_hz);
        for p in 0..packets {
            let mut bytes = Vec::with_capacity(frames_per_pkt * 2);
            for n in 0..frames_per_pkt {
                let idx = p * frames_per_pkt + n;
                let t = idx as f64 / f64::from(rate_hz);
                let s =
                    (0.25 * (2.0 * std::f64::consts::PI * freq * t).sin() * 32767.0).round() as i16;
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            out.push((bytes, frames_per_pkt, 10_000_000 + (p as u64) * step_ns));
        }
        out
    }

    fn make_tone_packets_f32_stereo(
        rate_hz: u32,
        freq_l: f64,
        freq_r: f64,
        packets: usize,
        frames_per_pkt: usize,
    ) -> Vec<(Vec<u8>, usize, u64)> {
        let mut out = Vec::with_capacity(packets);
        let step_ns = ((frames_per_pkt as u64) * 1_000_000_000) / u64::from(rate_hz);
        for p in 0..packets {
            let mut bytes = Vec::with_capacity(frames_per_pkt * 8);
            for n in 0..frames_per_pkt {
                let idx = p * frames_per_pkt + n;
                let t = idx as f64 / f64::from(rate_hz);
                let l = (0.25 * (2.0 * std::f64::consts::PI * freq_l * t).sin()) as f32;
                let r = (0.25 * (2.0 * std::f64::consts::PI * freq_r * t).sin()) as f32;
                bytes.extend_from_slice(&l.to_le_bytes());
                bytes.extend_from_slice(&r.to_le_bytes());
            }
            out.push((bytes, frames_per_pkt, 10_000_000 + (p as u64) * step_ns));
        }
        out
    }

    fn read_into_via_generic_manager<B: CaptureManagerBackend>(
        mgr: &mut crate::core::traits::CaptureManager<B>,
        dst: &mut [u8],
        frames: usize,
        timeout: Duration,
    ) -> Result<ReadResult, ReadError> {
        mgr.read_into(dst, frames, timeout)
    }

    #[test]
    fn platform_manager_shared_out_pool_zero_steady_allocations_and_format_gates()
    -> Result<(), CapabilityError> {
        let fmt_16k = Arc::new(
            Format::new(
                SampleType::I16,
                ByteOrder::Little,
                16_000,
                1,
                Layout::Known(Arc::from([Channel::Mono])),
            )
            .unwrap(),
        );
        let fmt_48k = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .unwrap(),
        );

        let budget = StorageBudget::new(8, 32 * 1024 * 1024).unwrap();
        let sys_t = CaptureTarget::device("pw-sys-0")
            .unwrap()
            .with_backend(BackendId::PipeWire);
        let mic_t = CaptureTarget::input_device("pa-mic-0")
            .unwrap()
            .with_backend(BackendId::PulseAudio);
        let s_sys = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_48k.clone(),
            1,
            make_tone_packets_f32_stereo(48_000, 1000.0, 1500.0, 50, 480),
            &budget,
        )?);
        let s_mic = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_16k.clone(),
            5,
            make_tone_packets_i16_mono(16_000, 500.0, 50, 160),
            &budget,
        )?);
        let stream_only_bytes = budget.reserved_bytes();
        let stream_only_sessions = budget.resident_sessions();
        assert_eq!(stream_only_sessions, 2);

        let mut mgr = CaptureManager::from_backend(PlatformManager::from_synthetic_streams(
            CaptureRequest::new(vec![sys_t, mic_t]),
            vec![(BackendId::PipeWire, s_sys), (BackendId::PulseAudio, s_mic)],
            budget.clone(),
        )?);
        // The output pool charges the shared budget without consuming a stream session slot.
        assert_eq!(budget.resident_sessions(), 2);
        assert!(budget.reserved_bytes() > stream_only_bytes);
        assert!(mgr.is_software_mix());
        assert_eq!(mgr.working_format().unwrap().rate(), 48_000);
        assert!(mgr.route_guard().is_some_and(|g| !g.allow_reroute()));

        let mut dst = [0u8; 256 * 8];
        for _ in 0..2 {
            let _ =
                read_into_via_generic_manager(&mut mgr, &mut dst, 256, Duration::from_millis(40))
                    .unwrap();
        }
        let mut measured_reads = 0usize;
        let allocs = allocation_counter::measure(|| {
            for _ in 0..64 {
                let res = read_into_via_generic_manager(
                    &mut mgr,
                    &mut dst,
                    256,
                    Duration::from_millis(40),
                )
                .unwrap();
                if res.frames > 0 {
                    measured_reads += 1;
                }
            }
        });
        assert!(measured_reads >= 40);
        assert_eq!(
            allocs.count_total, 0,
            "CaptureManager<PlatformManager>::read_into allocated {} times in steady state",
            allocs.count_total
        );
        mgr.stop(Stop::Abort);
        assert!(mgr.wait_cleanup(Duration::from_secs(2)));

        let follow_in = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        let pt_s1 = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_16k.clone(),
            2,
            make_tone_packets_i16_mono(16_000, 440.0, 2, 160),
            &budget,
        )?);
        let pt_s2 = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_48k.clone(),
            3,
            make_tone_packets_f32_stereo(48_000, 440.0, 440.0, 2, 480),
            &budget,
        )?);
        let mut pt_fail = PlatformManager::from_synthetic_streams(
            CaptureRequest::single(follow_in.clone()).with_on_format_change(OnFormatChange::Fail),
            vec![(BackendId::PipeWire, pt_s1)],
            budget.clone(),
        )?;
        assert_eq!(
            pt_fail.apply_opened_default_stream_for_test(TargetKind::DefaultInput, pt_s2),
            Err(CapabilityError::FormatChangeRejected)
        );
        drop(pt_fail);

        let mix_s1 = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_16k.clone(),
            2,
            make_tone_packets_i16_mono(16_000, 440.0, 2, 160),
            &budget,
        )?);
        let mix_s2 = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_48k.clone(),
            3,
            make_tone_packets_f32_stereo(48_000, 440.0, 440.0, 2, 480),
            &budget,
        )?);
        let mut mix_fail = PlatformManager::from_synthetic_streams(
            CaptureRequest::single(follow_in.clone())
                .with_combine_mode(CombineMode::CspaMixer)
                .with_on_format_change(OnFormatChange::Fail),
            vec![(BackendId::PipeWire, mix_s1)],
            budget.clone(),
        )?;
        assert_eq!(
            mix_fail.apply_opened_default_stream_for_test(TargetKind::DefaultInput, mix_s2),
            Err(CapabilityError::FormatChangeRejected)
        );
        drop(mix_fail);

        let adapt_s1 = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_16k.clone(),
            2,
            make_tone_packets_i16_mono(16_000, 440.0, 4, 160),
            &budget,
        )?);
        let adapt_s2 = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_48k.clone(),
            3,
            make_tone_packets_f32_stereo(48_000, 440.0, 440.0, 4, 480),
            &budget,
        )?);
        let mut mix_adapt = PlatformManager::from_synthetic_streams(
            CaptureRequest::single(follow_in).with_combine_mode(CombineMode::CspaMixer),
            vec![(BackendId::PipeWire, adapt_s1)],
            budget.clone(),
        )?;
        assert_eq!(mix_adapt.working_format().unwrap().rate(), 16_000);
        assert_eq!(
            mix_adapt.apply_opened_default_stream_for_test(TargetKind::DefaultInput, adapt_s2),
            Ok(true)
        );
        assert_eq!(mix_adapt.working_format().unwrap().rate(), 48_000);
        drop(mix_adapt);

        let strict_a = CaptureTarget::input_device("strict-16k").unwrap();
        let strict_b = CaptureTarget::device("strict-48k").unwrap();
        let sa = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_16k,
            2,
            make_tone_packets_i16_mono(16_000, 440.0, 2, 160),
            &budget,
        )?);
        let sb = PlatformStream::PipeWire(PipeWireStream::from_synthetic_packets(
            fmt_48k,
            1,
            make_tone_packets_f32_stereo(48_000, 440.0, 440.0, 2, 480),
            &budget,
        )?);
        let strict_err = PlatformManager::from_synthetic_streams(
            CaptureRequest::new(vec![strict_a, strict_b])
                .with_input_conversion(InputConversionPolicy::StrictNoResample),
            vec![(BackendId::PipeWire, sa), (BackendId::PipeWire, sb)],
            budget,
        )
        .err();
        assert_eq!(strict_err, Some(CapabilityError::UnsupportedFormat));
        Ok(())
    }
}
