use crate::{
    capabilities::resolve_windows_plan,
    core::{
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
            ParticipationState, ProcessScope, SampleType, TargetKind,
        },
        stream_runtime::{EndReason, StartupCleanup},
        traits::{
            AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
            TimingRequirement,
        },
    },
    discovery::{
        PlatformDefaultInputObserver, PlatformDefaultOutputObserver,
        query_default_capture_endpoint_id, query_default_render_endpoint_id,
    },
    endpoint::{PlatformStream, WasapiOpenTarget},
    process::inspect_process_executable,
};
use std::{
    sync::{Arc, Mutex, TryLockError},
    thread,
    time::{Duration, Instant},
};

const MAX_FRAMES_PER_SLOT: usize = 4096;

enum PlatformExecutionEngine {
    PassThrough {
        stream: Option<Box<PlatformStream>>,
        waiting_relaunch: bool,
        wait_deadline: Option<Instant>,
    },
    SoftwareMix {
        block_frames: usize,
        multi: Box<MultiInputManager>,
        streams: Vec<Option<PlatformStream>>,
        last_active_at: Vec<Option<Instant>>,
        wait_deadlines: Vec<Option<Instant>>,
        started_at: Instant,
        in_bytes_scratch: Vec<u8>,
    },
}

pub struct PlatformManager {
    request: CaptureRequest,
    plan: CapturePlanReport,
    route_guard: RouteRollbackGuard,
    policy: Overflow,
    timing: TimingRequirement,
    cleanup_timeout: Duration,
    budget: StorageBudget,
    engine: PlatformExecutionEngine,
    default_cancel: Cancellation,
    pending_cleanup: Option<StartupCleanup>,
    pending_boundary: Option<SessionBoundary>,
    last_metadata: Option<MixedBlockMetadata>,
    default_observer: Option<PlatformDefaultOutputObserver>,
    default_input_observer: Option<PlatformDefaultInputObserver>,
    last_default_check: Instant,
    session_counter: u64,
    segment_counter: u64,
    stopped: bool,
    terminal_error: Option<ManagerError>,
    read_lock: Mutex<()>,
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

fn resolve_pinned_capture_target(target: &mut CaptureTarget, endpoint: &Arc<str>) {
    if target.default_device_mode() != DefaultDeviceMode::Pin {
        return;
    }
    *target = match target.kind() {
        TargetKind::DefaultOutput => target.resolve_pinned_endpoint(Some(endpoint.as_ref()), None),
        TargetKind::DefaultInput => target.resolve_pinned_endpoint(None, Some(endpoint.as_ref())),
        _ => return,
    };
}

fn map_manager_error(err: ManagerError) -> CapabilityError {
    match err {
        ManagerError::Capacity => CapabilityError::CapacityExceeded,
        ManagerError::CleanupTimeout => CapabilityError::CleanupTimeout,
        ManagerError::WorkingFormatChanged => CapabilityError::FormatChangeRejected,
        ManagerError::Unsupported | ManagerError::InvalidConfiguration => {
            CapabilityError::UnsupportedFormat
        }
        ManagerError::InvalidState => CapabilityError::Stopped,
        _ => CapabilityError::NotImplemented,
    }
}

fn build_software_mix_engine(
    request: CaptureRequest,
    streams: Vec<Option<PlatformStream>>,
    last_active_at: Vec<Option<Instant>>,
    wait_deadlines: Vec<Option<Instant>>,
    shared_budget: StorageBudget,
    cleanup_timeout: Duration,
) -> Result<PlatformExecutionEngine, CapabilityError> {
    let fallback_fmt = fallback_working_format()?;
    let per_input: Vec<(BackendId, Arc<Format>)> = streams
        .iter()
        .map(|s| {
            let fmt = s
                .as_ref()
                .map_or_else(|| fallback_fmt.clone(), |st| st.native_format.pcm.clone());
            (BackendId::Wasapi, fmt)
        })
        .collect();
    let max_rate = per_input
        .iter()
        .map(|(_, f)| f.rate())
        .max()
        .unwrap_or(48_000);
    let block_frames = default_block_frames_for_rate(max_rate, MAX_FRAMES_PER_SLOT);
    let max_in_stride = streams
        .iter()
        .flatten()
        .map(|s| s.native_format.pcm.bytes_per_frame())
        .max()
        .unwrap_or(8)
        .max(8);

    let multi = match MultiInputManager::new_heterogeneous_with_budget(
        request,
        per_input,
        block_frames,
        MAX_FRAMES_PER_SLOT,
        shared_budget,
    ) {
        Ok(m) => m,
        Err(err) => {
            for existing in streams.into_iter().flatten() {
                existing.stop(Stop::Abort);
                let _ = existing.wait_cleanup(cleanup_timeout);
            }
            return Err(map_manager_error(err));
        }
    };

    for (idx, s) in streams.iter().enumerate() {
        if s.is_some() {
            let _ = multi.set_input_state(idx, ParticipationState::Active);
        } else if wait_deadlines.get(idx).and_then(|d| *d).is_some() {
            let _ = multi.set_input_state(idx, ParticipationState::WaitingRelaunch);
        }
    }

    Ok(PlatformExecutionEngine::SoftwareMix {
        block_frames,
        multi: Box::new(multi),
        streams,
        last_active_at,
        wait_deadlines,
        started_at: Instant::now(),
        in_bytes_scratch: vec![0u8; MAX_FRAMES_PER_SLOT * max_in_stride],
    })
}

impl PlatformManager {
    fn open_impl(
        request: CaptureRequest,
        policy: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError> {
        let plan = resolve_windows_plan(&request, timing)?;
        for target in request.targets() {
            crate::permissions::ensure_windows_target_permission(target)?;
        }
        let cleanup_timeout = Duration::from_nanos(request.budgets().cleanup_timeout_ns);
        if cleanup_timeout < Duration::from_millis(50) || cleanup_timeout > Duration::from_secs(30)
        {
            return Err(CapabilityError::InvalidTarget);
        }

        let max_streams = if plan.combine_resolution == CombineResolution::CspaMixer {
            request.budgets().max_active_streams
        } else {
            request.budgets().max_active_streams.max(2)
        };
        let budget = StorageBudget::new(max_streams, request.budgets().max_total_pcm_memory)
            .map_err(|_| CapabilityError::CapacityExceeded)?;

        let mut resolved_targets = request.targets().to_vec();
        let engine = match plan.combine_resolution {
            CombineResolution::PassThrough(BackendId::Wasapi) => {
                let target = &mut resolved_targets[0];
                let open_target = WasapiOpenTarget::from_capture_target(target)?;
                let initial = match PlatformStream::open_reserved_target(
                    open_target,
                    target.clone(),
                    policy,
                    budget.clone(),
                    Duration::from_secs(5),
                ) {
                    Ok(stream) => stream,
                    Err((cleanup, err)) => {
                        if let Some(c) = cleanup {
                            let _ = c.wait(cleanup_timeout);
                        }
                        return Err(err);
                    }
                };
                resolve_pinned_capture_target(target, &initial.native_format.endpoint);
                PlatformExecutionEngine::PassThrough {
                    stream: Some(Box::new(initial)),
                    waiting_relaunch: false,
                    wait_deadline: None,
                }
            }
            CombineResolution::CspaMixer => {
                let mut streams: Vec<Option<PlatformStream>> =
                    Vec::with_capacity(resolved_targets.len());
                let mut last_active_at = Vec::with_capacity(resolved_targets.len());
                let mut wait_deadlines = Vec::with_capacity(resolved_targets.len());

                for target in &mut resolved_targets {
                    let open_target = match WasapiOpenTarget::from_capture_target(target) {
                        Ok(ot) => ot,
                        Err(err) => {
                            for existing in streams.into_iter().flatten() {
                                existing.stop(Stop::Abort);
                                let _ = existing.wait_cleanup(cleanup_timeout);
                            }
                            return Err(err);
                        }
                    };
                    match PlatformStream::open_reserved_target(
                        open_target,
                        target.clone(),
                        policy,
                        budget.clone(),
                        Duration::from_secs(5),
                    ) {
                        Ok(stream) => {
                            resolve_pinned_capture_target(target, &stream.native_format.endpoint);
                            streams.push(Some(stream));
                            last_active_at.push(Some(Instant::now()));
                            wait_deadlines.push(None);
                        }
                        Err((cleanup, err)) => {
                            if let Some(c) = cleanup {
                                let _ = c.wait(cleanup_timeout);
                            }
                            for existing in streams.into_iter().flatten() {
                                existing.stop(Stop::Abort);
                                let _ = existing.wait_cleanup(cleanup_timeout);
                            }
                            return Err(err);
                        }
                    }
                }

                let resolved_req = request.clone().with_targets(resolved_targets.clone());
                build_software_mix_engine(
                    resolved_req,
                    streams,
                    last_active_at,
                    wait_deadlines,
                    budget.clone(),
                    cleanup_timeout,
                )?
            }
            _ => return Err(CapabilityError::InvalidBackend),
        };

        let final_request = request.with_targets(resolved_targets);
        let route_guard = RouteRollbackGuard::new(final_request.allow_reroute());
        let final_plan = resolve_windows_plan(&final_request, timing).unwrap_or(plan);
        let mut mgr = Self {
            request: final_request,
            plan: final_plan,
            route_guard,
            policy,
            timing,
            cleanup_timeout,
            budget,
            engine,
            default_cancel: Cancellation::default(),
            pending_cleanup: None,
            pending_boundary: None,
            last_metadata: None,
            default_observer: None,
            default_input_observer: None,
            last_default_check: Instant::now(),
            session_counter: 1,
            segment_counter: 1,
            stopped: false,
            terminal_error: None,
            read_lock: Mutex::new(()),
        };
        mgr.refresh_default_observers();
        Ok(mgr)
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn from_synthetic_streams(
        request: CaptureRequest,
        streams: Vec<PlatformStream>,
        shared_budget: StorageBudget,
    ) -> Result<Self, CapabilityError> {
        let plan = resolve_windows_plan(&request, TimingRequirement::Raw)?;
        let cleanup_timeout = Duration::from_nanos(request.budgets().cleanup_timeout_ns);
        let route_guard = RouteRollbackGuard::new(request.allow_reroute());
        let engine = if plan.combine_resolution == CombineResolution::PassThrough(BackendId::Wasapi)
            && streams.len() == 1
        {
            PlatformExecutionEngine::PassThrough {
                stream: streams.into_iter().next().map(Box::new),
                waiting_relaunch: false,
                wait_deadline: None,
            }
        } else {
            let len = streams.len();
            build_software_mix_engine(
                request.clone(),
                streams.into_iter().map(Some).collect(),
                vec![Some(Instant::now()); len],
                vec![None; len],
                shared_budget.clone(),
                cleanup_timeout,
            )?
        };
        Ok(Self {
            request,
            plan,
            route_guard,
            policy: Overflow::Fail,
            timing: TimingRequirement::Raw,
            cleanup_timeout,
            budget: shared_budget,
            engine,
            default_cancel: Cancellation::default(),
            pending_cleanup: None,
            pending_boundary: None,
            last_metadata: None,
            default_observer: None,
            default_input_observer: None,
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

    #[cfg(test)]
    fn apply_opened_default_stream_for_test(
        &mut self,
        expected_kind: TargetKind,
        next: PlatformStream,
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
                let old_pcm = stream.as_ref().map(|s| s.native_format.pcm.clone());
                if let Some(old) = stream.take() {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }
                if old_pcm
                    .as_ref()
                    .is_some_and(|prev| prev != &next.native_format.pcm)
                    && self.request.on_format_change() == OnFormatChange::Fail
                {
                    next.stop(Stop::Abort);
                    let _ = next.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(ManagerError::Unsupported);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::DeviceChanged,
                    format: next.native_format.pcm.clone(),
                    device_id: Some(next.native_format.endpoint.clone()),
                    identity: None,
                });
                *stream = Some(Box::new(next));
                Ok(true)
            }
            PlatformExecutionEngine::SoftwareMix {
                multi,
                streams,
                last_active_at,
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
                let old_pcm = streams[idx]
                    .as_ref()
                    .map(|s| s.native_format.pcm.clone())
                    .or_else(|| {
                        multi
                            .input_handles()
                            .get(idx)
                            .map(|h| h.native_format().clone())
                    });
                if old_pcm
                    .as_ref()
                    .is_some_and(|prev| prev != &next.native_format.pcm)
                    && self.request.on_format_change() == OnFormatChange::Fail
                {
                    next.stop(Stop::Abort);
                    let _ = next.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(ManagerError::Unsupported);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                if let Some(old) = streams[idx].take() {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }
                let stride = next.native_format.pcm.bytes_per_frame();
                if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * stride {
                    in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * stride, 0);
                }
                streams[idx] = Some(next);
                last_active_at[idx] = Some(Instant::now());
                let _ = multi.set_input_state(idx, ParticipationState::Active);
                self.reconcile_software_mix_after_stream_change()?;
                self.segment_counter = self.segment_counter.saturating_add(1);
                Ok(true)
            }
        }
    }

    fn reconcile_software_mix_after_stream_change(&mut self) -> Result<(), CapabilityError> {
        let PlatformExecutionEngine::SoftwareMix { multi, streams, .. } = &mut self.engine else {
            return Ok(());
        };
        let fallback_fmt = multi.working_format().clone();
        let per_input: Vec<(BackendId, Arc<Format>)> = streams
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                let fmt = s.as_ref().map_or_else(
                    || {
                        multi
                            .input_handles()
                            .get(idx)
                            .map_or_else(|| fallback_fmt.clone(), |h| h.native_format().clone())
                    },
                    |st| st.native_format.pcm.clone(),
                );
                (BackendId::Wasapi, fmt)
            })
            .collect();

        match multi.reconfigure_targets_with_formats(self.request.targets().to_vec(), per_input) {
            Ok(_) => Ok(()),
            Err(ManagerError::WorkingFormatChanged) => {
                // Preserve live streams while rebuilding at the promoted working format.
                let PlatformExecutionEngine::SoftwareMix {
                    streams,
                    last_active_at,
                    wait_deadlines,
                    ..
                } = &mut self.engine
                else {
                    unreachable!()
                };
                let taken_streams = std::mem::take(streams);
                let taken_active = std::mem::take(last_active_at);
                let taken_deadlines = std::mem::take(wait_deadlines);
                self.engine = build_software_mix_engine(
                    self.request.clone(),
                    taken_streams,
                    taken_active,
                    taken_deadlines,
                    self.budget.clone(),
                    self.cleanup_timeout,
                )?;
                Ok(())
            }
            Err(err) => {
                self.terminal_error = Some(err);
                Err(map_manager_error(err))
            }
        }
    }

    fn notify_default_flow_changed(
        &mut self,
        expected_kind: TargetKind,
        new_endpoint: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        if self.stopped || self.terminal_error.is_some() {
            return Err(CapabilityError::Stopped);
        }
        let is_input = expected_kind == TargetKind::DefaultInput;
        let has_follow = self.request.targets().iter().any(|t| {
            t.kind() == expected_kind && t.default_device_mode() == DefaultDeviceMode::Follow
        });
        if !has_follow {
            return Err(CapabilityError::InvalidTarget);
        }

        let resolved_id = match new_endpoint {
            Some(id) => Some(id.to_owned()),
            None if is_input => query_default_capture_endpoint_id()?,
            None => query_default_render_endpoint_id()?,
        };

        match &mut self.engine {
            PlatformExecutionEngine::PassThrough { stream, .. } => {
                let target = self
                    .request
                    .targets()
                    .first()
                    .cloned()
                    .ok_or(CapabilityError::InvalidTarget)?;
                if target.kind() != expected_kind
                    || target.default_device_mode() != DefaultDeviceMode::Follow
                {
                    return Err(CapabilityError::InvalidTarget);
                }
                let old_pcm = stream.as_ref().map(|s| s.native_format.pcm.clone());
                if let Some(old) = stream.take() {
                    old.stop_checked(Stop::Drain)?;
                    if !old.wait_cleanup(self.cleanup_timeout) {
                        self.terminal_error = Some(ManagerError::CleanupTimeout);
                        return Err(CapabilityError::CleanupTimeout);
                    }
                }
                let open_target = match (is_input, resolved_id.as_deref()) {
                    (false, Some(id)) => WasapiOpenTarget::RenderEndpoint(id.into()),
                    (false, None) => WasapiOpenTarget::DefaultRender,
                    (true, Some(id)) => WasapiOpenTarget::CaptureEndpoint(id.into()),
                    (true, None) => WasapiOpenTarget::DefaultCapture,
                };
                let next = match PlatformStream::open_reserved_target(
                    open_target,
                    target,
                    self.policy,
                    self.budget.clone(),
                    Duration::from_secs(5),
                ) {
                    Ok(s) => s,
                    Err((cleanup, err)) => {
                        self.pending_cleanup = cleanup;
                        self.terminal_error = Some(ManagerError::OpenFailed);
                        return Err(err);
                    }
                };
                if old_pcm
                    .as_ref()
                    .is_some_and(|prev| prev != &next.native_format.pcm)
                    && self.request.on_format_change() == OnFormatChange::Fail
                {
                    next.stop(Stop::Abort);
                    let _ = next.wait_cleanup(self.cleanup_timeout);
                    self.terminal_error = Some(ManagerError::Unsupported);
                    return Err(CapabilityError::FormatChangeRejected);
                }
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::DeviceChanged,
                    format: next.native_format.pcm.clone(),
                    device_id: Some(next.native_format.endpoint.clone()),
                    identity: None,
                });
                *stream = Some(Box::new(next));
                Ok(true)
            }
            PlatformExecutionEngine::SoftwareMix {
                multi,
                streams,
                last_active_at,
                in_bytes_scratch,
                ..
            } => {
                let mut switched = 0usize;
                for idx in 0..self.request.targets().len() {
                    let target = &self.request.targets()[idx];
                    if target.kind() != expected_kind
                        || target.default_device_mode() != DefaultDeviceMode::Follow
                    {
                        continue;
                    }
                    if let (Some(new_id), Some(active)) = (resolved_id.as_deref(), &streams[idx])
                        && active.native_format.endpoint.as_ref() == new_id
                    {
                        continue;
                    }
                    let old_pcm = streams[idx]
                        .as_ref()
                        .map(|s| s.native_format.pcm.clone())
                        .or_else(|| {
                            multi
                                .input_handles()
                                .get(idx)
                                .map(|h| h.native_format().clone())
                        });
                    if let Some(old) = streams[idx].take() {
                        old.stop(Stop::Drain);
                        if !old.wait_cleanup(self.cleanup_timeout) {
                            self.terminal_error = Some(ManagerError::CleanupTimeout);
                            return Err(CapabilityError::CleanupTimeout);
                        }
                    }
                    let _ticket = multi.acquire_pending_open().map_err(map_manager_error)?;
                    let open_target = match (is_input, resolved_id.as_deref()) {
                        (false, Some(id)) => WasapiOpenTarget::RenderEndpoint(id.into()),
                        (false, None) => WasapiOpenTarget::DefaultRender,
                        (true, Some(id)) => WasapiOpenTarget::CaptureEndpoint(id.into()),
                        (true, None) => WasapiOpenTarget::DefaultCapture,
                    };
                    let next = match PlatformStream::open_reserved_target(
                        open_target,
                        target.clone(),
                        self.policy,
                        multi.storage_budget().clone(),
                        Duration::from_secs(5),
                    ) {
                        Ok(s) => s,
                        Err((cleanup, err)) => {
                            self.pending_cleanup = cleanup;
                            return Err(err);
                        }
                    };
                    if old_pcm
                        .as_ref()
                        .is_some_and(|prev| prev != &next.native_format.pcm)
                        && self.request.on_format_change() == OnFormatChange::Fail
                    {
                        next.stop(Stop::Abort);
                        let _ = next.wait_cleanup(self.cleanup_timeout);
                        self.terminal_error = Some(ManagerError::Unsupported);
                        return Err(CapabilityError::FormatChangeRejected);
                    }
                    let stride = next.native_format.pcm.bytes_per_frame();
                    if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * stride {
                        in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * stride, 0);
                    }
                    streams[idx] = Some(next);
                    last_active_at[idx] = Some(Instant::now());
                    let _ = multi.set_input_state(idx, ParticipationState::Active);
                    switched += 1;
                }
                if switched > 0 {
                    self.reconcile_software_mix_after_stream_change()?;
                    self.segment_counter = self.segment_counter.saturating_add(1);
                }
                Ok(switched > 0)
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

        if let Some(ref obs) = self.default_observer {
            let switched_id = match &self.engine {
                PlatformExecutionEngine::PassThrough { stream, .. } => self
                    .request
                    .targets()
                    .first()
                    .filter(|t| {
                        t.kind() == TargetKind::DefaultOutput
                            && t.default_device_mode() == DefaultDeviceMode::Follow
                    })
                    .and_then(|_| stream.as_ref().map(|s| s.native_format.endpoint.as_ref()))
                    .and_then(|cur| obs.changed_from(Some(cur))),
                PlatformExecutionEngine::SoftwareMix { streams, .. } => self
                    .request
                    .targets()
                    .iter()
                    .enumerate()
                    .find_map(|(idx, t)| {
                        if t.kind() == TargetKind::DefaultOutput
                            && t.default_device_mode() == DefaultDeviceMode::Follow
                        {
                            streams.get(idx).and_then(Option::as_ref).and_then(|s| {
                                obs.changed_from(Some(s.native_format.endpoint.as_ref()))
                            })
                        } else {
                            None
                        }
                    }),
            };
            if let Some(current_out) = switched_id {
                let _ =
                    self.notify_default_flow_changed(TargetKind::DefaultOutput, Some(&current_out));
            }
        }

        if let Some(ref obs) = self.default_input_observer {
            let switched_in_id = match &self.engine {
                PlatformExecutionEngine::PassThrough { stream, .. } => self
                    .request
                    .targets()
                    .first()
                    .filter(|t| {
                        t.kind() == TargetKind::DefaultInput
                            && t.default_device_mode() == DefaultDeviceMode::Follow
                    })
                    .and_then(|_| stream.as_ref().map(|s| s.native_format.endpoint.as_ref()))
                    .and_then(|cur| obs.changed_from(Some(cur))),
                PlatformExecutionEngine::SoftwareMix { streams, .. } => self
                    .request
                    .targets()
                    .iter()
                    .enumerate()
                    .find_map(|(idx, t)| {
                        if t.kind() == TargetKind::DefaultInput
                            && t.default_device_mode() == DefaultDeviceMode::Follow
                        {
                            streams.get(idx).and_then(Option::as_ref).and_then(|s| {
                                obs.changed_from(Some(s.native_format.endpoint.as_ref()))
                            })
                        } else {
                            None
                        }
                    }),
            };
            if let Some(current_in) = switched_in_id {
                let _ =
                    self.notify_default_flow_changed(TargetKind::DefaultInput, Some(&current_in));
            }
        }
    }

    fn update_targets_impl(
        &mut self,
        mut new_targets: Vec<CaptureTarget>,
    ) -> Result<u64, CapabilityError> {
        if self.stopped || self.terminal_error.is_some() {
            return Err(CapabilityError::Stopped);
        }
        let candidate_req = self.request.clone().with_targets(new_targets.clone());
        let new_plan = resolve_windows_plan(&candidate_req, self.timing)?;

        if let PlatformExecutionEngine::PassThrough {
            stream,
            waiting_relaunch,
            wait_deadline,
        } = &mut self.engine
            && new_plan.combine_resolution == CombineResolution::PassThrough(BackendId::Wasapi)
        {
            let mut new_target = new_targets.remove(0);
            let cur_target = &self.request.targets()[0];
            if cur_target.logical_id() == new_target.logical_id()
                && cur_target.backend() == new_target.backend()
                && cur_target.scope() == new_target.scope()
                && cur_target.default_device_mode() == new_target.default_device_mode()
            {
                let updated = cur_target
                    .clone()
                    .with_on_root_exit(new_target.on_root_exit())
                    .map_err(CapabilityError::from)?;
                self.request = self.request.clone().with_targets(vec![updated]);
                self.plan = new_plan;
                return Ok(self.segment_counter);
            }

            let open_target = WasapiOpenTarget::from_capture_target(&new_target)?;
            let next = match PlatformStream::open_reserved_target(
                open_target,
                new_target.clone(),
                self.policy,
                self.budget.clone(),
                Duration::from_secs(5),
            ) {
                Ok(s) => s,
                Err((cleanup, err)) => {
                    if let Some(c) = cleanup {
                        let _ = c.wait(self.cleanup_timeout);
                    }
                    return Err(err);
                }
            };
            if let Some(old) = stream.take() {
                old.stop(Stop::Drain);
                let _ = old.wait_cleanup(self.cleanup_timeout);
            }
            resolve_pinned_capture_target(&mut new_target, &next.native_format.endpoint);
            self.session_counter = self.session_counter.saturating_add(1);
            self.segment_counter = self.segment_counter.saturating_add(1);
            self.pending_boundary = Some(SessionBoundary {
                session: self.session_counter,
                segment: self.segment_counter,
                kind: BoundaryKind::ConfigurationChanged,
                format: next.native_format.pcm.clone(),
                device_id: Some(next.native_format.endpoint.clone()),
                identity: new_target.process_identity(),
            });
            *stream = Some(Box::new(next));
            *waiting_relaunch = false;
            *wait_deadline = None;
            let next_req = self.request.clone().with_targets(vec![new_target]);
            self.plan = resolve_windows_plan(&next_req, self.timing).unwrap_or(new_plan);
            self.request = next_req;
            self.refresh_default_observers();
            return Ok(self.segment_counter);
        }

        if let PlatformExecutionEngine::SoftwareMix {
            multi,
            streams,
            last_active_at,
            wait_deadlines,
            in_bytes_scratch,
            ..
        } = &mut self.engine
            && new_plan.combine_resolution == CombineResolution::CspaMixer
        {
            let old_targets = self.request.targets();
            let mut preview_used = vec![false; old_targets.len()];
            let mut planned_old_slot: Vec<Option<usize>> = Vec::with_capacity(new_targets.len());
            for target in &new_targets {
                let mut found = None;
                for (old_idx, old_target) in old_targets.iter().enumerate() {
                    if !preview_used[old_idx]
                        && old_target.logical_id() == target.logical_id()
                        && old_target.backend() == target.backend()
                        && old_target.scope() == target.scope()
                        && (streams[old_idx].is_some() || wait_deadlines[old_idx].is_some())
                    {
                        preview_used[old_idx] = true;
                        found = Some(old_idx);
                        break;
                    }
                }
                planned_old_slot.push(found);
            }

            let budget = multi.storage_budget().clone();
            let mut newly_opened: Vec<Option<PlatformStream>> =
                Vec::with_capacity(new_targets.len());
            for (new_idx, target) in new_targets.iter_mut().enumerate() {
                if planned_old_slot[new_idx].is_some() {
                    newly_opened.push(None);
                    continue;
                }
                let _open_ticket = multi.acquire_pending_open().map_err(map_manager_error)?;
                let open_target = match WasapiOpenTarget::from_capture_target(target) {
                    Ok(ot) => ot,
                    Err(err) => {
                        for opened in newly_opened.into_iter().flatten() {
                            opened.stop(Stop::Abort);
                            let _ = opened.wait_cleanup(self.cleanup_timeout);
                        }
                        return Err(err);
                    }
                };
                match PlatformStream::open_reserved_target(
                    open_target,
                    target.clone(),
                    self.policy,
                    budget.clone(),
                    Duration::from_secs(5),
                ) {
                    Ok(s) => {
                        resolve_pinned_capture_target(target, &s.native_format.endpoint);
                        newly_opened.push(Some(s));
                    }
                    Err((cleanup, err)) => {
                        if let Some(c) = cleanup {
                            let _ = c.wait(self.cleanup_timeout);
                        }
                        for opened in newly_opened.into_iter().flatten() {
                            opened.stop(Stop::Abort);
                            let _ = opened.wait_cleanup(self.cleanup_timeout);
                        }
                        return Err(err);
                    }
                }
            }

            let fallback_fmt = multi.working_format().clone();
            let per_input: Vec<(BackendId, Arc<Format>)> = (0..new_targets.len())
                .map(|new_idx| {
                    let fmt = if let Some(old_idx) = planned_old_slot[new_idx] {
                        streams
                            .get(old_idx)
                            .and_then(Option::as_ref)
                            .map(|s| s.native_format.pcm.clone())
                            .or_else(|| {
                                multi
                                    .input_handles()
                                    .get(old_idx)
                                    .map(|h| h.native_format().clone())
                            })
                            .unwrap_or_else(|| fallback_fmt.clone())
                    } else {
                        newly_opened[new_idx]
                            .as_ref()
                            .map_or_else(|| fallback_fmt.clone(), |s| s.native_format.pcm.clone())
                    };
                    (BackendId::Wasapi, fmt)
                })
                .collect();

            match multi.reconfigure_targets_with_formats(new_targets.clone(), per_input) {
                Ok(mapping) => {
                    let mut next_streams = Vec::with_capacity(new_targets.len());
                    let mut next_last_active = Vec::with_capacity(new_targets.len());
                    let mut next_wait_deadlines = Vec::with_capacity(new_targets.len());

                    for (new_idx, mapped_old) in mapping.iter().enumerate() {
                        if let Some(old_idx) = *mapped_old {
                            let retained_stream = streams
                                .get_mut(old_idx)
                                .and_then(Option::take)
                                .or_else(|| newly_opened[new_idx].take());
                            let retained_active = last_active_at
                                .get_mut(old_idx)
                                .and_then(Option::take)
                                .or_else(|| Some(Instant::now()));
                            let retained_deadline =
                                wait_deadlines.get_mut(old_idx).and_then(Option::take);
                            if retained_stream.is_some() {
                                let _ = multi.set_input_state(new_idx, ParticipationState::Active);
                            }
                            next_streams.push(retained_stream);
                            next_last_active.push(retained_active);
                            next_wait_deadlines.push(retained_deadline);
                        } else {
                            let opened = newly_opened[new_idx].take();
                            if opened.is_some() {
                                let _ = multi.set_input_state(new_idx, ParticipationState::Active);
                            }
                            next_streams.push(opened);
                            next_last_active.push(Some(Instant::now()));
                            next_wait_deadlines.push(None);
                        }
                    }

                    for retired in streams
                        .drain(..)
                        .flatten()
                        .chain(newly_opened.into_iter().flatten())
                    {
                        retired.stop(Stop::Drain);
                        let _ = retired.wait_cleanup(self.cleanup_timeout);
                    }

                    let max_stride = next_streams
                        .iter()
                        .flatten()
                        .map(|s| s.native_format.pcm.bytes_per_frame())
                        .max()
                        .unwrap_or(8)
                        .max(8);
                    if in_bytes_scratch.len() < MAX_FRAMES_PER_SLOT * max_stride {
                        in_bytes_scratch.resize(MAX_FRAMES_PER_SLOT * max_stride, 0);
                    }

                    *streams = next_streams;
                    *last_active_at = next_last_active;
                    *wait_deadlines = next_wait_deadlines;
                }
                Err(ManagerError::WorkingFormatChanged) => {
                    let mut next_streams = Vec::with_capacity(new_targets.len());
                    let mut next_last_active = Vec::with_capacity(new_targets.len());
                    let mut next_wait_deadlines = Vec::with_capacity(new_targets.len());
                    for new_idx in 0..new_targets.len() {
                        if let Some(old_idx) = planned_old_slot[new_idx] {
                            next_streams.push(streams[old_idx].take());
                            next_last_active.push(
                                last_active_at[old_idx]
                                    .take()
                                    .or_else(|| Some(Instant::now())),
                            );
                            next_wait_deadlines.push(wait_deadlines[old_idx].take());
                        } else {
                            next_streams.push(newly_opened[new_idx].take());
                            next_last_active.push(Some(Instant::now()));
                            next_wait_deadlines.push(None);
                        }
                    }
                    for retired in streams
                        .drain(..)
                        .flatten()
                        .chain(newly_opened.into_iter().flatten())
                    {
                        retired.stop(Stop::Drain);
                        let _ = retired.wait_cleanup(self.cleanup_timeout);
                    }
                    let rebuilt_req = self.request.clone().with_targets(new_targets.clone());
                    self.engine = build_software_mix_engine(
                        rebuilt_req,
                        next_streams,
                        next_last_active,
                        next_wait_deadlines,
                        self.budget.clone(),
                        self.cleanup_timeout,
                    )?;
                    self.session_counter = self.session_counter.saturating_add(1);
                }
                Err(err) => {
                    for opened in newly_opened.into_iter().flatten() {
                        opened.stop(Stop::Abort);
                        let _ = opened.wait_cleanup(self.cleanup_timeout);
                    }
                    return Err(map_manager_error(err));
                }
            }

            let next_req = self.request.clone().with_targets(new_targets);
            self.plan = resolve_windows_plan(&next_req, self.timing).unwrap_or(new_plan);
            self.request = next_req;
            self.refresh_default_observers();
            self.segment_counter = self.segment_counter.saturating_add(1);
            return Ok(self.segment_counter);
        }

        // Preserve surviving streams across engine transitions.
        match new_plan.combine_resolution {
            CombineResolution::CspaMixer => {
                let PlatformExecutionEngine::PassThrough {
                    stream: old_pt_stream,
                    waiting_relaunch,
                    wait_deadline,
                } = &mut self.engine
                else {
                    return Err(CapabilityError::NotImplemented);
                };
                let old_t = &self.request.targets()[0];
                let reusable_new_idx = new_targets.iter().position(|nt| {
                    old_t.logical_id() == nt.logical_id()
                        && old_t.backend() == nt.backend()
                        && old_t.scope() == nt.scope()
                        && (old_pt_stream.is_some() || *waiting_relaunch)
                });

                let mut newly_opened: Vec<Option<PlatformStream>> =
                    Vec::with_capacity(new_targets.len());
                for (idx, nt) in new_targets.iter_mut().enumerate() {
                    if Some(idx) == reusable_new_idx {
                        if let Some(s) = old_pt_stream.as_ref() {
                            resolve_pinned_capture_target(nt, &s.native_format.endpoint);
                        }
                        newly_opened.push(None);
                        continue;
                    }
                    let open_target = match WasapiOpenTarget::from_capture_target(nt) {
                        Ok(ot) => ot,
                        Err(err) => {
                            for s in newly_opened.into_iter().flatten() {
                                s.stop(Stop::Abort);
                                let _ = s.wait_cleanup(self.cleanup_timeout);
                            }
                            return Err(err);
                        }
                    };
                    match PlatformStream::open_reserved_target(
                        open_target,
                        nt.clone(),
                        self.policy,
                        self.budget.clone(),
                        Duration::from_secs(5),
                    ) {
                        Ok(s) => {
                            resolve_pinned_capture_target(nt, &s.native_format.endpoint);
                            newly_opened.push(Some(s));
                        }
                        Err((cleanup, err)) => {
                            if let Some(c) = cleanup {
                                let _ = c.wait(self.cleanup_timeout);
                            }
                            for s in newly_opened.into_iter().flatten() {
                                s.stop(Stop::Abort);
                                let _ = s.wait_cleanup(self.cleanup_timeout);
                            }
                            return Err(err);
                        }
                    }
                }

                let fallback_fmt = fallback_working_format()?;
                let preview_formats: Vec<Format> = (0..new_targets.len())
                    .map(|idx| {
                        if Some(idx) == reusable_new_idx {
                            old_pt_stream.as_ref().map_or_else(
                                || (*fallback_fmt).clone(),
                                |s| (*s.native_format.pcm).clone(),
                            )
                        } else {
                            newly_opened[idx].as_ref().map_or_else(
                                || (*fallback_fmt).clone(),
                                |s| (*s.native_format.pcm).clone(),
                            )
                        }
                    })
                    .collect();
                if crate::core::converter::resolve_mixer_working_format(
                    &preview_formats,
                    self.request.output_converter(),
                    self.request.input_conversion(),
                )
                .is_err()
                {
                    for s in newly_opened.into_iter().flatten() {
                        s.stop(Stop::Abort);
                        let _ = s.wait_cleanup(self.cleanup_timeout);
                    }
                    return Err(CapabilityError::UnsupportedFormat);
                }

                let mut next_streams = Vec::with_capacity(new_targets.len());
                let mut next_active = Vec::with_capacity(new_targets.len());
                let mut next_deadlines = Vec::with_capacity(new_targets.len());
                for (idx, opened_slot) in
                    newly_opened.iter_mut().enumerate().take(new_targets.len())
                {
                    if Some(idx) == reusable_new_idx {
                        next_streams.push(old_pt_stream.take().map(|b| *b));
                        next_active.push(Some(Instant::now()));
                        next_deadlines.push(if *waiting_relaunch {
                            *wait_deadline
                        } else {
                            None
                        });
                    } else {
                        next_streams.push(opened_slot.take());
                        next_active.push(Some(Instant::now()));
                        next_deadlines.push(None);
                    }
                }
                if let Some(retired) = old_pt_stream.take() {
                    retired.stop(Stop::Drain);
                    let _ = retired.wait_cleanup(self.cleanup_timeout);
                }

                let next_req = self.request.clone().with_targets(new_targets);
                self.engine = build_software_mix_engine(
                    next_req.clone(),
                    next_streams,
                    next_active,
                    next_deadlines,
                    self.budget.clone(),
                    self.cleanup_timeout,
                )?;
                self.plan = resolve_windows_plan(&next_req, self.timing).unwrap_or(new_plan);
                self.request = next_req;
                self.pending_boundary = None;
                self.refresh_default_observers();
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                Ok(self.segment_counter)
            }
            CombineResolution::PassThrough(BackendId::Wasapi) => {
                let PlatformExecutionEngine::SoftwareMix {
                    multi,
                    streams,
                    wait_deadlines,
                    ..
                } = &mut self.engine
                else {
                    return Err(CapabilityError::NotImplemented);
                };
                let mut new_target = new_targets.remove(0);
                let old_targets = self.request.targets();
                let reusable_old_idx = old_targets.iter().enumerate().find_map(|(idx, ot)| {
                    (ot.logical_id() == new_target.logical_id()
                        && ot.backend() == new_target.backend()
                        && ot.scope() == new_target.scope()
                        && (streams[idx].is_some() || wait_deadlines[idx].is_some()))
                    .then_some(idx)
                });

                let (pt_stream, pt_waiting, pt_deadline) = if let Some(old_idx) = reusable_old_idx {
                    let s = streams[old_idx].take();
                    if let Some(ref st) = s {
                        resolve_pinned_capture_target(&mut new_target, &st.native_format.endpoint);
                    }
                    let dl = wait_deadlines[old_idx].take();
                    (s, dl.is_some(), dl)
                } else {
                    let open_target = WasapiOpenTarget::from_capture_target(&new_target)?;
                    let s = match PlatformStream::open_reserved_target(
                        open_target,
                        new_target.clone(),
                        self.policy,
                        self.budget.clone(),
                        Duration::from_secs(5),
                    ) {
                        Ok(st) => st,
                        Err((cleanup, err)) => {
                            if let Some(c) = cleanup {
                                let _ = c.wait(self.cleanup_timeout);
                            }
                            return Err(err);
                        }
                    };
                    resolve_pinned_capture_target(&mut new_target, &s.native_format.endpoint);
                    (Some(s), false, None)
                };

                multi.clear_pending_output();
                for retired in streams.drain(..).flatten() {
                    retired.stop(Stop::Drain);
                    let _ = retired.wait_cleanup(self.cleanup_timeout);
                }

                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                if let Some(ref st) = pt_stream {
                    self.pending_boundary = Some(SessionBoundary {
                        session: self.session_counter,
                        segment: self.segment_counter,
                        kind: BoundaryKind::ConfigurationChanged,
                        format: st.native_format.pcm.clone(),
                        device_id: Some(st.native_format.endpoint.clone()),
                        identity: new_target.process_identity(),
                    });
                }
                self.engine = PlatformExecutionEngine::PassThrough {
                    stream: pt_stream.map(Box::new),
                    waiting_relaunch: pt_waiting,
                    wait_deadline: pt_deadline,
                };
                let next_req = self.request.clone().with_targets(vec![new_target]);
                self.plan = resolve_windows_plan(&next_req, self.timing).unwrap_or(new_plan);
                self.request = next_req;
                self.refresh_default_observers();
                Ok(self.segment_counter)
            }
            _ => Err(CapabilityError::InvalidBackend),
        }
    }

    fn reopen_relaunched_target_impl(
        &mut self,
        target_index: usize,
        candidate_pid: u32,
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
        let OnRootExit::Wait { wait_timeout_ns } = target.on_root_exit() else {
            return Err(CapabilityError::InvalidTarget);
        };
        let old_id = target
            .process_identity()
            .ok_or(CapabilityError::InvalidTarget)?;
        if target.scope() == ProcessScope::Exact {
            return Err(CapabilityError::UnsupportedScope);
        }

        match &mut self.engine {
            PlatformExecutionEngine::PassThrough {
                stream,
                waiting_relaunch,
                wait_deadline,
            } => {
                if target_index != 0 {
                    return Err(CapabilityError::InvalidTarget);
                }
                let deadline = wait_deadline
                    .unwrap_or_else(|| Instant::now() + Duration::from_nanos(wait_timeout_ns));
                if Instant::now() >= deadline {
                    self.terminal_error = Some(ManagerError::Deadline);
                    return Err(CapabilityError::CleanupTimeout);
                }
                let (cand_id, cand_exe) = inspect_process_executable(candidate_pid)?;
                if cand_id == old_id || cand_id.generation <= old_id.generation {
                    self.terminal_error = Some(ManagerError::InvalidObservation);
                    return Err(CapabilityError::PidReused);
                }
                if let Some(expected_exe) = target.executable_path()
                    && !expected_exe.eq_ignore_ascii_case(&cand_exe)
                {
                    self.terminal_error = Some(ManagerError::InvalidObservation);
                    return Err(CapabilityError::InvalidTarget);
                }
                if let Some(old) = stream.take() {
                    old.stop_checked(Stop::Drain)?;
                    if !old.wait_cleanup(self.cleanup_timeout) {
                        self.terminal_error = Some(ManagerError::CleanupTimeout);
                        return Err(CapabilityError::CleanupTimeout);
                    }
                }
                let updated_target = target
                    .with_process_identity(cand_id)
                    .map_err(CapabilityError::from)?;
                let next = match PlatformStream::open_reserved_target(
                    WasapiOpenTarget::ProcessTree(cand_id),
                    updated_target.clone(),
                    self.policy,
                    self.budget.clone(),
                    Duration::from_secs(5),
                ) {
                    Ok(s) => s,
                    Err((cleanup, err)) => {
                        self.pending_cleanup = cleanup;
                        self.terminal_error = Some(ManagerError::OpenFailed);
                        return Err(err);
                    }
                };
                self.session_counter = self.session_counter.saturating_add(1);
                self.segment_counter = self.segment_counter.saturating_add(1);
                *waiting_relaunch = false;
                *wait_deadline = None;
                self.pending_boundary = Some(SessionBoundary {
                    session: self.session_counter,
                    segment: self.segment_counter,
                    kind: BoundaryKind::ConfigurationChanged,
                    format: next.native_format.pcm.clone(),
                    device_id: Some(next.native_format.endpoint.clone()),
                    identity: Some(cand_id),
                });
                *stream = Some(Box::new(next));
                self.request = self.request.clone().with_targets(vec![updated_target]);
                Ok(())
            }
            PlatformExecutionEngine::SoftwareMix {
                multi,
                streams,
                last_active_at,
                wait_deadlines,
                ..
            } => {
                let deadline = wait_deadlines[target_index]
                    .unwrap_or_else(|| Instant::now() + Duration::from_nanos(wait_timeout_ns));
                if Instant::now() >= deadline {
                    let _ = multi.set_input_state(target_index, ParticipationState::Failed(9));
                    return Err(CapabilityError::CleanupTimeout);
                }
                let (cand_id, cand_exe) = inspect_process_executable(candidate_pid)?;
                if cand_id == old_id || cand_id.generation <= old_id.generation {
                    return Err(CapabilityError::PidReused);
                }
                if let Some(expected_exe) = target.executable_path()
                    && !expected_exe.eq_ignore_ascii_case(&cand_exe)
                {
                    return Err(CapabilityError::InvalidTarget);
                }
                if let Some(old) = streams[target_index].take() {
                    old.stop(Stop::Drain);
                    let _ = old.wait_cleanup(self.cleanup_timeout);
                }
                let _ticket = multi.acquire_pending_open().map_err(map_manager_error)?;
                let updated_target = target
                    .with_process_identity(cand_id)
                    .map_err(CapabilityError::from)?;
                let next = match PlatformStream::open_reserved_target(
                    WasapiOpenTarget::ProcessTree(cand_id),
                    updated_target.clone(),
                    self.policy,
                    multi.storage_budget().clone(),
                    Duration::from_secs(5),
                ) {
                    Ok(s) => s,
                    Err((cleanup, err)) => {
                        self.pending_cleanup = cleanup;
                        return Err(err);
                    }
                };
                let mut next_targets = self.request.targets().to_vec();
                next_targets[target_index] = updated_target;
                self.request = self.request.clone().with_targets(next_targets);
                streams[target_index] = Some(next);
                last_active_at[target_index] = Some(Instant::now());
                wait_deadlines[target_index] = None;
                let _ = multi.set_input_state(target_index, ParticipationState::Active);
                self.segment_counter = self.segment_counter.saturating_add(1);
                Ok(())
            }
        }
    }

    fn drain_software_mix_inputs(&mut self, timeout: Duration, cancel: &Cancellation) -> u64 {
        let targets_ref = &self.request;
        let cleanup_timeout = self.cleanup_timeout;
        let PlatformExecutionEngine::SoftwareMix {
            block_frames,
            multi,
            streams,
            last_active_at,
            wait_deadlines,
            started_at,
            in_bytes_scratch,
        } = &mut self.engine
        else {
            return 0;
        };

        let working_rate = u64::from(multi.working_format().rate().max(1));
        let mix_quantum = *block_frames;
        let call_started = Instant::now();

        loop {
            let any_recently_active = last_active_at
                .iter()
                .any(|t| t.is_some_and(|inst| inst.elapsed() < Duration::from_millis(80)));
            let mut waited_on_idle_leader = false;
            let mut pushed_any = false;

            for idx in 0..streams.len() {
                if let Some(deadline) = wait_deadlines[idx]
                    && Instant::now() >= deadline
                {
                    wait_deadlines[idx] = None;
                    let _ = multi.set_input_state(idx, ParticipationState::Failed(9));
                }
                let already_queued = multi
                    .input_handles()
                    .get(idx)
                    .and_then(|h| h.snapshot())
                    .map_or(0, |s| s.queued_packets);
                if already_queued > 0 {
                    pushed_any = true;
                    continue;
                }

                let mut target_exited = false;
                let mut target_failed = false;
                if let Some(stream) = &mut streams[idx] {
                    let bpf = stream.native_format.pcm.bytes_per_frame();
                    let stream_rate = u64::from(stream.native_format.pcm.rate().max(1));
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
                        let is_this_active = last_active_at[idx]
                            .is_some_and(|inst| inst.elapsed() < Duration::from_millis(80));
                        let stream_timeout = if pass > 0 {
                            timeout.min(Duration::from_millis(12))
                        } else if is_this_active {
                            timeout.min(Duration::from_millis(20))
                        } else if !any_recently_active && !pushed_any && !waited_on_idle_leader {
                            waited_on_idle_leader = true;
                            timeout.min(Duration::from_millis(10))
                        } else {
                            Duration::ZERO
                        };

                        match stream.read_into_with_cancel(
                            &mut in_bytes_scratch[..dst_bytes],
                            in_quantum,
                            stream_timeout,
                            cancel.clone(),
                        ) {
                            Ok(res) if res.frames > 0 => {
                                last_active_at[idx] = Some(Instant::now());
                                let now_ns = started_at.elapsed().as_nanos() as u64;
                                let stamp = res.first_packet.and_then(|a| a.header.timestamp);
                                if multi
                                    .push_raw_input_bytes(
                                        idx,
                                        &in_bytes_scratch[..res.frames * bpf],
                                        stamp,
                                        now_ns,
                                    )
                                    .is_err()
                                {
                                    target_failed = true;
                                    break;
                                }
                                if res.reason == PartialReason::Eof {
                                    target_exited = true;
                                    break;
                                }
                                let now_queued = multi
                                    .input_handles()
                                    .get(idx)
                                    .and_then(|h| h.snapshot())
                                    .map_or(0, |s| s.queued_packets);
                                if now_queued > 0 {
                                    pushed_any = true;
                                    break;
                                }
                            }
                            Ok(res) if res.reason == PartialReason::Boundary => continue,
                            Ok(res) if res.reason == PartialReason::Eof => {
                                target_exited = true;
                                break;
                            }
                            Ok(res) if matches!(res.reason, PartialReason::Failure(_)) => {
                                target_failed = true;
                                break;
                            }
                            Ok(_) => {
                                if pass == 0
                                    && last_active_at[idx].is_none_or(|inst| {
                                        inst.elapsed() >= Duration::from_millis(500)
                                    })
                                {
                                    let _ = multi.set_input_state(idx, ParticipationState::Idle);
                                }
                                break;
                            }
                            Err(_) => {
                                target_failed = true;
                                break;
                            }
                        }
                    }
                }

