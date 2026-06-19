#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("capture-system-or-process-audio supports Windows, Linux, and macOS only");

#[cfg(windows)]
use capture_system_or_process_audio_windows as sys;

#[cfg(target_os = "linux")]
use capture_system_or_process_audio_linux as sys;

#[cfg(target_os = "macos")]
use capture_system_or_process_audio_macos as sys;

pub use capture_system_or_process_audio_core as core;
pub use core::{
    CapacityError, StorageLimits, converter, discovery, lifecycle, manager, measurements, mixer,
    ordering, planner, reads, routing, schema, storage, stream_runtime, traits,
};

pub use core::discovery::{
    DiscoveryDedupe, DiscoveryEvent, DiscoveryEventKind, DiscoveryFilter, DiscoveryOrder,
    DiscoveryQuery, DiscoverySubscription, FindAppError, TargetCatalog, TargetSnapshot,
};
pub use core::manager::{BoundaryKind, InputStatusHandle, SessionBoundary, StorageBudget};
pub use core::mixer::InputDiagnosticSnapshot;
pub use core::ordering::{Control, ControlHandle, ControlKind, Failure, Losses, Overflow, Stop};
pub use core::planner::{
    CapturePlanReport, CaptureRequest, OverlapCombinationSemantics, TargetPlan,
};
pub use core::reads::{
    Cancellation, Mode, PacketAnchor, PartialReason, ReadError, ReadResult, Slice,
};
pub use core::routing::{OwnedRoute, RouteRollbackGuard};
pub use core::schema::{
    AggregateManagerBudgets, BackendId, Backends, ByteOrder, CaptureTarget, Channel, CombineMode,
    CombineResolution, DefaultDeviceMode, DitherMode, Format, Header, InputConversionPolicy,
    Layout, MembershipDescriptor, MixedBlockMetadata, MixerConfig, OnFormatChange, OnInputFailure,
    OnRootExit, OutputConverterConfig, ParticipationState, PermissionEntry, PermissionKind,
    PermissionPolicy, PermissionReport, PermissionState, PlanError, PlatformOs, ProcessIdentity,
    ProcessScope, ProcessTreeRange, ProcessTreeStrategy, Provenance, SampleType, Stamp, TargetKind,
};
pub use core::stream_runtime::{EndReason, StartupCleanup};
pub use core::traits::{
    AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
    DiscoveryBackend, OpenableStreamBackend, PermissionsBackend, TimingRequirement,
    TopologyControllerBackend,
};
use std::time::Duration;

/// Active OS single-point stream backend (`PlatformStream`).
pub type PlatformStream = sys::PlatformStream;

/// Active OS discovery backend (`PlatformDiscovery`).
pub type PlatformDiscovery = sys::PlatformDiscovery;

/// Active OS multi-target / pass-through capture manager backend (`PlatformManager`).
pub type PlatformManager = sys::PlatformManager;

/// Active OS recording-permissions check & request backend (`PlatformPermissions`).
pub type PlatformPermissions = sys::PlatformPermissions;

/// Active OS event-driven default output endpoint observer.
pub type DefaultOutputObserver = sys::PlatformDefaultOutputObserver;

/// Active OS event-driven default input (microphone) endpoint observer.
pub type DefaultInputObserver = sys::PlatformDefaultInputObserver;

/// Unified zero-cost static-dispatch cross-platform `AudioStream`.
pub type AudioStream = core::traits::AudioStream<PlatformStream>;

/// Unified zero-cost static-dispatch cross-platform `Discovery`.
pub type Discovery = core::traits::Discovery<PlatformDiscovery>;

/// Unified zero-cost static-dispatch cross-platform `CaptureManager`.
pub type CaptureManager = core::traits::CaptureManager<PlatformManager>;

/// Unified zero-cost static-dispatch cross-platform `Permissions`.
pub type Permissions = core::traits::Permissions<PlatformPermissions>;

/// Discover available targets matching `filter` without starting a background watcher thread.
#[inline]
pub fn discover(filter: DiscoveryFilter) -> Result<TargetSnapshot, CapabilityError> {
    Discovery::new().discover(filter)
}

/// Execute a structured `DiscoveryQuery` (`backends`, `dedupe`, `order`) without starting a watcher thread.
#[inline]
pub fn discover_query(query: &DiscoveryQuery) -> Result<TargetCatalog, CapabilityError> {
    Discovery::new().discover_query(query)
}

/// Subscribe to bounded `DiscoveryEvent` updates matching `query` (`max_discovery_queue` in `32..=4096`).
#[inline]
pub fn subscribe_discovery(
    query: DiscoveryQuery,
    max_discovery_queue: usize,
) -> Result<DiscoverySubscription, CapabilityError> {
    Discovery::new().subscribe_query(query, max_discovery_queue)
}

/// Construct a `CaptureRequest` for `targets` ready for `.inspect()` or `CaptureManager::open(...)`.
#[inline]
pub fn request(targets: Vec<CaptureTarget>) -> CaptureRequest {
    CaptureRequest::new(targets)
}

/// Open a `CaptureManager` session (`PassThrough`, `SharedNativeGraphMix`, or `CspaMixer`) for `targets`.
#[inline]
pub fn capture(targets: Vec<CaptureTarget>) -> Result<CaptureManager, CapabilityError> {
    CaptureManager::open(CaptureRequest::new(targets))
}

/// Open a `CaptureManager` session for a single `CaptureTarget`.
#[inline]
pub fn capture_single(target: CaptureTarget) -> Result<CaptureManager, CapabilityError> {
    CaptureManager::open_single(target)
}

/// Open a single-point `AudioStream` without dynamic process-tree or default-device monitoring.
#[inline]
pub fn open_stream(
    target: &CaptureTarget,
    overflow: Overflow,
) -> Result<AudioStream, CapabilityError> {
    AudioStream::open(target, overflow)
}

/// Open a single-point `AudioStream` with explicit `ProcessTreeRange`, `Backends`, `Overflow`, and `TimingRequirement`.
#[inline]
pub fn open_stream_with_options(
    target: &CaptureTarget,
    tree_range: ProcessTreeRange,
    backends: &Backends,
    overflow: Overflow,
    timing: TimingRequirement,
) -> Result<AudioStream, CapabilityError> {
    AudioStream::open_with_options(target, tree_range, backends, overflow, timing)
}

/// Open a single-point `AudioStream` with explicit `PermissionPolicy` (`AutoRequest` vs `CheckOnly`).
#[inline]
pub fn open_stream_with_permission_policy(
    target: &CaptureTarget,
    tree_range: ProcessTreeRange,
    backends: &Backends,
    overflow: Overflow,
    timing: TimingRequirement,
    permission_policy: PermissionPolicy,
) -> Result<AudioStream, CapabilityError> {
    AudioStream::open_with_permission_policy(
        target,
        tree_range,
        backends,
        overflow,
        timing,
        permission_policy,
    )
}

/// Non-prompting OS recording permission check for `targets` (empty slice inspects all OS domains).
#[inline]
pub fn check_permissions(targets: &[CaptureTarget]) -> Result<PermissionReport, CapabilityError> {
    Permissions::check(targets, &Backends::default())
}

