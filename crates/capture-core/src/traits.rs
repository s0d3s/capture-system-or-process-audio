use crate::{
    discovery::{
        DiscoveryFilter, DiscoveryQuery, DiscoveryService, DiscoverySubscription, FindAppError,
        TargetCatalog, TargetSnapshot,
    },
    manager::{InputStatusHandle, ManagerError},
    measurements::Trace,
    ordering::{ControlHandle, Overflow, Stop},
    planner::{CapturePlanReport, CaptureRequest},
    reads::{Cancellation, ReadError, ReadResult},
    routing::RouteRollbackGuard,
    schema::{
        BackendId, Backends, CaptureTarget, Format, MembershipDescriptor, MixedBlockMetadata,
        OnFormatChange, OnRootExit, PermissionKind, PermissionPolicy, PermissionReport, PlanError,
        PlatformOs, ProcessScope, ProcessTreeRange,
    },
    stream_runtime::EndReason,
};
use std::{sync::Arc, time::Duration};

/// Cross-platform sample-timing qualification level.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimingRequirement {
    #[default]
    Raw,
    Precise,
}

/// Cross-platform capability, permission, liveness, and session error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    InvalidTarget,
    UnsupportedScope,
    UnsupportedTargetKind,
    TargetNotShareableApplication,
    ReversedStrategyRange,
    InvalidBackend,
    PermissionDenied,
    DeviceUnavailable,
    ServerUnavailable,
    ClientLibraryUnavailable {
        backend: BackendId,
        detail: Arc<str>,
    },
    BackendVersionTooOld {
        backend: BackendId,
        required: (u32, u32, u32),
        observed: (u32, u32, u32),
    },
    NoSuchProcess,
    PidReused,
    StaleNativeObject(u32),
    RerouteNotPermitted,
    PreciseTimingUnqualified,
    CapacityExceeded,
    UnsupportedFormat,
    FormatChangeRejected,
    CleanupTimeout,
    Stopped,
    NotImplemented,
    BackendFailure {
        backend: BackendId,
        code: i32,
    },
}

impl CapabilityError {
    /// Returns `true` when the planner may evaluate the next candidate backend on the same OS
    /// (when allowed by `Backends` and `target.backend().is_none()`), and `false` when planning
    /// must abort immediately (`PermissionDenied`, `PidReused`, `NoSuchProcess`, invalid config, etc.).
    #[inline]
    pub fn allows_cross_backend_fallback(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedScope
                | Self::UnsupportedTargetKind
                | Self::TargetNotShareableApplication
                | Self::BackendVersionTooOld { .. }
                | Self::ServerUnavailable
                | Self::ClientLibraryUnavailable { .. }
                | Self::NotImplemented
        )
    }

    /// Project a `CapabilityError` into the portable `PlanError` summary recorded on
    /// `TargetCandidateAttempt` entries inside `CapturePlanReport`.
    pub fn to_plan_error(&self) -> PlanError {
        match self {
            Self::InvalidTarget => PlanError::InvalidTarget,
            Self::UnsupportedScope
            | Self::UnsupportedTargetKind
            | Self::TargetNotShareableApplication => PlanError::UnsupportedScope,
            Self::ReversedStrategyRange => PlanError::ReversedStrategyRange,
            Self::InvalidBackend => PlanError::InvalidBackend,
            Self::PermissionDenied => PlanError::PermissionDenied,
            Self::DeviceUnavailable
            | Self::ServerUnavailable
            | Self::ClientLibraryUnavailable { .. }
            | Self::BackendVersionTooOld { .. }
            | Self::BackendFailure { .. } => PlanError::BackendUnavailable,
            Self::NoSuchProcess => PlanError::NoSuchProcess,
            Self::PidReused | Self::StaleNativeObject(_) => PlanError::PidReused,
            Self::PreciseTimingUnqualified => PlanError::PreciseTimingUnqualified,
            Self::CapacityExceeded => PlanError::Capacity(crate::CapacityError::ExceedsByteCeiling),
            Self::RerouteNotPermitted
            | Self::UnsupportedFormat
            | Self::FormatChangeRejected
            | Self::CleanupTimeout
            | Self::Stopped
            | Self::NotImplemented => PlanError::UnsupportedCapturePlan,
        }
    }
}

impl std::fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget => {
                formatter.write_str("invalid capture target or missing target policy")
            }
            Self::UnsupportedScope => {
                formatter.write_str("requested capture scope is not supported on this backend")
            }
            Self::UnsupportedTargetKind => {
                formatter.write_str("requested target kind is not supported on this backend")
            }
            Self::TargetNotShareableApplication => formatter.write_str(
                "target process is not a discoverable shareable application on this backend",
            ),
            Self::ReversedStrategyRange => {
                formatter.write_str("reversed process-tree strategy range")
            }
            Self::InvalidBackend => {
                formatter.write_str("invalid or unsupported backend selection for this OS")
            }
            Self::PermissionDenied => formatter.write_str(
                "OS microphone/screen-audio privacy consent or audio server access denied",
            ),
            Self::DeviceUnavailable => formatter.write_str(
                "requested audio endpoint is unavailable, disconnected, or in exclusive use",
            ),
            Self::ServerUnavailable => {
                formatter.write_str("OS audio server or session daemon is unavailable")
            }
            Self::ClientLibraryUnavailable { backend, detail } => write!(
                formatter,
                "client library for backend {} is unavailable: {detail}",
                backend.as_str()
            ),
            Self::BackendVersionTooOld {
                backend,
                required,
                observed,
            } => write!(
                formatter,
                "backend {} requires version {}.{}.{}, observed {}.{}.{}",
                backend.as_str(),
                required.0,
                required.1,
                required.2,
                observed.0,
                observed.1,
                observed.2
            ),
            Self::NoSuchProcess => {
                formatter.write_str("target process does not exist or has already exited")
            }
            Self::PidReused => formatter
                .write_str("target process creation identity changed (PID reused); refusing open"),
            Self::StaleNativeObject(object_id) => write!(
                formatter,
                "native audio process object {object_id} belongs to an exited or stale process"
            ),
            Self::RerouteNotPermitted => {
                formatter.write_str("playback rerouting or muting requires allow_reroute = true")
            }
            Self::PreciseTimingUnqualified => {
                formatter.write_str("independent sample timing within 1 ms is not qualified")
            }
            Self::CapacityExceeded => {
                formatter.write_str("capture request exceeds configured storage or stream budgets")
            }
            Self::UnsupportedFormat => formatter.write_str(
                "unsupported native PCM representation or resampling rejected under StrictNoResample",
            ),
            Self::FormatChangeRejected => formatter
                .write_str("native format changed while on_format_change=fail is active"),
            Self::CleanupTimeout => {
                formatter.write_str("native capture startup or teardown timed out")
            }
            Self::Stopped => formatter.write_str("capture manager is stopped or in terminal state"),
            Self::NotImplemented => {
                formatter.write_str("requested capture strategy or policy is not implemented")
            }
            Self::BackendFailure { backend, code } => write!(
                formatter,
                "{} backend failure (code 0x{code:08x})",
                backend.as_str()
            ),
        }
    }
}

impl std::error::Error for CapabilityError {}

impl From<PlanError> for CapabilityError {
    fn from(err: PlanError) -> Self {
        match err {
            PlanError::InvalidTarget => Self::InvalidTarget,
            PlanError::UnsupportedScope => Self::UnsupportedScope,
            PlanError::ReversedStrategyRange => Self::ReversedStrategyRange,
            PlanError::InvalidBackend => Self::InvalidBackend,
            PlanError::BackendUnavailable => Self::ServerUnavailable,
            PlanError::PermissionDenied => Self::PermissionDenied,
            PlanError::NoSuchProcess => Self::NoSuchProcess,
            PlanError::PidReused => Self::PidReused,
            PlanError::PreciseTimingUnqualified => Self::PreciseTimingUnqualified,
            PlanError::UnsupportedCapturePlan => Self::NotImplemented,
            PlanError::Capacity(_) => Self::CapacityExceeded,
        }
    }
}

impl From<ManagerError> for CapabilityError {
    fn from(err: ManagerError) -> Self {
        match err {
            ManagerError::InvalidConfiguration | ManagerError::InvalidObservation => {
                Self::InvalidTarget
            }
            ManagerError::InvalidState => Self::Stopped,
            ManagerError::Capacity
            | ManagerError::CandidateCapacity
            | ManagerError::EventCapacity
            | ManagerError::Exhausted => Self::CapacityExceeded,
            ManagerError::Deadline | ManagerError::CleanupTimeout => Self::CleanupTimeout,
            ManagerError::Permission => Self::PermissionDenied,
            ManagerError::Unsupported => Self::UnsupportedFormat,
            ManagerError::WorkingFormatChanged => Self::FormatChangeRejected,
            ManagerError::UnknownVisibility
            | ManagerError::Ambiguous
            | ManagerError::OpenFailed => Self::DeviceUnavailable,
        }
    }
}

/// A stream owns one backend capture session and never monitors process trees, dynamic membership,
/// or default-device switches.
pub trait AudioStreamBackend: Send + Sized {
    fn format(&self) -> &Arc<Format>;
    fn trace(&self) -> Option<&Arc<Trace>>;
    fn control_handle(&self) -> ControlHandle;
    fn default_cancel(&self) -> Cancellation;
    fn end_reason(&self) -> Option<EndReason>;

    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError>;

    fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError>;

    fn stop(&self, mode: Stop);
    fn resources_done(&self) -> bool;
    fn wait_cleanup(&self, timeout: Duration) -> bool;

    #[inline]
    fn read(&mut self, frames: usize, timeout: Duration) -> Result<ReadResult, ReadError> {
        let cancel = self.default_cancel();
        self.read_with_cancel(frames, timeout, cancel)
    }

    #[inline]
    fn read_into(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
    ) -> Result<ReadResult, ReadError> {
        let cancel = self.default_cancel();
        self.read_into_with_cancel(destination, frames, timeout, cancel)
    }
}

/// A platform stream that can be opened directly from a single-point `CaptureTarget`.
pub trait OpenableStreamBackend: AudioStreamBackend {
    fn open_with_permission_policy(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
        permission_policy: PermissionPolicy,
    ) -> Result<Self, CapabilityError>;

    #[inline]
    fn open_with_options(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError> {
        Self::open_with_permission_policy(
            target,
            tree_range,
            backends,
            overflow,
            timing,
            PermissionPolicy::AutoRequest,
        )
    }

    #[inline]
    fn open(target: &CaptureTarget, overflow: Overflow) -> Result<Self, CapabilityError> {
        Self::open_with_permission_policy(
            target,
            ProcessTreeRange::default(),
            &Backends::default(),
            overflow,
            TimingRequirement::Raw,
            PermissionPolicy::AutoRequest,
        )
    }
}

/// Zero-cost static-dispatch core `AudioStream<B>` entity (`0` vtable overhead, `0` steady-state
/// heap allocations on `read_into`).
#[repr(transparent)]
pub struct AudioStream<B: AudioStreamBackend> {
    inner: B,
}

impl<B: OpenableStreamBackend> AudioStream<B> {
    #[inline]
    pub fn open(target: &CaptureTarget, overflow: Overflow) -> Result<Self, CapabilityError> {
        B::open(target, overflow).map(Self::new)
    }

    #[inline]
    pub fn open_with_options(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError> {
        B::open_with_options(target, tree_range, backends, overflow, timing).map(Self::new)
    }

    #[inline]
    pub fn open_with_permission_policy(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
        permission_policy: PermissionPolicy,
    ) -> Result<Self, CapabilityError> {
        B::open_with_permission_policy(
            target,
            tree_range,
            backends,
            overflow,
            timing,
            permission_policy,
        )
        .map(Self::new)
    }
}

impl<B: AudioStreamBackend> AudioStream<B> {
    #[inline]
    pub fn new(inner: B) -> Self {
        Self { inner }
    }

    #[inline]
    pub fn into_inner(self) -> B {
        self.inner
    }

    #[inline]
    pub fn backend(&self) -> &B {
        &self.inner
    }

    #[inline]
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    #[inline]
    pub fn format(&self) -> &Arc<Format> {
        self.inner.format()
    }

    #[inline]
    pub fn trace(&self) -> Option<&Arc<Trace>> {
        self.inner.trace()
    }

    #[inline]
    pub fn control_handle(&self) -> ControlHandle {
        self.inner.control_handle()
    }

    #[inline]
    pub fn default_cancel(&self) -> Cancellation {
        self.inner.default_cancel()
    }

    #[inline]
    pub fn end_reason(&self) -> Option<EndReason> {
        self.inner.end_reason()
    }

    #[inline]
    pub fn read(&mut self, frames: usize, timeout: Duration) -> Result<ReadResult, ReadError> {
        self.inner.read(frames, timeout)
    }

    #[inline]
    pub fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.inner.read_with_cancel(frames, timeout, cancel)
    }

    #[inline]
    pub fn read_into(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
    ) -> Result<ReadResult, ReadError> {
        self.inner.read_into(destination, frames, timeout)
    }

    #[inline]
    pub fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.inner
            .read_into_with_cancel(destination, frames, timeout, cancel)
    }

    #[inline]
    pub fn stop(&self, mode: Stop) {
        self.inner.stop(mode);
    }

    #[inline]
    pub fn resources_done(&self) -> bool {
        self.inner.resources_done()
    }

    #[inline]
    pub fn wait_cleanup(&self, timeout: Duration) -> bool {
        self.inner.wait_cleanup(timeout)
    }
}

/// Lazy discovery with shared methods that honor `self.backends()`.
pub trait DiscoveryBackend: Clone + Send + Sync + Sized {
    fn new() -> Self;
    fn with_backends(backends: Backends) -> Result<Self, CapabilityError>;
    fn service(&self) -> &DiscoveryService;
    fn backends(&self) -> &Backends;
    fn is_watcher_running(&self) -> bool;
    fn refresh_once(&self) -> Result<u64, CapabilityError>;
    fn default_output_device_id(&self) -> Result<Option<Arc<str>>, CapabilityError>;
    fn default_input_device_id(&self) -> Result<Option<Arc<str>>, CapabilityError>;

    fn process(
        &self,
        pid: u32,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, CapabilityError>;

    fn discover_query(&self, query: &DiscoveryQuery) -> Result<TargetCatalog, CapabilityError>;

    fn subscribe(
        &self,
        filter: DiscoveryFilter,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, CapabilityError>;

    fn subscribe_query(
        &self,
        query: DiscoveryQuery,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, CapabilityError>;

    fn discover(&self, filter: DiscoveryFilter) -> Result<TargetSnapshot, CapabilityError> {
        let query = DiscoveryQuery::from_filter_for_os(self.service().os(), &filter)
            .with_backends(self.backends().clone());
        let catalog = self.discover_query(&query)?;
        Ok(TargetSnapshot {
            cursor: catalog.cursor,
            targets: catalog.targets,
        })
    }

    fn applications(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> Result<Vec<CaptureTarget>, CapabilityError> {
        Ok(self
            .discover(DiscoveryFilter::applications(
                active_only,
                include_non_audio,
            ))?
            .targets)
    }

    fn processes(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> Result<Vec<CaptureTarget>, CapabilityError> {
        Ok(self
            .discover(DiscoveryFilter::processes(active_only, include_non_audio))?
            .targets)
    }

    fn output_devices(&self) -> Result<Vec<CaptureTarget>, CapabilityError> {
        Ok(self.discover(DiscoveryFilter::output_devices())?.targets)
    }

    fn input_devices(&self) -> Result<Vec<CaptureTarget>, CapabilityError> {
        let query = DiscoveryQuery::input_devices_for_os(self.service().os())
            .with_backends(self.backends().clone());
        Ok(self.discover_query(&query)?.input_devices)
    }

    fn find_app(
        &self,
        query: &str,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, FindAppError> {
        if !self.is_watcher_running() {
            let _ = self.refresh_once();
        }
        self.service()
            .find_app_with_backends(query, self.backends(), scope, on_root_exit)
    }
}

/// Static-dispatch discovery wrapper.
#[repr(transparent)]
#[derive(Clone)]
pub struct Discovery<B: DiscoveryBackend> {
    inner: B,
}

impl<B: DiscoveryBackend> Default for Discovery<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: DiscoveryBackend> Discovery<B> {
    #[inline]
    pub fn new() -> Self {
        Self { inner: B::new() }
    }

    #[inline]
    pub fn from_backend(inner: B) -> Self {
        Self { inner }
    }

    #[inline]
    pub fn with_backends(backends: Backends) -> Result<Self, CapabilityError> {
        Ok(Self {
            inner: B::with_backends(backends)?,
        })
    }

    #[inline]
    pub fn into_inner(self) -> B {
        self.inner
    }

    #[inline]
    pub fn backend(&self) -> &B {
        &self.inner
    }

    #[inline]
    pub fn service(&self) -> &DiscoveryService {
        self.inner.service()
    }

    #[inline]
    pub fn backends(&self) -> &Backends {
        self.inner.backends()
    }

    #[inline]
    pub fn is_watcher_running(&self) -> bool {
        self.inner.is_watcher_running()
    }

    #[inline]
    pub fn refresh_once(&self) -> Result<u64, CapabilityError> {
        self.inner.refresh_once()
    }

    #[inline]
    pub fn default_output_device_id(&self) -> Result<Option<Arc<str>>, CapabilityError> {
        self.inner.default_output_device_id()
    }

    #[inline]
    pub fn default_input_device_id(&self) -> Result<Option<Arc<str>>, CapabilityError> {
        self.inner.default_input_device_id()
    }

    #[inline]
    pub fn process(
        &self,
        pid: u32,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, CapabilityError> {
        self.inner.process(pid, scope, on_root_exit)
    }

    #[inline]
    pub fn find_app(
        &self,
        query: &str,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, FindAppError> {
        self.inner.find_app(query, scope, on_root_exit)
    }

    #[inline]
    pub fn applications(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> Result<Vec<CaptureTarget>, CapabilityError> {
        self.inner.applications(active_only, include_non_audio)
    }

    #[inline]
    pub fn processes(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> Result<Vec<CaptureTarget>, CapabilityError> {
        self.inner.processes(active_only, include_non_audio)
    }

    #[inline]
    pub fn output_devices(&self) -> Result<Vec<CaptureTarget>, CapabilityError> {
        self.inner.output_devices()
    }

    #[inline]
    pub fn input_devices(&self) -> Result<Vec<CaptureTarget>, CapabilityError> {
        self.inner.input_devices()
    }

    #[inline]
    pub fn discover(&self, filter: DiscoveryFilter) -> Result<TargetSnapshot, CapabilityError> {
        self.inner.discover(filter)
    }

    #[inline]
    pub fn discover_query(&self, query: &DiscoveryQuery) -> Result<TargetCatalog, CapabilityError> {
        self.inner.discover_query(query)
    }

    #[inline]
    pub fn subscribe(
        &self,
        filter: DiscoveryFilter,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, CapabilityError> {
        self.inner.subscribe(filter, max_discovery_queue)
    }

    #[inline]
    pub fn subscribe_query(
        &self,
        query: DiscoveryQuery,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, CapabilityError> {
        self.inner.subscribe_query(query, max_discovery_queue)
    }
}

/// Plans, monitors, mixes, converts, reconfigures, and reopens capture sessions.
pub trait CaptureManagerBackend: Send + Sized {
    fn open(request: CaptureRequest) -> Result<Self, CapabilityError> {
        Self::open_with_options(request, Overflow::DropOldest, TimingRequirement::Raw)
    }

    fn open_single(target: CaptureTarget) -> Result<Self, CapabilityError> {
        Self::open(CaptureRequest::single(target))
    }

    fn open_with_options(
        request: CaptureRequest,
        overflow: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError>;

    fn plan(&self) -> &CapturePlanReport;
    fn selected_backend(&self) -> BackendId;
    fn is_software_mix(&self) -> bool;
    fn working_format(&self) -> Option<&Arc<Format>>;
    fn output_format(&self) -> Option<&Arc<Format>>;
    fn targets(&self) -> &[CaptureTarget];
    fn input_handles(&self) -> &[InputStatusHandle];
    fn membership_descriptor(&self) -> Option<MembershipDescriptor>;
    fn last_block_metadata(&self) -> Option<MixedBlockMetadata>;
    fn session_id(&self) -> u64;
    fn segment_id(&self) -> u64;
    fn on_format_change(&self) -> OnFormatChange;
    fn route_guard(&self) -> Option<&RouteRollbackGuard>;

    fn update_targets(&mut self, new_targets: Vec<CaptureTarget>) -> Result<u64, CapabilityError>;
    fn notify_default_device_changed(
        &mut self,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError>;
    fn notify_default_input_changed(
        &mut self,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError>;
    fn reopen_relaunched_target(
        &mut self,
        target_index: usize,
        new_pid: u32,
    ) -> Result<(), CapabilityError>;

    fn default_cancel(&self) -> Cancellation;

    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError>;

    fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError>;

    fn stop(&mut self, mode: Stop);
    fn wait_cleanup(&mut self, timeout: Duration) -> bool;
    fn resources_done(&self) -> bool;

    #[inline]
    fn read(&mut self, frames: usize, timeout: Duration) -> Result<ReadResult, ReadError> {
        let cancel = self.default_cancel();
        self.read_with_cancel(frames, timeout, cancel)
    }

    #[inline]
    fn read_into(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
    ) -> Result<ReadResult, ReadError> {
        let cancel = self.default_cancel();
        self.read_into_with_cancel(destination, frames, timeout, cancel)
    }
}

/// Zero-cost static-dispatch core `CaptureManager<B>` entity (`0` vtable overhead, `0` steady-state
/// heap allocations on `read_into`).
#[repr(transparent)]
pub struct CaptureManager<B: CaptureManagerBackend> {
    inner: B,
}

impl<B: CaptureManagerBackend> CaptureManager<B> {
    #[inline]
    pub fn from_backend(inner: B) -> Self {
        Self { inner }
    }

    #[inline]
    pub fn open(request: CaptureRequest) -> Result<Self, CapabilityError> {
        Ok(Self {
            inner: B::open(request)?,
        })
    }

    #[inline]
    pub fn open_single(target: CaptureTarget) -> Result<Self, CapabilityError> {
        Ok(Self {
            inner: B::open_single(target)?,
        })
    }

    #[inline]
    pub fn open_with_options(
        request: CaptureRequest,
        overflow: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError> {
        Ok(Self {
            inner: B::open_with_options(request, overflow, timing)?,
        })
    }

    #[inline]
    pub fn into_inner(self) -> B {
        self.inner
    }

    #[inline]
    pub fn backend(&self) -> &B {
        &self.inner
    }

    #[inline]
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    #[inline]
    pub fn plan(&self) -> &CapturePlanReport {
        self.inner.plan()
    }

    #[inline]
    pub fn selected_backend(&self) -> BackendId {
        self.inner.selected_backend()
    }

    #[inline]
    pub fn is_software_mix(&self) -> bool {
        self.inner.is_software_mix()
    }

    #[inline]
    pub fn working_format(&self) -> Option<&Arc<Format>> {
        self.inner.working_format()
    }

    #[inline]
    pub fn output_format(&self) -> Option<&Arc<Format>> {
        self.inner.output_format()
    }

    #[inline]
    pub fn targets(&self) -> &[CaptureTarget] {
        self.inner.targets()
    }

    #[inline]
    pub fn input_handles(&self) -> &[InputStatusHandle] {
        self.inner.input_handles()
    }

    #[inline]
    pub fn membership_descriptor(&self) -> Option<MembershipDescriptor> {
        self.inner.membership_descriptor()
    }

    #[inline]
    pub fn last_block_metadata(&self) -> Option<MixedBlockMetadata> {
        self.inner.last_block_metadata()
    }

    #[inline]
    pub fn session_id(&self) -> u64 {
        self.inner.session_id()
    }

    #[inline]
    pub fn segment_id(&self) -> u64 {
        self.inner.segment_id()
    }

    #[inline]
    pub fn on_format_change(&self) -> OnFormatChange {
        self.inner.on_format_change()
    }

    #[inline]
    pub fn route_guard(&self) -> Option<&RouteRollbackGuard> {
        self.inner.route_guard()
    }

    #[inline]
    pub fn update_targets(
        &mut self,
        new_targets: Vec<CaptureTarget>,
    ) -> Result<u64, CapabilityError> {
        self.inner.update_targets(new_targets)
    }

    #[inline]
    pub fn notify_default_device_changed(
        &mut self,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        self.inner.notify_default_device_changed(new_device_id)
    }

    #[inline]
    pub fn notify_default_input_changed(
        &mut self,
        new_device_id: Option<&str>,
    ) -> Result<bool, CapabilityError> {
        self.inner.notify_default_input_changed(new_device_id)
    }

    #[inline]
    pub fn reopen_relaunched_target(
        &mut self,
        target_index: usize,
        new_pid: u32,
    ) -> Result<(), CapabilityError> {
        self.inner.reopen_relaunched_target(target_index, new_pid)
    }

    #[inline]
    pub fn default_cancel(&self) -> Cancellation {
        self.inner.default_cancel()
    }

    #[inline]
    pub fn read(&mut self, frames: usize, timeout: Duration) -> Result<ReadResult, ReadError> {
        self.inner.read(frames, timeout)
    }

    #[inline]
    pub fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.inner.read_with_cancel(frames, timeout, cancel)
    }

    #[inline]
    pub fn read_into(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
    ) -> Result<ReadResult, ReadError> {
        self.inner.read_into(destination, frames, timeout)
    }

    #[inline]
    pub fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.inner
            .read_into_with_cancel(destination, frames, timeout, cancel)
    }

    #[inline]
    pub fn stop(&mut self, mode: Stop) {
        self.inner.stop(mode);
    }

    #[inline]
    pub fn wait_cleanup(&mut self, timeout: Duration) -> bool {
        self.inner.wait_cleanup(timeout)
    }

    #[inline]
    pub fn resources_done(&self) -> bool {
        self.inner.resources_done()
    }
}

/// Event-driven OS default endpoint observer, independent of reads.
pub trait DefaultEndpointObserver: Send + Sync + Sized {
    fn acquire() -> Self;
    fn latest_endpoint_id(&self) -> Option<String>;
    /// Allocation-free check returning `Some(new_id)` **only** when the live OS default endpoint
    /// differs from `expected_id` (`0` heap allocations on steady-state `Follow` polls).
    fn changed_from(&self, expected_id: Option<&str>) -> Option<String>;
}

/// `NativeTreeMix` / `SharedNativeGraphMix` graph topology controller.
pub trait TopologyControllerBackend: Send + Sync {
    fn targets(&self) -> &[CaptureTarget];
    fn active_topology_link_count(&self) -> usize;
    fn retire_target(&self, target_index: usize) -> Vec<Arc<str>>;
    fn shutdown(&self);
}

/// Recording-permissions inspection and interactive request contract.
pub trait PermissionsBackend: Send + Sync + Sized {
    /// Non-prompting permission check for `targets` (empty slice checks all OS-relevant audio
    /// capture domains) honoring `backends`. Never triggers OS consent dialogs or opens Settings.
    fn check_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError>;

    /// Request OS recording permissions required by `targets` (empty slice requests all promptable
    /// OS-relevant audio capture domains). Opens OS privacy settings only when
    /// `open_settings_if_denied == true` and a required domain is `Denied` in an interactive session.
    fn request_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError>;

    /// Non-prompting permission check for explicit `PermissionKind` domains.
    fn check_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError>;

    /// Request OS recording permissions for explicit `PermissionKind` domains.
    fn request_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError>;
}

/// Static-dispatch permissions wrapper.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Permissions<B: PermissionsBackend> {
    _marker: std::marker::PhantomData<B>,
}

impl<B: PermissionsBackend> Permissions<B> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }

    #[inline]
    pub fn check(
        targets: &[CaptureTarget],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        B::check_permissions(targets, backends)
    }

    #[inline]
    pub fn check_request(request: &CaptureRequest) -> Result<PermissionReport, CapabilityError> {
        let kinds_set = request.required_permission_kinds_for_os(PlatformOs::current())?;
        let kinds: Vec<PermissionKind> = kinds_set.into_iter().collect();
        B::check_permission_kinds(&kinds, request.backends())
    }

    #[inline]
    pub fn check_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        B::check_permission_kinds(kinds, backends)
    }

    #[inline]
    pub fn request(
        targets: &[CaptureTarget],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        B::request_permissions(targets, backends, timeout, open_settings_if_denied)
    }

    #[inline]
    pub fn request_for_capture_request(
        request: &CaptureRequest,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        let kinds_set = request.required_permission_kinds_for_os(PlatformOs::current())?;
        let kinds: Vec<PermissionKind> = kinds_set.into_iter().collect();
        B::request_permission_kinds(&kinds, request.backends(), timeout, open_settings_if_denied)
    }

    #[inline]
    pub fn request_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        B::request_permission_kinds(kinds, backends, timeout, open_settings_if_denied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        StorageLimits,
        discovery::{
            BackendRawInventory, DeviceDirection, RawBackendDeviceRecord, RawBackendProcessRecord,
            RawProcessRecord,
        },
        manager::{MultiInputManager, default_block_frames_for_rate},
        ordering::{Producer, channel},
        reads::{PartialReason, ReadStream},
        schema::{
            ByteOrder, Channel, DefaultDeviceMode, Header, Layout, ProcessIdentity, SampleType,
        },
        storage::Pool,
        stream_runtime::{ReadySignal, StreamReadContext, poll_read_stream, poll_read_stream_into},
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    };

