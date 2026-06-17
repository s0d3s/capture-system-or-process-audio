#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::{
    capabilities::resolve_macos_plan_with_readiness,
    core::{
        manager::{
            BoundaryKind, InputStatusHandle, ManagerError, MultiInputManager,
            NativeTreeMixTopology, SessionBoundary, StorageBudget, default_block_frames_for_rate,
        },
        ordering::{self, Failure, Overflow, Stop},
        planner::{CapturePlanReport, CaptureRequest},
        reads::{Cancellation, PartialReason, ReadError, ReadResult},
        routing::RouteRollbackGuard,
        schema::{
            BackendId, ByteOrder, CaptureTarget, Channel, CombineResolution, DefaultDeviceMode,
            Format, Layout, MembershipDescriptor, MixedBlockMetadata, OnFormatChange,
            ParticipationState, SampleType, TargetKind,
        },
        traits::{
            AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
            TimingRequirement, TopologyControllerBackend,
        },
    },
    coreaudio_stream::{
        MAX_FRAMES_PER_SLOT, PlatformTopologyController,
        open_coreaudio_input_point_with_auth_override, open_coreaudio_tap_point,
        reconcile_target_audio_objects, start_coreaudio_topology_controller,
    },
    discovery::{
        PlatformDefaultInputObserver, PlatformDefaultOutputObserver,
        query_macos_default_input_uid_now, query_macos_default_output_uid_now,
    },
    proc_info::{inspect_bsd_process, inspect_process_executable},
    sck_stream::{PlatformStream, open_sck_app_point},
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

struct MacMixInput {
    target_idx: usize,
    stream: PlatformStream,
    last_active: Option<Instant>,
    eof: bool,
}

#[allow(clippy::large_enum_variant)]
enum PlatformExecutionEngine {
    PassThrough {
        stream: Option<PlatformStream>,
        topology: Option<PlatformTopologyController>,
    },
    SoftwareMix {
        block_frames: usize,
        multi: MultiInputManager,
        inputs: Vec<MacMixInput>,
        sub_topologies: Vec<(usize, PlatformTopologyController)>,
        started_at: Instant,
        in_bytes_scratch: Vec<u8>,
        drained_blocks: u64,
    },
}

type MacReadinessTuple = (
    (u32, u32, u32),
    Result<(), CapabilityError>,
    Result<(), CapabilityError>,
    Result<(), CapabilityError>,
);

fn probe_macos_readiness() -> MacReadinessTuple {
    #[cfg(target_os = "macos")]
    {
        let os_ver = crate::native_bridge::os_version();
        let ca_tap = crate::native_bridge::list_coreaudio_output_devices().map(|_| ());
        let ca_in = crate::native_bridge::list_coreaudio_input_devices().map(|_| ());
        let sck = if crate::native_bridge::sck_preflight_granted() {
            Ok(())
        } else {
            Err(CapabilityError::PermissionDenied)
        };
        (os_ver, ca_tap, ca_in, sck)
    }
    #[cfg(not(target_os = "macos"))]
    {
        (
            (15, 7, 9),
            Err(CapabilityError::ServerUnavailable),
            Err(CapabilityError::ServerUnavailable),
            Err(CapabilityError::ServerUnavailable),
        )
    }
}

fn validate_macos_process_targets(targets: &[CaptureTarget]) -> Result<(), CapabilityError> {
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
            let rec = inspect_bsd_process(pid)?;
            if expected_gen != 0 && rec.start_time_us != expected_gen {
                return Err(CapabilityError::PidReused);
            }
        }
    }
    Ok(())
}

fn resolve_pinned_mac_target(
    target: &mut CaptureTarget,
    default_out_uid: Option<&str>,
    default_in_uid: Option<&str>,
) {
    *target = target.resolve_pinned_endpoint(default_out_uid, default_in_uid);
}

fn lower_target_for_backend(target: &CaptureTarget, backend: BackendId) -> CaptureTarget {
    if backend == BackendId::CoreAudio && target.kind() == TargetKind::Application {
        target.lower_to_process().unwrap_or_else(|_| target.clone())
    } else {
        target.clone()
    }
}

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

fn build_software_mix_multi(
    request: CaptureRequest,
    per_input_formats: Vec<(BackendId, Arc<Format>)>,
    slot_active: &[bool],
    budget: StorageBudget,
) -> Result<(usize, MultiInputManager), CapabilityError> {
    let fallback_fmt = fallback_working_format()?;
    let max_rate = per_input_formats
        .iter()
        .map(|(_, f)| f.rate())
        .max()
        .unwrap_or(fallback_fmt.rate());
    let init_block = default_block_frames_for_rate(max_rate, MAX_FRAMES_PER_SLOT);
    let mut multi = MultiInputManager::new_heterogeneous_with_budget(
        request.clone(),
        per_input_formats,
        init_block,
        MAX_FRAMES_PER_SLOT,
        budget,
    )
    .map_err(CapabilityError::from)?;
    let block_frames =
        default_block_frames_for_rate(multi.working_format().rate(), MAX_FRAMES_PER_SLOT);
    if block_frames != init_block {
        let per_in: Vec<(BackendId, Arc<Format>)> = multi
            .input_handles()
            .iter()
            .map(|h| (h.backend(), h.native_format().clone()))
            .collect();
        multi = MultiInputManager::new_heterogeneous_with_budget(
            request,
            per_in,
            block_frames,
            MAX_FRAMES_PER_SLOT,
            multi.storage_budget().clone(),
        )
        .map_err(CapabilityError::from)?;
    }
    for (idx, &active) in slot_active.iter().enumerate() {
        let st = if active {
            ParticipationState::Active
        } else {
            ParticipationState::Idle
        };
        let _ = multi.set_input_state(idx, st);
    }
    Ok((block_frames, multi))
}

#[allow(clippy::too_many_arguments)]
fn open_single_mac_target_stream(
    backend: BackendId,
    target: &CaptureTarget,
    default_out_uid: Option<&str>,
    default_in_uid: Option<&str>,
    auth_override: Option<i32>,
    allow_prompt: bool,
    max_native_graph_links: usize,
    overflow: Overflow,
    timing: TimingRequirement,
    budget: &StorageBudget,
    observed: bool,
) -> Result<(PlatformStream, Option<PlatformTopologyController>), CapabilityError> {
    let lowered = lower_target_for_backend(target, backend);
    match (backend, lowered.kind()) {
        (BackendId::CoreAudio, TargetKind::Device | TargetKind::DefaultOutput) => {
            let dev_uid = lowered
                .clean_device_id()
                .map(str::to_owned)
                .or_else(|| default_out_uid.map(str::to_owned));
            let stream = open_coreaudio_tap_point(
                Vec::new(),
                false,
                None,
                dev_uid,
                false,
                overflow,
                timing,
                budget,
                observed,
            )?;
            Ok((PlatformStream::CoreAudio(stream), None))
        }
        (BackendId::CoreAudio, TargetKind::InputDevice | TargetKind::DefaultInput) => {
            let src_uid = lowered
                .clean_device_id()
                .map(str::to_owned)
                .or_else(|| default_in_uid.map(str::to_owned));
            let stream = open_coreaudio_input_point_with_auth_override(
                src_uid,
                auth_override,
                allow_prompt,
                overflow,
                timing,
                budget,
                observed,
            )?;
            Ok((PlatformStream::CoreAudioInput(stream), None))
        }
        (BackendId::CoreAudio, TargetKind::Process | TargetKind::Application) => {
            let mut tmp_topo = NativeTreeMixTopology::new(max_native_graph_links.clamp(1, 256))
                .map_err(|_| CapabilityError::CapacityExceeded)?;
            let initial_objs =
                reconcile_target_audio_objects(std::slice::from_ref(&lowered), &mut tmp_topo)?;
            let root_id = lowered.process_identity();
            let stream = open_coreaudio_tap_point(
                initial_objs,
                true,
                root_id,
                None,
                false,
                overflow,
                timing,
                budget,
                observed,
            )?;
            let single_topo = match start_coreaudio_topology_controller(
                vec![lowered],
                max_native_graph_links,
                &stream,
            ) {
                Ok(t) => t,
                Err(err) => {
                    stream.stop(Stop::Abort);
                    let _ = stream.wait_cleanup(Duration::from_millis(500));
                    return Err(err);
                }
            };
            Ok((PlatformStream::CoreAudio(stream), Some(single_topo)))
        }
        (BackendId::ScreenCaptureKit, TargetKind::Application) => {
            let id = lowered
                .process_identity()
                .ok_or(CapabilityError::InvalidTarget)?;
            let stream = open_sck_app_point(id, false, overflow, timing, budget, observed)?;
            Ok((PlatformStream::ScreenCaptureKit(stream), None))
        }
        (BackendId::ScreenCaptureKit, _) => Err(CapabilityError::UnsupportedTargetKind),
        _ => Err(CapabilityError::InvalidBackend),
    }
}

pub struct PlatformManager {
    request: CaptureRequest,
    plan: CapturePlanReport,
    policy: Overflow,
    timing: TimingRequirement,
    budget: StorageBudget,
    cleanup_timeout: Duration,
    engine: PlatformExecutionEngine,
    route_guard: RouteRollbackGuard,
    default_cancel: Cancellation,
    last_metadata: Option<MixedBlockMetadata>,
    pending_boundary: Option<SessionBoundary>,
    opened_default_device: Option<String>,
    default_observer: Option<PlatformDefaultOutputObserver>,
    opened_default_input: Option<String>,
    default_input_observer: Option<PlatformDefaultInputObserver>,
    last_default_check: Instant,
    session_counter: u64,
    segment_counter: u64,
    stopped: bool,
    terminal_error: Option<CapabilityError>,
    read_lock: Mutex<()>,
}

impl PlatformManager {
    #[allow(clippy::too_many_arguments)]
    fn open_impl(
        request: CaptureRequest,
        policy: Overflow,
        timing: TimingRequirement,
        observed: bool,
        auth_override: Option<i32>,
        default_in_uid_override: Option<&str>,
    ) -> Result<Self, CapabilityError> {
        let cleanup_timeout = Duration::from_nanos(request.budgets().cleanup_timeout_ns);
        if cleanup_timeout.is_zero() {
            return Err(CapabilityError::InvalidTarget);
        }

        validate_macos_process_targets(request.targets())?;

        let allow_prompt = request.permission_policy().allows_prompt();
        if allow_prompt
            && auth_override.is_none()
            && let Ok(req_kinds) =
                request.required_permission_kinds_for_os(crate::core::schema::PlatformOs::MacOs)
            && req_kinds.contains(&crate::core::schema::PermissionKind::ScreenRecording)
        {
            crate::permissions::maybe_auto_request_sck_permission(request.permission_policy());
        }

        let (os_ver, ca_tap_ready, ca_in_ready, sck_ready) = probe_macos_readiness();
        let initial_default_out = query_macos_default_output_uid_now();
        let initial_default_in = default_in_uid_override
            .map(str::to_owned)
            .or_else(query_macos_default_input_uid_now);

        let mut resolved_targets = request.targets().to_vec();
        for target in &mut resolved_targets {
            resolve_pinned_mac_target(
                target,
                initial_default_out.as_deref(),
                initial_default_in.as_deref(),
            );
        }
        let final_request = request.with_targets(resolved_targets);

        let plan = resolve_macos_plan_with_readiness(
            &final_request,
            timing,
            os_ver,
            ca_tap_ready,
            ca_in_ready,
            sck_ready,
        )?;

        let budget = StorageBudget::new(
            final_request.budgets().max_active_streams.max(2),
            final_request.budgets().max_total_pcm_memory,
        )
        .map_err(|_| CapabilityError::CapacityExceeded)?;
        let route_guard = RouteRollbackGuard::new(final_request.allow_reroute());

        let engine = match plan.combine_resolution {
            CombineResolution::PassThrough(b) => {
                let (stream, topology) = open_single_mac_target_stream(
                    b,
                    &final_request.targets()[0],
                    initial_default_out.as_deref(),
                    initial_default_in.as_deref(),
                    auth_override,
                    allow_prompt,
                    final_request.budgets().max_native_graph_links,
                    policy,
                    timing,
                    &budget,
                    observed,
                )?;
                PlatformExecutionEngine::PassThrough {
                    stream: Some(stream),
                    topology,
                }
            }
            CombineResolution::SharedNativeGraphMix(BackendId::CoreAudio) => {
                let lowered_all: Vec<CaptureTarget> = final_request
                    .targets()
                    .iter()
                    .map(|t| lower_target_for_backend(t, BackendId::CoreAudio))
                    .collect();
                let mut tmp_topo = NativeTreeMixTopology::new(
                    final_request.budgets().max_native_graph_links.clamp(1, 256),
                )
                .map_err(|_| CapabilityError::CapacityExceeded)?;
                let initial_objs = reconcile_target_audio_objects(&lowered_all, &mut tmp_topo)?;
                let root_id = if lowered_all.len() == 1 {
                    lowered_all[0].process_identity()
                } else {
                    None
                };
                let stream = open_coreaudio_tap_point(
                    initial_objs,
                    true,
                    root_id,
                    None,
                    false,
                    policy,
                    timing,
                    &budget,
                    observed,
                )?;
                let topo = match start_coreaudio_topology_controller(
                    lowered_all,
                    final_request.budgets().max_native_graph_links,
                    &stream,
                ) {
                    Ok(t) => t,
                    Err(err) => {
                        stream.stop(Stop::Abort);
                        let _ = stream.wait_cleanup(cleanup_timeout);
                        return Err(err);
                    }
                };
                PlatformExecutionEngine::PassThrough {
                    stream: Some(PlatformStream::CoreAudio(stream)),
                    topology: Some(topo),
                }
            }
            CombineResolution::CspaMixer => {
                let mut inputs = Vec::with_capacity(plan.target_plans.len());
                let mut sub_topologies = Vec::new();
                let mut per_input_formats = Vec::with_capacity(plan.target_plans.len());
                let slot_active = vec![true; plan.target_plans.len()];

                let open_outcome: Result<(), CapabilityError> = (|| {
                    for (t_idx, item) in plan.target_plans.iter().enumerate() {
                        let (stream, maybe_topo) = open_single_mac_target_stream(
                            item.resolved_backend,
                            &final_request.targets()[t_idx],
                            initial_default_out.as_deref(),
                            initial_default_in.as_deref(),
                            auth_override,
                            allow_prompt,
                            final_request.budgets().max_native_graph_links,
                            policy,
                            timing,
                            &budget,
                            observed,
                        )?;
                        per_input_formats.push((item.resolved_backend, stream.format().clone()));
                        if let Some(topo) = maybe_topo {
                            sub_topologies.push((t_idx, topo));
                        }
                        inputs.push(MacMixInput {
                            target_idx: t_idx,
                            stream,
                            last_active: Some(Instant::now()),
                            eof: false,
                        });
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
                    &slot_active,
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

                let max_in_stride = inputs
                    .iter()
                    .map(|inp| inp.stream.format().bytes_per_frame())
                    .max()
                    .unwrap_or(8)
                    .max(8);

                PlatformExecutionEngine::SoftwareMix {
                    block_frames,
                    multi,
                    inputs,
                    sub_topologies,
                    started_at: Instant::now(),
                    in_bytes_scratch: vec![0u8; MAX_FRAMES_PER_SLOT * max_in_stride],
                    drained_blocks: 0,
                }
            }
            _ => return Err(CapabilityError::NotImplemented),
        };

        let has_follow_out = final_request.targets().iter().any(|t| {
            t.kind() == TargetKind::DefaultOutput
                && t.default_device_mode() == DefaultDeviceMode::Follow
        });
        let has_follow_in = default_in_uid_override.is_none()
            && final_request.targets().iter().any(|t| {
                t.kind() == TargetKind::DefaultInput
                    && t.default_device_mode() == DefaultDeviceMode::Follow
            });
        let default_observer = has_follow_out.then(PlatformDefaultOutputObserver::acquire);
        let default_input_observer = has_follow_in.then(PlatformDefaultInputObserver::acquire);

        Ok(Self {
            request: final_request,
            plan,
            policy,
            timing,
            budget,
            cleanup_timeout,
            engine,
            route_guard,
            default_cancel: Cancellation::default(),
            last_metadata: None,
            pending_boundary: None,
            opened_default_device: initial_default_out,
            default_observer,
            opened_default_input: initial_default_in,
            default_input_observer,
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

    pub fn route_guard_mut(&mut self) -> &mut RouteRollbackGuard {
        &mut self.route_guard
    }

    pub fn topology_controller(&self) -> Option<&PlatformTopologyController> {
        match &self.engine {
            PlatformExecutionEngine::PassThrough { topology, .. } => topology.as_ref(),
            PlatformExecutionEngine::SoftwareMix { .. } => None,
        }
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn open_observed(request: CaptureRequest, observed: bool) -> Result<Self, CapabilityError> {
        Self::open_impl(
            request,
            Overflow::DropOldest,
            TimingRequirement::Raw,
            observed,
            None,
            None,
        )
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn open_with_auth_override(
        request: CaptureRequest,
        observed: bool,
        auth_override: Option<i32>,
    ) -> Result<Self, CapabilityError> {
        Self::open_impl(
            request,
            Overflow::DropOldest,
            TimingRequirement::Raw,
            observed,
            auth_override,
            None,
        )
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn open_with_default_input_override(
        request: CaptureRequest,
        default_in_uid_override: Option<&str>,
    ) -> Result<Self, CapabilityError> {
        Self::open_impl(
            request,
            Overflow::DropOldest,
            TimingRequirement::Raw,
            false,
            None,
            default_in_uid_override,
        )
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn from_synthetic_streams(
        request: CaptureRequest,
        streams: Vec<(BackendId, PlatformStream)>,
        shared_budget: StorageBudget,
    ) -> Result<Self, CapabilityError> {
        if streams.is_empty() || streams.len() != request.targets().len() {
            return Err(CapabilityError::InvalidTarget);
        }
        let plan = resolve_macos_plan_with_readiness(
            &request,
            TimingRequirement::Raw,
            (15, 7, 9),
            Ok(()),
            Ok(()),
            Ok(()),
        )?;
        let cleanup_timeout = Duration::from_nanos(request.budgets().cleanup_timeout_ns);
        let route_guard = RouteRollbackGuard::new(request.allow_reroute());

        let engine = if matches!(plan.combine_resolution, CombineResolution::PassThrough(_)) {
            let mut it = streams.into_iter();
            let (_, s) = it.next().unwrap();
            PlatformExecutionEngine::PassThrough {
                stream: Some(s),
                topology: None,
            }
        } else {
            let mut inputs = Vec::with_capacity(streams.len());
            let mut per_input = Vec::with_capacity(streams.len());
            let slot_active = vec![true; streams.len()];
            for (idx, (b, s)) in streams.into_iter().enumerate() {
                per_input.push((b, s.format().clone()));
                inputs.push(MacMixInput {
                    target_idx: idx,
                    stream: s,
                    last_active: Some(Instant::now()),
                    eof: false,
                });
            }
            let (block_frames, multi) = build_software_mix_multi(
                request.clone(),
                per_input,
                &slot_active,
                shared_budget.clone(),
            )?;
            let max_in_stride = inputs
                .iter()
                .map(|inp| inp.stream.format().bytes_per_frame())
                .max()
                .unwrap_or(8)
                .max(8);
            PlatformExecutionEngine::SoftwareMix {
                block_frames,
                multi,
                inputs,
                sub_topologies: Vec::new(),
                started_at: Instant::now(),
                in_bytes_scratch: vec![0u8; MAX_FRAMES_PER_SLOT * max_in_stride],
                drained_blocks: 0,
            }
        };

        Ok(Self {
            request,
            plan,
            policy: Overflow::DropOldest,
            timing: TimingRequirement::Raw,
            budget: shared_budget,
            cleanup_timeout,
            engine,
            route_guard,
            default_cancel: Cancellation::default(),
            last_metadata: None,
            pending_boundary: None,
            opened_default_device: None,
            default_observer: None,
            opened_default_input: None,
            default_input_observer: None,
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
        let on_fmt = self.request.on_format_change();
        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                if self.request.targets().len() != 1
                    || self.request.targets()[0].kind() != expected_kind
                {
                    return Ok(false);
                }
                let prev_fmt = stream.as_ref().map(|s| s.format().clone());
                let next_fmt = next_stream.format().clone();
                if on_fmt == OnFormatChange::Fail
                    && let Some(prev) = prev_fmt
                    && prev != next_fmt
                {
                    next_stream.stop(Stop::Abort);
                    let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                if let Some(old) = stream.replace(next_stream) {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::DeviceChanged,
                    format: next_fmt,
                    device_id: Some("synthetic-default".into()),
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
                let Some(pos) = inputs.iter().position(|inp| {
                    self.request
                        .targets()
                        .get(inp.target_idx)
                        .is_some_and(|t| t.kind() == expected_kind)
                }) else {
                    return Ok(false);
                };
                let t_idx = inputs[pos].target_idx;
                let prev_fmt = inputs[pos].stream.format().clone();
                let next_fmt = next_stream.format().clone();
                if on_fmt == OnFormatChange::Fail && prev_fmt != next_fmt {
                    next_stream.stop(Stop::Abort);
                    let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                let old = std::mem::replace(&mut inputs[pos].stream, next_stream);
                old.stop(Stop::Drain);
                let _ = old.wait_cleanup(self.cleanup_timeout);
                inputs[pos].last_active = Some(Instant::now());
                inputs[pos].eof = false;

                let fallback_fmt = multi.working_format().clone();
                let per_input: Vec<(BackendId, Arc<Format>)> = (0..self.request.targets().len())
                    .map(|idx| {
                        let b = self.plan.target_plans[idx].resolved_backend;
                        let f = inputs
                            .iter()
                            .find(|i| i.target_idx == idx)
                            .map_or_else(|| fallback_fmt.clone(), |i| i.stream.format().clone());
                        (b, f)
                    })
                    .collect();
                match multi.reconfigure_targets_with_formats(
                    self.request.targets().to_vec(),
                    per_input.clone(),
                ) {
                    Ok(_) => {}
                    Err(ManagerError::WorkingFormatChanged) => {
                        let active_flags: Vec<bool> = (0..self.request.targets().len())
                            .map(|idx| inputs.iter().any(|i| i.target_idx == idx && !i.eof))
                            .collect();
                        let (new_bf, new_multi) = build_software_mix_multi(
                            self.request.clone(),
                            per_input,
                            &active_flags,
                            self.budget.clone(),
                        )?;
                        *block_frames = new_bf;
                        *multi = new_multi;
                    }
                    Err(e) => return Err(CapabilityError::from(e)),
                }
                let max_in_stride = inputs
                    .iter()
                    .map(|i| i.stream.format().bytes_per_frame())
                    .max()
                    .unwrap_or(8)
                    .max(8);
                if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * max_in_stride {
                    in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * max_in_stride, 0);
                }
                let _ = multi.set_input_state(t_idx, ParticipationState::Active);
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                Ok(true)
            }
        }
    }

    fn refresh_default_observers(&mut self) {
        let has_follow_out = self.request.targets().iter().any(|t| {
            t.kind() == TargetKind::DefaultOutput
                && t.default_device_mode() == DefaultDeviceMode::Follow
        });
        if has_follow_out {
            if self.default_observer.is_none() {
                self.default_observer = Some(PlatformDefaultOutputObserver::acquire());
            }
        } else {
            self.default_observer = None;
        }

        let has_follow_in = self.request.targets().iter().any(|t| {
            t.kind() == TargetKind::DefaultInput
                && t.default_device_mode() == DefaultDeviceMode::Follow
        });
        if has_follow_in {
            if self.default_input_observer.is_none() {
                self.default_input_observer = Some(PlatformDefaultInputObserver::acquire());
            }
        } else {
            self.default_input_observer = None;
        }
    }

    fn update_targets_impl(
        &mut self,
        mut new_targets: Vec<CaptureTarget>,
    ) -> Result<u64, CapabilityError> {
        if self.stopped {
            return Err(CapabilityError::Stopped);
        }
        validate_macos_process_targets(&new_targets)?;

        let (os_ver, ca_tap_ready, ca_in_ready, sck_ready) = probe_macos_readiness();
        let cur_out = self
            .opened_default_device
            .clone()
            .or_else(query_macos_default_output_uid_now);
        let cur_in = self
            .opened_default_input
            .clone()
            .or_else(query_macos_default_input_uid_now);
        for target in &mut new_targets {
            resolve_pinned_mac_target(target, cur_out.as_deref(), cur_in.as_deref());
        }

        let candidate_req = self.request.clone().with_targets(new_targets.clone());
        let new_plan = resolve_macos_plan_with_readiness(
            &candidate_req,
            self.timing,
            os_ver,
            ca_tap_ready,
            ca_in_ready,
            sck_ready,
        )?;

        let new_is_ca_native_mix = matches!(
            new_plan.combine_resolution,
            CombineResolution::SharedNativeGraphMix(BackendId::CoreAudio)
        ) || (matches!(
            new_plan.combine_resolution,
            CombineResolution::PassThrough(BackendId::CoreAudio)
        ) && matches!(
            new_targets[0].kind(),
            TargetKind::Process | TargetKind::Application
        ));

        if let PlatformExecutionEngine::PassThrough {
            stream: Some(PlatformStream::CoreAudio(ca_stream)),
            topology: Some(topo),
        } = &mut self.engine
            && ca_stream.end_reason().is_none()
            && new_is_ca_native_mix
            && self.request.output_converter().is_none()
        {
            let lowered_all: Vec<CaptureTarget> = new_targets
                .iter()
                .map(|t| lower_target_for_backend(t, BackendId::CoreAudio))
                .collect();
            let next_root = if lowered_all.len() == 1 {
                lowered_all[0].process_identity()
            } else {
                None
            };
            ca_stream.set_root_identity(next_root);
            topo.update_targets_with_stream(lowered_all, ca_stream)?;
            self.request = candidate_req;
            self.plan = new_plan;
            self.refresh_default_observers();
            self.segment_counter = self.segment_counter.saturating_add(1);
            return Ok(self.segment_counter);
        }

        if let PlatformExecutionEngine::SoftwareMix {
            block_frames,
            multi,
            inputs,
            sub_topologies,
            in_bytes_scratch,
            ..
        } = &mut self.engine
            && new_plan.combine_resolution == CombineResolution::CspaMixer
        {
            let old_targets = self.request.targets();
            let old_handles = multi.input_handles();
            let mut old_used = vec![false; old_targets.len()];
            let mut pre_mapping = Vec::with_capacity(new_targets.len());
            for (new_idx, nt) in new_targets.iter().enumerate() {
                let new_backend = new_plan.target_plans[new_idx].resolved_backend;
                let mut matched = None;
                for (old_idx, ot) in old_targets.iter().enumerate() {
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
                        && self.plan.target_plans[old_idx].resolved_backend == new_backend
                        && ot.id() == nt.id()
                        && ot.scope() == nt.scope()
                    {
                        old_used[old_idx] = true;
                        matched = Some(old_idx);
                        break;
                    }
                }
                pre_mapping.push(matched);
            }

            let mut newly_opened: BTreeMap<
                usize,
                (PlatformStream, Option<PlatformTopologyController>),
            > = BTreeMap::new();
            for (new_idx, item) in new_plan.target_plans.iter().enumerate() {
                if pre_mapping[new_idx].is_some() {
                    continue;
                }
                let _ticket = multi
                    .acquire_pending_open()
                    .map_err(CapabilityError::from)?;
                match open_single_mac_target_stream(
                    item.resolved_backend,
                    &new_targets[new_idx],
                    cur_out.as_deref(),
                    cur_in.as_deref(),
                    None,
                    self.request.permission_policy().allows_prompt(),
                    self.request.budgets().max_native_graph_links,
                    self.policy,
                    self.timing,
                    multi.storage_budget(),
                    false,
                ) {
                    Ok(pair) => {
                        newly_opened.insert(new_idx, pair);
                    }
                    Err(err) => {
                        for (_, (s, t)) in newly_opened {
                            if let Some(tp) = t {
                                tp.shutdown();
                            }
                            s.stop(Stop::Abort);
                            let _ = s.wait_cleanup(self.cleanup_timeout);
                        }
                        return Err(err);
                    }
                }
            }

            let fallback_fmt = multi.working_format().clone();
            let mut new_per_input_formats = Vec::with_capacity(new_targets.len());
            let mut new_slot_active = vec![false; new_targets.len()];
            for (new_idx, item) in new_plan.target_plans.iter().enumerate() {
                if let Some(old_idx) = pre_mapping[new_idx] {
                    let old_fmt = inputs
                        .iter()
                        .find(|i| i.target_idx == old_idx)
                        .map(|i| i.stream.format().clone())
                        .or_else(|| {
                            multi
                                .input_handles()
                                .get(old_idx)
                                .map(|h| h.native_format().clone())
                        })
                        .unwrap_or_else(|| fallback_fmt.clone());
                    new_per_input_formats.push((item.resolved_backend, old_fmt));
                    new_slot_active[new_idx] =
                        inputs.iter().any(|i| i.target_idx == old_idx && !i.eof);
                } else if let Some((s, _)) = newly_opened.get(&new_idx) {
                    new_per_input_formats.push((item.resolved_backend, s.format().clone()));
                    new_slot_active[new_idx] = true;
                } else {
                    new_per_input_formats.push((item.resolved_backend, fallback_fmt.clone()));
                }
            }

            let mut format_promoted = false;
            let resolved_slot_map = match multi.reconfigure_targets_with_formats(
                new_targets.clone(),
                new_per_input_formats.clone(),
            ) {
                Ok(m) => m,
                Err(ManagerError::WorkingFormatChanged) => {
                    if self.request.on_format_change() == OnFormatChange::Fail {
                        for (_, (s, t)) in newly_opened {
                            if let Some(tp) = t {
                                tp.shutdown();
                            }
                            s.stop(Stop::Abort);
                            let _ = s.wait_cleanup(self.cleanup_timeout);
                        }
                        return Err(CapabilityError::FormatChangeRejected);
                    }
                    match build_software_mix_multi(
                        candidate_req.clone(),
                        new_per_input_formats,
                        &new_slot_active,
                        self.budget.clone(),
                    ) {
                        Ok((new_bf, new_multi)) => {
                            *block_frames = new_bf;
                            *multi = new_multi;
                            self.session_counter = self.session_counter.saturating_add(1);
                            format_promoted = true;
                            pre_mapping
                        }
                        Err(err) => {
                            for (_, (s, t)) in newly_opened {
                                if let Some(tp) = t {
                                    tp.shutdown();
                                }
                                s.stop(Stop::Abort);
                                let _ = s.wait_cleanup(self.cleanup_timeout);
                            }
                            return Err(err);
                        }
                    }
                }
                Err(err) => {
                    for (_, (s, t)) in newly_opened {
                        if let Some(tp) = t {
                            tp.shutdown();
                        }
                        s.stop(Stop::Abort);
                        let _ = s.wait_cleanup(self.cleanup_timeout);
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

            let mut next_inputs = Vec::with_capacity(new_targets.len());
            for mut inp in inputs.drain(..) {
                if let Some(&new_idx) = old_to_new.get(&inp.target_idx) {
                    inp.target_idx = new_idx;
                    next_inputs.push(inp);
                } else {
                    inp.stream.stop(Stop::Drain);
                    let _ = inp.stream.wait_cleanup(self.cleanup_timeout);
                }
            }

            let mut next_topos = Vec::new();
            for (old_idx, topo) in sub_topologies.drain(..) {
                if let Some(&new_idx) = old_to_new.get(&old_idx) {
                    next_topos.push((new_idx, topo));
                } else {
                    topo.shutdown();
                }
            }

            for (new_idx, (stream, maybe_topo)) in newly_opened {
                if let Some(tp) = maybe_topo {
                    next_topos.push((new_idx, tp));
                }
                next_inputs.push(MacMixInput {
                    target_idx: new_idx,
                    stream,
                    last_active: Some(Instant::now()),
                    eof: false,
                });
            }

            for (new_idx, &is_act) in new_slot_active.iter().enumerate() {
                let st = if is_act {
                    ParticipationState::Active
                } else {
                    ParticipationState::Idle
                };
                let _ = multi.set_input_state(new_idx, st);
            }

            let max_in_stride = next_inputs
                .iter()
                .map(|inp| inp.stream.format().bytes_per_frame())
                .max()
                .unwrap_or(8)
                .max(8);
            if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * max_in_stride {
                in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * max_in_stride, 0);
            }
            multi.clear_pending_output();
            *inputs = next_inputs;
            *sub_topologies = next_topos;
            let next_out_fmt = multi.output_format().clone();
            self.request = candidate_req;
            self.plan = new_plan;
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

        let mut replacement =
            Self::open_impl(candidate_req, self.policy, self.timing, false, None, None)?;
        std::mem::swap(&mut self.engine, &mut replacement.engine);
        replacement.stop(Stop::Drain);
        let _ = replacement.wait_cleanup(self.cleanup_timeout);
        self.request = replacement.request.clone();
        self.plan = replacement.plan.clone();
        self.default_observer = replacement.default_observer.take();
        self.opened_default_device = replacement.opened_default_device.take();
        self.default_input_observer = replacement.default_input_observer.take();
        self.opened_default_input = replacement.opened_default_input.take();
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

    fn reopen_relaunched_target_impl(
        &mut self,
        target_index: usize,
        new_pid: u32,
    ) -> Result<(), CapabilityError> {
        if self.stopped {
            return Err(CapabilityError::Stopped);
        }
        let Some(target) = self.request.targets().get(target_index) else {
            return Err(CapabilityError::InvalidTarget);
        };
        if !matches!(target.kind(), TargetKind::Process | TargetKind::Application) {
            return Err(CapabilityError::InvalidTarget);
        }
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
        let updated_target = target.clone().with_process_identity(cand_id)?;
        let mut next_targets = self.request.targets().to_vec();
        next_targets[target_index] = updated_target;
        let _ = self.update_targets_impl(next_targets)?;
        Ok(())
    }

    fn notify_default_flow_changed(
        &mut self,
        expected_kind: TargetKind,
        new_endpoint_opt: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        if self.stopped {
            return Err(CapabilityError::Stopped);
        }
        let is_input = expected_kind == TargetKind::DefaultInput;
        let has_follow = self.request.targets().iter().any(|t| {
            t.kind() == expected_kind && t.default_device_mode() == DefaultDeviceMode::Follow
        });
        if !has_follow {
            return Ok(false);
        }

        let resolved_uid = match new_endpoint_opt {
            Some(explicit) if !explicit.is_empty() => explicit.to_owned(),
            _ => {
                if is_input {
                    query_macos_default_input_uid_now().ok_or(CapabilityError::DeviceUnavailable)?
                } else {
                    query_macos_default_output_uid_now()
                        .ok_or(CapabilityError::DeviceUnavailable)?
                }
            }
        };

        let on_fmt = self.request.on_format_change();
        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                if self.request.targets().len() != 1
                    || self.request.targets()[0].kind() != expected_kind
                {
                    return Ok(false);
                }
                let prev_fmt = stream.as_ref().map(|s| s.format().clone());
                if let Some(old) = stream.take() {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }

                let next_stream = if is_input {
                    PlatformStream::CoreAudioInput(open_coreaudio_input_point_with_auth_override(
                        Some(resolved_uid.clone()),
                        None,
                        self.request.permission_policy().allows_prompt(),
                        self.policy,
                        self.timing,
                        &self.budget,
                        false,
                    )?)
                } else {
                    PlatformStream::CoreAudio(open_coreaudio_tap_point(
                        Vec::new(),
                        false,
                        None,
                        Some(resolved_uid.clone()),
                        false,
                        self.policy,
                        self.timing,
                        &self.budget,
                        false,
                    )?)
                };

                let next_format = next_stream.format().clone();
                if on_fmt == OnFormatChange::Fail
                    && let Some(prev) = prev_fmt
                    && prev != next_format
                {
                    next_stream.stop(Stop::Abort);
                    let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(CapabilityError::FormatChangeRejected);
                    return Err(CapabilityError::FormatChangeRejected);
                }

                *stream = Some(next_stream);
                if is_input {
                    self.opened_default_input = Some(resolved_uid.clone());
                } else {
                    self.opened_default_device = Some(resolved_uid.clone());
                }
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::DeviceChanged,
                    format: next_format,
                    device_id: Some(resolved_uid.into()),
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
                let mut format_changed = false;
                for inp in inputs.iter_mut() {
                    let Some(target) = self.request.targets().get(inp.target_idx) else {
                        continue;
                    };
                    if target.kind() != expected_kind
                        || target.default_device_mode() != DefaultDeviceMode::Follow
                    {
                        continue;
                    }
                    let old_fmt = inp.stream.format().clone();
                    inp.stream.stop(Stop::Drain);
                    let _ = inp.stream.wait_cleanup(self.cleanup_timeout);

                    let next_stream = if is_input {
                        PlatformStream::CoreAudioInput(
                            open_coreaudio_input_point_with_auth_override(
                                Some(resolved_uid.clone()),
                                None,
                                self.request.permission_policy().allows_prompt(),
                                self.policy,
                                self.timing,
                                multi.storage_budget(),
                                false,
                            )?,
                        )
                    } else {
                        PlatformStream::CoreAudio(open_coreaudio_tap_point(
                            Vec::new(),
                            false,
                            None,
                            Some(resolved_uid.clone()),
                            false,
                            self.policy,
                            self.timing,
                            multi.storage_budget(),
                            false,
                        )?)
                    };
                    if next_stream.format() != &old_fmt {
                        if on_fmt == OnFormatChange::Fail {
                            next_stream.stop(Stop::Abort);
                            let _ = next_stream.wait_cleanup(self.cleanup_timeout);
                            self.terminal_error = Some(CapabilityError::FormatChangeRejected);
                            return Err(CapabilityError::FormatChangeRejected);
                        }
                        format_changed = true;
                    }
                    inp.stream = next_stream;
                    inp.last_active = Some(Instant::now());
                    inp.eof = false;
                    let _ = multi.set_input_state(inp.target_idx, ParticipationState::Active);
                    switched = true;
                }
                if switched {
                    let mut format_promoted = false;
                    if format_changed {
                        let fallback_fmt = multi.working_format().clone();
                        let mut per_input = Vec::with_capacity(self.request.targets().len());
                        for t_idx in 0..self.request.targets().len() {
                            let b = self.plan.target_plans[t_idx].resolved_backend;
                            let f = inputs.iter().find(|i| i.target_idx == t_idx).map_or_else(
                                || fallback_fmt.clone(),
                                |i| i.stream.format().clone(),
                            );
                            per_input.push((b, f));
                        }
                        match multi.reconfigure_targets_with_formats(
                            self.request.targets().to_vec(),
                            per_input.clone(),
                        ) {
                            Ok(_) => {}
                            Err(ManagerError::WorkingFormatChanged) => {
                                let active_flags: Vec<bool> = (0..self.request.targets().len())
                                    .map(|idx| inputs.iter().any(|i| i.target_idx == idx && !i.eof))
                                    .collect();
                                let (new_bf, new_multi) = build_software_mix_multi(
                                    self.request.clone(),
                                    per_input,
                                    &active_flags,
                                    self.budget.clone(),
                                )?;
                                *block_frames = new_bf;
                                *multi = new_multi;
                                self.session_counter = self.session_counter.saturating_add(1);
                                format_promoted = true;
                            }
                            Err(e) => return Err(CapabilityError::from(e)),
                        }
                        let max_in_stride = inputs
                            .iter()
                            .map(|i| i.stream.format().bytes_per_frame())
                            .max()
                            .unwrap_or(8)
                            .max(8);
                        if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * max_in_stride {
                            in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * max_in_stride, 0);
                        }
                    }
                    if is_input {
                        self.opened_default_input = Some(resolved_uid);
                    } else {
                        self.opened_default_device = Some(resolved_uid);
                    }
                    self.segment_counter = self.segment_counter.saturating_add(1);
                    if format_promoted {
                        self.pending_boundary = Some(SessionBoundary {
                            session: self.session_counter,
                            segment: self.segment_counter,
                            kind: BoundaryKind::FormatChanged,
                            format: multi.output_format().clone(),
                            device_id: None,
                            identity: None,
                        });
                    }
                }
                Ok(switched)
            }
        }
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
        let next_out = self
            .default_observer
            .as_ref()
            .and_then(|obs| obs.changed_from(self.opened_default_device.as_deref()));
        if let Some(uid) = next_out {
            let _ = self.notify_default_flow_changed(TargetKind::DefaultOutput, Some(&uid));
        }
        let next_in = self
            .default_input_observer
            .as_ref()
            .and_then(|obs| obs.changed_from(self.opened_default_input.as_deref()));
        if let Some(uid) = next_in {
            let _ = self.notify_default_flow_changed(TargetKind::DefaultInput, Some(&uid));
        }
    }

    fn drain_software_mix_inputs(&mut self, timeout: Duration, cancel: &Cancellation) -> u64 {
        let req_targets_len = self.request.targets().len();
        let silence_on_idle = self.request.mixer_config().silence_on_idle;
        let PlatformExecutionEngine::SoftwareMix {
            block_frames,
            multi,
            inputs,
            started_at,
            in_bytes_scratch,
            drained_blocks,
            ..
        } = &mut self.engine
        else {
            return 0;
        };

