#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]

pub mod records;

use capture_system_or_process_audio::{
    AudioStream, Backends, Cancellation, CapabilityError, CaptureManager, CaptureTarget,
    DefaultDeviceMode, Discovery, DiscoveryEventKind, FindAppError, InputStatusHandle, OnRootExit,
    PartialReason, PlanError, PlatformOs, ProcessIdentity, ProcessScope, ProcessTreeRange,
    ReadError, ReadResult, Stop, TimingRequirement, check_permissions_with_backends,
    check_request_permissions, core::manager::Visibility, discover_query,
    request_for_capture_request, request_permissions_with_options, storage::OwnedBlock,
};
use records::{
    AggregateBudgetsRecord, BackendsRecord, CaptureTargetRecord, CspaBlockRecord,
    CspaCaptureRequestRecord, CspaDiscoveryEventRecord, CspaDiscoveryQueryRecord, CspaHeaderRecord,
    CspaInputStatusRecord, CspaManagedReadOutcomeRecord, CspaMixerConfigRecord,
    CspaPcmFormatRecord, CspaPermissionReportRecord, CspaPlanReportRecord,
    MixedBlockMetadataRecord, ProcessTreeRangeRecord, decode_backend_opt, decode_overflow,
    decode_permission_policy, decode_platform_os, encode_backend, encode_sample_type,
    write_truncating_cstr,
};
use std::{
    ffi::{CStr, c_char, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub const CSPA_EXPERIMENTAL_ABI_MAJOR: u32 = 0;
pub const CSPA_EXPERIMENTAL_SCHEMA_REVISION: u32 = 1;

pub const CSPA_STATUS_OK: u32 = 0;
pub const CSPA_STATUS_INVALID_ARGUMENT: u32 = 1;
pub const CSPA_STATUS_ABI_MISMATCH: u32 = 2;
pub const CSPA_STATUS_UNSUPPORTED_SCOPE: u32 = 3;
pub const CSPA_STATUS_PRECISE_TIMING_UNQUALIFIED: u32 = 4;
pub const CSPA_STATUS_ALREADY_PENDING: u32 = 5;
pub const CSPA_STATUS_NO_MEMORY: u32 = 6;
pub const CSPA_STATUS_CALLBACK_FAILURE: u32 = 7;
pub const CSPA_STATUS_FORMAT_CHANGED: u32 = 8;
pub const CSPA_STATUS_INTERNAL_ERROR: u32 = 9;
pub const CSPA_STATUS_PERMISSION_DENIED: u32 = 10;
pub const CSPA_STATUS_DEVICE_UNAVAILABLE: u32 = 11;
pub const CSPA_STATUS_BACKEND_UNAVAILABLE: u32 = 12;
pub const CSPA_STATUS_NO_SUCH_PROCESS: u32 = 13;
pub const CSPA_STATUS_PID_REUSED: u32 = 14;
pub const CSPA_STATUS_REROUTE_NOT_PERMITTED: u32 = 15;
pub const CSPA_STATUS_UNSUPPORTED_FORMAT: u32 = 16;
pub const CSPA_STATUS_CLEANUP_TIMEOUT: u32 = 17;
pub const CSPA_STATUS_STOPPED: u32 = 18;

pub const CSPA_PLATFORM_OS_WINDOWS: u32 = 1;
pub const CSPA_PLATFORM_OS_LINUX: u32 = 2;
pub const CSPA_PLATFORM_OS_MACOS: u32 = 3;

pub const CSPA_TARGET_KIND_PROCESS: u32 = 1;
pub const CSPA_TARGET_KIND_APPLICATION: u32 = 2;
pub const CSPA_TARGET_KIND_DEVICE: u32 = 3;
pub const CSPA_TARGET_KIND_DEFAULT_OUTPUT: u32 = 4;
pub const CSPA_TARGET_KIND_INPUT_DEVICE: u32 = 5;
pub const CSPA_TARGET_KIND_DEFAULT_INPUT: u32 = 6;

pub const CSPA_SCOPE_TREE: u32 = 1;
pub const CSPA_SCOPE_EXACT: u32 = 2;

pub const CSPA_ON_ROOT_EXIT_STOP: u32 = 1;
pub const CSPA_ON_ROOT_EXIT_WAIT: u32 = 2;

pub const CSPA_DEFAULT_DEVICE_FOLLOW: u32 = 1;
pub const CSPA_DEFAULT_DEVICE_PIN: u32 = 2;

pub const CSPA_BACKEND_NONE: u32 = 0;
pub const CSPA_BACKEND_WASAPI: u32 = 1;
pub const CSPA_BACKEND_PIPEWIRE: u32 = 2;
pub const CSPA_BACKEND_PULSEAUDIO: u32 = 3;
pub const CSPA_BACKEND_COREAUDIO: u32 = 4;
pub const CSPA_BACKEND_SCREENCAPTUREKIT: u32 = 5;

pub const CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE: u32 = 1;
pub const CSPA_TREE_STRATEGY_NATIVE_GRAPH_MIX: u32 = 2;
pub const CSPA_TREE_STRATEGY_SOFTWARE_STREAM_MIX: u32 = 3;

pub const CSPA_COMBINE_MODE_AUTO: u32 = 1;
pub const CSPA_COMBINE_MODE_NATIVE_ONLY: u32 = 2;
pub const CSPA_COMBINE_MODE_CSPA_MIXER: u32 = 3;

pub const CSPA_COMBINE_RESOLUTION_PASS_THROUGH: u32 = 1;
pub const CSPA_COMBINE_RESOLUTION_SHARED_NATIVE_GRAPH_MIX: u32 = 2;
pub const CSPA_COMBINE_RESOLUTION_CSPA_MIXER: u32 = 3;

pub const CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING: u32 = 1;
pub const CSPA_INPUT_CONVERSION_STRICT_NO_RESAMPLE: u32 = 2;

pub const CSPA_ON_FORMAT_CHANGE_ADAPT: u32 = 1;
pub const CSPA_ON_FORMAT_CHANGE_FAIL: u32 = 2;

pub const CSPA_ON_INPUT_FAILURE_DEGRADE: u32 = 1;
pub const CSPA_ON_INPUT_FAILURE_FAIL: u32 = 2;

pub const CSPA_SAMPLE_FORMAT_I16: u32 = 1;
pub const CSPA_SAMPLE_FORMAT_I24: u32 = 2;
pub const CSPA_SAMPLE_FORMAT_F32: u32 = 3;
pub const CSPA_SAMPLE_FORMAT_I32: u32 = 4;

pub const CSPA_BYTE_ORDER_LITTLE: u32 = 1;
pub const CSPA_BYTE_ORDER_BIG: u32 = 2;

pub const CSPA_DITHER_TPDF: u32 = 1;
pub const CSPA_DITHER_NONE: u32 = 2;

pub const CSPA_OVERFLOW_DROP_OLDEST: u32 = 1;
pub const CSPA_OVERFLOW_DROP_NEWEST: u32 = 2;
pub const CSPA_OVERFLOW_FAIL: u32 = 3;

pub const CSPA_DISCOVERY_DEDUPE_BY_LOGICAL_TARGET: u32 = 1;
pub const CSPA_DISCOVERY_DEDUPE_PER_BACKEND: u32 = 2;

pub const CSPA_DISCOVERY_ORDER_KIND_THEN_BACKEND: u32 = 1;
pub const CSPA_DISCOVERY_ORDER_BACKEND_THEN_KIND: u32 = 2;

pub const CSPA_DISCOVERY_EVENT_ADDED: u32 = 1;
pub const CSPA_DISCOVERY_EVENT_REMOVED: u32 = 2;
pub const CSPA_DISCOVERY_EVENT_STATE_CHANGED: u32 = 3;
pub const CSPA_DISCOVERY_EVENT_DEFAULT_DEVICE_CHANGED: u32 = 4;
pub const CSPA_DISCOVERY_EVENT_DEFAULT_INPUT_CHANGED: u32 = 5;
pub const CSPA_DISCOVERY_EVENT_WATCHER_FAILED: u32 = 6;
pub const CSPA_DISCOVERY_EVENT_VISIBILITY_CHANGED: u32 = 7;
pub const CSPA_DISCOVERY_EVENT_RESNAPSHOT_REQUIRED: u32 = 8;

pub const CSPA_OVERLAP_SINGLE_TARGET: u32 = 1;
pub const CSPA_OVERLAP_SOFTWARE_SUMMATION: u32 = 2;
pub const CSPA_OVERLAP_NATIVE_SET_UNION: u32 = 3;

pub const CSPA_ACQUISITION_DIRECT_ENDPOINT: u32 = 1;
pub const CSPA_ACQUISITION_PROCESS_TREE: u32 = 2;

pub const CSPA_PARTICIPATION_STARTING: u32 = 1;
pub const CSPA_PARTICIPATION_ACTIVE: u32 = 2;
pub const CSPA_PARTICIPATION_IDLE: u32 = 3;
pub const CSPA_PARTICIPATION_WAITING_RELAUNCH: u32 = 4;
pub const CSPA_PARTICIPATION_LATE: u32 = 5;
pub const CSPA_PARTICIPATION_EXITED: u32 = 6;
pub const CSPA_PARTICIPATION_FAILED: u32 = 7;

pub const CSPA_PARTIAL_REASON_COMPLETE: u32 = 1;
pub const CSPA_PARTIAL_REASON_WOULD_BLOCK: u32 = 2;
pub const CSPA_PARTIAL_REASON_BOUNDARY: u32 = 3;
pub const CSPA_PARTIAL_REASON_CANCELLED: u32 = 4;
pub const CSPA_PARTIAL_REASON_TIMEOUT: u32 = 5;
pub const CSPA_PARTIAL_REASON_EOF: u32 = 6;
pub const CSPA_PARTIAL_REASON_FAILURE: u32 = 7;
pub const CSPA_PARTIAL_REASON_READY: u32 = 8;

pub const CSPA_BOUNDARY_NONE: u32 = 0;
pub const CSPA_BOUNDARY_SESSION_CHANGED: u32 = 1;
pub const CSPA_BOUNDARY_CONFIGURATION_CHANGED: u32 = 2;
pub const CSPA_BOUNDARY_DEVICE_CHANGED: u32 = 3;
pub const CSPA_BOUNDARY_FORMAT_CHANGED: u32 = 4;
pub const CSPA_BOUNDARY_GAP: u32 = 5;

pub const CSPA_ACTIVE_AUDIO_UNKNOWN: u32 = 0;
pub const CSPA_ACTIVE_AUDIO_INACTIVE: u32 = 1;
pub const CSPA_ACTIVE_AUDIO_ACTIVE: u32 = 2;

pub const CSPA_PERMISSION_KIND_COUNT: usize = 3;
pub const CSPA_PERMISSION_KIND_MICROPHONE: u32 = 1;
pub const CSPA_PERMISSION_KIND_SYSTEM_AUDIO: u32 = 2;
pub const CSPA_PERMISSION_KIND_SCREEN_RECORDING: u32 = 3;

pub const CSPA_PERMISSION_STATE_GRANTED: u32 = 1;
pub const CSPA_PERMISSION_STATE_NOT_DETERMINED: u32 = 2;
pub const CSPA_PERMISSION_STATE_DENIED: u32 = 3;
pub const CSPA_PERMISSION_STATE_RESTRICTED: u32 = 4;
pub const CSPA_PERMISSION_STATE_UNKNOWN: u32 = 5;

pub const CSPA_PERMISSION_POLICY_AUTO_REQUEST: u32 = 1;
pub const CSPA_PERMISSION_POLICY_CHECK_ONLY: u32 = 2;

pub type CspaManagedCallbackFn = Option<
    unsafe extern "C" fn(
        outcome: *const CspaManagedReadOutcomeRecord,
        user_data: *mut c_void,
    ) -> u32,
>;
pub type CspaContextReleaseFn = Option<unsafe extern "C" fn(user_data: *mut c_void)>;

pub struct CspaOwnerHandle {
    block: OwnedBlock,
    first: usize,
    frames: usize,
}

impl CspaOwnerHandle {
    #[inline]
    fn slice_bytes(&self) -> &[u8] {
        let stride = self.block.format().bytes_per_frame();
        &self.block.bytes()[self.first * stride..(self.first + self.frames) * stride]
    }
}

pub struct CspaCancelHandle {
    inner: Cancellation,
}

pub struct CspaDiscoveryHandle {
    inner: Discovery,
}

pub struct CspaDiscoverySubscriptionHandle {
    sub: capture_system_or_process_audio::DiscoverySubscription,
}

pub struct CspaStreamHandle {
    stream: Mutex<AudioStream>,
    default_cancel: Cancellation,
    active_readers: AtomicUsize,
}

struct FfiCallbackControl {
    stop_flag: Arc<AtomicBool>,
    cb_cancel: Cancellation,
    join_handle: Option<JoinHandle<()>>,
    user_data_addr: usize,
    on_context_release: CspaContextReleaseFn,
    released: bool,
}

impl FfiCallbackControl {
    fn stop_join_and_release_context(&mut self) {
        self.stop_flag.store(true, SeqCst);
        self.cb_cancel.cancel();
        let on_same_worker = self
            .join_handle
            .as_ref()
            .is_some_and(|h| h.thread().id() == thread::current().id());
        if !on_same_worker {
            if let Some(handle) = self.join_handle.take() {
                let _ = handle.join();
            }
            if !self.released {
                self.released = true;
                if let Some(release_fn) = self.on_context_release {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        // SAFETY: Worker thread has joined above, so `user_data` is no longer
                        // accessed concurrently and `on_context_release` is invoked at most once.
                        unsafe {
                            release_fn(self.user_data_addr as *mut c_void);
                        }
                    }));
                }
            }
        }
    }
}

impl Drop for FfiCallbackControl {
    fn drop(&mut self) {
        self.stop_join_and_release_context();
    }
}

pub struct CspaManagerHandle {
    manager: Arc<Mutex<CaptureManager>>,
    default_cancel: Cancellation,
    active_readers: Arc<AtomicUsize>,
    callback_active: Arc<AtomicBool>,
    callback_ctrl: Mutex<Option<FfiCallbackControl>>,
    callback_invocations: Arc<AtomicUsize>,
}

impl CspaManagerHandle {
    fn new(manager: CaptureManager) -> Self {
        let default_cancel = manager.default_cancel();
        Self {
            manager: Arc::new(Mutex::new(manager)),
            default_cancel,
            active_readers: Arc::new(AtomicUsize::new(0)),
            callback_active: Arc::new(AtomicBool::new(false)),
            callback_ctrl: Mutex::new(None),
            callback_invocations: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn join_callback_if_safe(&self) {
        if let Ok(mut guard) = self.callback_ctrl.lock()
            && let Some(mut ctrl) = guard.take()
        {
            ctrl.stop_join_and_release_context();
        }
        self.callback_active.store(false, SeqCst);
    }
}

impl Drop for CspaManagerHandle {
    fn drop(&mut self) {
        self.join_callback_if_safe();
    }
}

#[inline]
fn map_capability_error(err: CapabilityError) -> u32 {
    match err {
        CapabilityError::InvalidTarget
        | CapabilityError::ReversedStrategyRange
        | CapabilityError::InvalidBackend => CSPA_STATUS_INVALID_ARGUMENT,
        CapabilityError::UnsupportedScope
        | CapabilityError::UnsupportedTargetKind
        | CapabilityError::TargetNotShareableApplication
        | CapabilityError::NotImplemented => CSPA_STATUS_UNSUPPORTED_SCOPE,
        CapabilityError::PreciseTimingUnqualified => CSPA_STATUS_PRECISE_TIMING_UNQUALIFIED,
        CapabilityError::CapacityExceeded => CSPA_STATUS_NO_MEMORY,
        CapabilityError::FormatChangeRejected => CSPA_STATUS_FORMAT_CHANGED,
        CapabilityError::PermissionDenied => CSPA_STATUS_PERMISSION_DENIED,
        CapabilityError::DeviceUnavailable => CSPA_STATUS_DEVICE_UNAVAILABLE,
        CapabilityError::ServerUnavailable
        | CapabilityError::ClientLibraryUnavailable { .. }
        | CapabilityError::BackendVersionTooOld { .. }
        | CapabilityError::BackendFailure { .. } => CSPA_STATUS_BACKEND_UNAVAILABLE,
        CapabilityError::NoSuchProcess => CSPA_STATUS_NO_SUCH_PROCESS,
        CapabilityError::PidReused | CapabilityError::StaleNativeObject(_) => {
            CSPA_STATUS_PID_REUSED
        }
        CapabilityError::RerouteNotPermitted => CSPA_STATUS_REROUTE_NOT_PERMITTED,
        CapabilityError::UnsupportedFormat => CSPA_STATUS_UNSUPPORTED_FORMAT,
        CapabilityError::CleanupTimeout => CSPA_STATUS_CLEANUP_TIMEOUT,
        CapabilityError::Stopped => CSPA_STATUS_STOPPED,
    }
}

#[inline]
fn map_plan_error(err: PlanError) -> u32 {
    match err {
        PlanError::InvalidTarget | PlanError::ReversedStrategyRange | PlanError::InvalidBackend => {
            CSPA_STATUS_INVALID_ARGUMENT
        }
        PlanError::UnsupportedScope | PlanError::UnsupportedCapturePlan => {
            CSPA_STATUS_UNSUPPORTED_SCOPE
        }
        PlanError::PreciseTimingUnqualified => CSPA_STATUS_PRECISE_TIMING_UNQUALIFIED,
        PlanError::Capacity(_) => CSPA_STATUS_NO_MEMORY,
        PlanError::PermissionDenied => CSPA_STATUS_PERMISSION_DENIED,
        PlanError::BackendUnavailable => CSPA_STATUS_BACKEND_UNAVAILABLE,
        PlanError::NoSuchProcess => CSPA_STATUS_NO_SUCH_PROCESS,
        PlanError::PidReused => CSPA_STATUS_PID_REUSED,
    }
}

#[inline]
fn map_read_error(err: ReadError) -> u32 {
    match err {
        ReadError::AlreadyPending => CSPA_STATUS_ALREADY_PENDING,
        ReadError::InvalidRequest | ReadError::InvalidDestination | ReadError::WrongMode => {
            CSPA_STATUS_INVALID_ARGUMENT
        }
        ReadError::Poisoned => CSPA_STATUS_INTERNAL_ERROR,
    }
}

#[inline]
fn partial_reason_code(reason: PartialReason) -> u32 {
    match reason {
        PartialReason::Complete => CSPA_PARTIAL_REASON_COMPLETE,
        PartialReason::WouldBlock => CSPA_PARTIAL_REASON_WOULD_BLOCK,
        PartialReason::Boundary => CSPA_PARTIAL_REASON_BOUNDARY,
        PartialReason::Cancelled => CSPA_PARTIAL_REASON_CANCELLED,
        PartialReason::Timeout => CSPA_PARTIAL_REASON_TIMEOUT,
        PartialReason::Eof => CSPA_PARTIAL_REASON_EOF,
        PartialReason::Failure(_) => CSPA_PARTIAL_REASON_FAILURE,
        PartialReason::Ready => CSPA_PARTIAL_REASON_READY,
    }
}

unsafe fn opt_cstr_to_str<'a>(ptr: *const c_char, max_len: usize) -> Result<Option<&'a str>, u32> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: Caller passes either null or a valid NUL-terminated C string pointer.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    let bytes = cstr.to_bytes();
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() >= max_len {
        return Err(CSPA_STATUS_INVALID_ARGUMENT);
    }
    let s = cstr.to_str().map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
    Ok(Some(s))
}

unsafe fn write_target_slice(
    targets: &[CaptureTarget],
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    if out_count.is_null() {
        return CSPA_STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: `out_count` was checked non-null above.
    unsafe {
        *out_count = targets.len();
    }
    if !out_targets.is_null() && capacity > 0 {
        // SAFETY: `out_targets` is non-null and caller declared `capacity` contiguous elements.
        let dst = unsafe { std::slice::from_raw_parts_mut(out_targets, capacity) };
        let to_copy = dst.len().min(targets.len());
        for (slot, target) in dst[..to_copy].iter_mut().zip(targets.iter()) {
            if (slot.struct_size != 0 || slot.schema_revision != 0) && slot.check_abi().is_err() {
                return CSPA_STATUS_ABI_MISMATCH;
            }
            *slot = CaptureTargetRecord::from(target);
        }
    }
    CSPA_STATUS_OK
}

fn populate_owned_outcome_record(
    session: u64,
    segment: u64,
    mixed_meta: Option<capture_system_or_process_audio::MixedBlockMetadata>,
    read_res: ReadResult,
    out_outcome: &mut CspaManagedReadOutcomeRecord,
) {
    let boundary_kind = if read_res.reason == PartialReason::Boundary {
        CSPA_BOUNDARY_DEVICE_CHANGED
    } else {
        CSPA_BOUNDARY_NONE
    };
    let block_rec = if let Some(first_slice) = read_res.chunks.first() {
        let hdr = first_slice.block.header();
        let owner_box = Box::new(CspaOwnerHandle {
            block: first_slice.block.view(),
            first: first_slice.first,
            frames: first_slice.frames,
        });
        let slice_bytes = owner_box.slice_bytes();
        let bytes_ptr = slice_bytes.as_ptr();
        let byte_len = slice_bytes.len() as u64;
        let owner_raw = Box::into_raw(owner_box);
        CspaBlockRecord {
            header: CspaHeaderRecord::from_header(hdr, first_slice.frames),
            bytes: bytes_ptr,
            byte_length: byte_len,
            retained_owner: owner_raw,
        }
    } else {
        CspaBlockRecord {
            header: CspaHeaderRecord {
                segment,
                generation: 1,
                ..CspaHeaderRecord::default()
            },
            bytes: ptr::null(),
            byte_length: 0,
            retained_owner: ptr::null_mut(),
        }
    };

    out_outcome.partial_reason = partial_reason_code(read_res.reason);
    out_outcome.boundary_kind = boundary_kind;
    out_outcome.has_mixed_metadata = u32::from(mixed_meta.is_some());
    out_outcome.reserved = 0;
    out_outcome.session = session;
    out_outcome.segment = segment;
    out_outcome.actual_frames = read_res.frames as u64;
    out_outcome.copy_bytes = read_res.copies.bytes;
    out_outcome.copy_destinations = read_res.copies.destinations;
    out_outcome.block = block_rec;
    out_outcome.mixed_metadata = MixedBlockMetadataRecord::from(mixed_meta.unwrap_or_default());
}

/// Zero-allocation outcome population for `cspa_manager_read_into` and `cspa_stream_read_into`.
#[inline]
fn populate_into_outcome_record(
    session: u64,
    segment: u64,
    mixed_meta: Option<capture_system_or_process_audio::MixedBlockMetadata>,
    read_res: ReadResult,
    destination_ptr: *const u8,
    out_outcome: &mut CspaManagedReadOutcomeRecord,
) {
    let boundary_kind = if read_res.reason == PartialReason::Boundary {
        CSPA_BOUNDARY_DEVICE_CHANGED
    } else {
        CSPA_BOUNDARY_NONE
    };
    let anchor = read_res.last_packet.or(read_res.first_packet);
    let block_rec = if read_res.frames > 0 {
        let header = match anchor {
            Some(pkt) => CspaHeaderRecord::from_header(pkt.header, read_res.frames),
            None => CspaHeaderRecord {
                sequence: segment,
                segment,
                generation: 1,
                frames: read_res.frames as u64,
                valid: 1,
                ..CspaHeaderRecord::default()
            },
        };
        CspaBlockRecord {
            header,
            bytes: destination_ptr,
            byte_length: read_res.copies.bytes,
            retained_owner: ptr::null_mut(),
        }
    } else {
        CspaBlockRecord {
            header: CspaHeaderRecord {
                segment,
                generation: 1,
                ..CspaHeaderRecord::default()
            },
            bytes: ptr::null(),
            byte_length: 0,
            retained_owner: ptr::null_mut(),
        }
    };

    out_outcome.partial_reason = partial_reason_code(read_res.reason);
    out_outcome.boundary_kind = boundary_kind;
    out_outcome.has_mixed_metadata = u32::from(mixed_meta.is_some());
    out_outcome.reserved = 0;
    out_outcome.session = session;
    out_outcome.segment = segment;
    out_outcome.actual_frames = read_res.frames as u64;
    out_outcome.copy_bytes = read_res.copies.bytes;
    out_outcome.copy_destinations = read_res.copies.destinations;
    out_outcome.block = block_rec;
    out_outcome.mixed_metadata = MixedBlockMetadataRecord::from(mixed_meta.unwrap_or_default());
}

fn fill_input_status_from_handle(h: &InputStatusHandle, out: &mut CspaInputStatusRecord) {
    let snap = h.snapshot();
    out.target_index = h.target_index() as u32;
    out.backend = encode_backend(h.backend());
    let (state_code, fail_code) =
        snap.as_ref().map_or((CSPA_PARTICIPATION_STARTING, 0), |s| {
            match s.participation_state {
                capture_system_or_process_audio::ParticipationState::Starting => {
                    (CSPA_PARTICIPATION_STARTING, 0)
                }
                capture_system_or_process_audio::ParticipationState::Active => {
                    (CSPA_PARTICIPATION_ACTIVE, 0)
                }
                capture_system_or_process_audio::ParticipationState::Idle => {
                    (CSPA_PARTICIPATION_IDLE, 0)
                }
                capture_system_or_process_audio::ParticipationState::WaitingRelaunch => {
                    (CSPA_PARTICIPATION_WAITING_RELAUNCH, 0)
                }
                capture_system_or_process_audio::ParticipationState::Late => {
                    (CSPA_PARTICIPATION_LATE, 0)
                }
                capture_system_or_process_audio::ParticipationState::Exited => {
                    (CSPA_PARTICIPATION_EXITED, 0)
                }
                capture_system_or_process_audio::ParticipationState::Failed(c) => {
                    (CSPA_PARTICIPATION_FAILED, c)
                }
            }
        });
    out.participation_state = state_code;
    out.failure_code = fail_code;
    out.native_rate_hz = h.native_format().rate();
    out.native_channels = u32::from(h.native_format().channels());
    out.native_sample_format = encode_sample_type(h.native_format().sample());
    out.mixer_rate_hz = h.mixer_format().rate();
    out.mixer_channels = u32::from(h.mixer_format().channels());
    out.is_resampled = u32::from(h.is_resampled());
    out.is_identity_bypass = u32::from(h.is_identity_bypass());
    out.reserved = 0;
    out.queued_packets = snap.as_ref().map_or(0, |s| s.queued_packets as u64);
    out.dropped_frames = snap.as_ref().map_or(0, |s| s.dropped_frames);
    out.clipped_samples = snap.as_ref().map_or(0, |s| s.clipped_samples);
    out.nonfinite_samples = snap.as_ref().map_or(0, |s| s.nonfinite_samples);
    out.drift_slips = snap.as_ref().map_or(0, |s| s.drift_slips);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_abi_check(abi_major: u32, schema_revision: u32) -> u32 {
    catch_unwind(|| {
        if abi_major == CSPA_EXPERIMENTAL_ABI_MAJOR
            && schema_revision == CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            CSPA_STATUS_OK
        } else {
            CSPA_STATUS_ABI_MISMATCH
        }
    })
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_backends_init(out_backends: *mut BackendsRecord) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_backends.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_backends` was checked non-null; caller provides writable storage.
        unsafe {
            *out_backends = BackendsRecord::default();
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_query_init(
    out_query: *mut CspaDiscoveryQueryRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_query.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_query` was checked non-null; caller provides writable storage.
        unsafe {
            *out_query = CspaDiscoveryQueryRecord::default();
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_mixer_config_init(out_config: *mut CspaMixerConfigRecord) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_config.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_config` was checked non-null; caller provides writable storage.
        unsafe {
            *out_config = CspaMixerConfigRecord::default();
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_aggregate_budgets_init(
    out_budgets: *mut AggregateBudgetsRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_budgets.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_budgets` was checked non-null; caller provides writable storage.
        unsafe {
            *out_budgets = AggregateBudgetsRecord::default();
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_capture_request_init(
    out_request: *mut CspaCaptureRequestRecord,
    targets: *const CaptureTargetRecord,
    target_count: u64,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_request.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_request` was checked non-null; caller provides writable storage.
        unsafe {
            *out_request = CspaCaptureRequestRecord {
                struct_size: size_of::<CspaCaptureRequestRecord>() as u32,
                schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
                tree_strategy: ProcessTreeRangeRecord::default(),
                combine_mode: CSPA_COMBINE_MODE_AUTO,
                input_conversion: CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING,
                on_format_change: CSPA_ON_FORMAT_CHANGE_ADAPT,
                allow_reroute: 0,
                require_precise_timing: 0,
                overflow: CSPA_OVERFLOW_DROP_OLDEST,
                permission_policy: CSPA_PERMISSION_POLICY_AUTO_REQUEST,
                reserved: 0,
                targets,
                target_count,
                backends: ptr::null(),
                converter: ptr::null(),
                mixer_config: ptr::null(),
                budgets: ptr::null(),
            };
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_process(
    pid: u32,
    start_time: u64,
    scope: u32,
    on_root_exit: u32,
    wait_timeout_ns: u64,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_target.is_null() || pid == 0 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        let scope_enum = match scope {
            CSPA_SCOPE_TREE => ProcessScope::Tree,
            CSPA_SCOPE_EXACT => ProcessScope::Exact,
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let exit_enum = match (on_root_exit, wait_timeout_ns) {
            (CSPA_ON_ROOT_EXIT_STOP, 0) => OnRootExit::Stop,
            (CSPA_ON_ROOT_EXIT_WAIT, t) if t > 0 => OnRootExit::Wait { wait_timeout_ns: t },
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let base_target = if start_time != 0 {
            match CaptureTarget::process(
                ProcessIdentity {
                    pid,
                    generation: start_time,
                },
                scope_enum,
                exit_enum,
            ) {
                Ok(t) => t,
                Err(_) => return CSPA_STATUS_INVALID_ARGUMENT,
            }
        } else {
            match Discovery::new().process(pid, scope_enum, exit_enum) {
                Ok(t) => t,
                Err(err) => return map_capability_error(err),
            }
        };
        let Ok(target) = base_target.with_gain(gain) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&target);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_application(
    pid: u32,
    start_time: u64,
    app_id: *const c_char,
    name: *const c_char,
    executable_path: *const c_char,
    scope: u32,
    on_root_exit: u32,
    wait_timeout_ns: u64,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_target.is_null() || pid == 0 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        let scope_enum = match scope {
            CSPA_SCOPE_TREE => ProcessScope::Tree,
            CSPA_SCOPE_EXACT => ProcessScope::Exact,
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let exit_enum = match (on_root_exit, wait_timeout_ns) {
            (CSPA_ON_ROOT_EXIT_STOP, 0) => OnRootExit::Stop,
            (CSPA_ON_ROOT_EXIT_WAIT, t) if t > 0 => OnRootExit::Wait { wait_timeout_ns: t },
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: Each pointer must be null or point to a valid NUL-terminated C string.
        let (app_id_opt, name_opt, exe_opt) = unsafe {
            let a = match opt_cstr_to_str(app_id, records::CSPA_APP_ID_MAX_BYTES) {
                Ok(v) => v.map(Arc::<str>::from),
                Err(code) => return code,
            };
            let n = match opt_cstr_to_str(name, records::CSPA_NAME_MAX_BYTES) {
                Ok(v) => v.map(Arc::<str>::from),
                Err(code) => return code,
            };
            let e = match opt_cstr_to_str(executable_path, records::CSPA_EXECUTABLE_PATH_MAX_BYTES)
            {
                Ok(v) => v.map(Arc::<str>::from),
                Err(code) => return code,
            };
            (a, n, e)
        };
        let generation = if start_time != 0 {
            start_time
        } else {
            match Discovery::new().process(pid, scope_enum, exit_enum) {
                Ok(t) => t.process_start_time().unwrap_or(1),
                Err(err) => return map_capability_error(err),
            }
        };
        let Ok(target) = CaptureTarget::application(
            ProcessIdentity { pid, generation },
            app_id_opt,
            name_opt,
            exe_opt,
            scope_enum,
            exit_enum,
        )
        .and_then(|t| t.with_gain(gain)) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&target);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_lower_to_process(
    app_target: *const CaptureTargetRecord,
    out_process_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if app_target.is_null() || out_process_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Read `src` by value first so in-place calls never alias.
        let src = unsafe { *app_target };
        let dst = unsafe { &mut *out_process_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        let target = match src.decode() {
            Ok(t) => t,
            Err(code) => return code,
        };
        let Ok(lowered) = target.lower_to_process() else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&lowered);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_device(
    device_id: *const c_char,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if device_id.is_null() || out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `device_id` was checked non-null above.
        let Some(id_str) =
            (match unsafe { opt_cstr_to_str(device_id, records::CSPA_DEVICE_ID_MAX_BYTES) } {
                Ok(v) => v,
                Err(code) => return code,
            })
        else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        let Ok(target) = CaptureTarget::device(id_str).and_then(|t| t.with_gain(gain)) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&target);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_input_device(
    device_id: *const c_char,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if device_id.is_null() || out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `device_id` was checked non-null above.
        let Some(id_str) =
            (match unsafe { opt_cstr_to_str(device_id, records::CSPA_DEVICE_ID_MAX_BYTES) } {
                Ok(v) => v,
                Err(code) => return code,
            })
        else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        let Ok(target) = CaptureTarget::input_device(id_str).and_then(|t| t.with_gain(gain)) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&target);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_default_output(
    default_device_mode: u32,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        let mode = match default_device_mode {
            CSPA_DEFAULT_DEVICE_FOLLOW => DefaultDeviceMode::Follow,
            CSPA_DEFAULT_DEVICE_PIN => DefaultDeviceMode::Pin,
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let Ok(target) = CaptureTarget::default_output(mode).with_gain(gain) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&target);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_microphone(
    default_device_mode: u32,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        let mode = match default_device_mode {
            CSPA_DEFAULT_DEVICE_FOLLOW => DefaultDeviceMode::Follow,
            CSPA_DEFAULT_DEVICE_PIN => DefaultDeviceMode::Pin,
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let Ok(target) = CaptureTarget::microphone(mode).with_gain(gain) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        *dst = CaptureTargetRecord::from(&target);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_with_backend(
    target: *const CaptureTargetRecord,
    backend: u32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if target.is_null() || out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Read `src` by value first so `target == out_target` in-place calls never alias.
        let src = unsafe { *target };
        let decoded = match src.decode() {
            Ok(t) => t,
            Err(code) => return code,
        };
        let backend_opt = match decode_backend_opt(backend) {
            Ok(b) => b,
            Err(code) => return code,
        };
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        let updated = decoded.with_optional_backend(backend_opt);
        *dst = CaptureTargetRecord::from(&updated);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_target_with_gain(
    target: *const CaptureTargetRecord,
    gain: f32,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if target.is_null() || out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Read `src` by value first so `target == out_target` in-place calls never alias.
        let src = unsafe { *target };
        let decoded = match src.decode() {
            Ok(t) => t,
            Err(code) => return code,
        };
        let Ok(updated) = decoded.with_gain(gain) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: `out_target` was checked non-null above.
        let dst = unsafe { &mut *out_target };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        *dst = CaptureTargetRecord::from(&updated);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_request_inspect_for_os(
    platform_os: u32,
    request: *const CspaCaptureRequestRecord,
    out_report: *mut CspaPlanReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if request.is_null() || out_report.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let os = match decode_platform_os(platform_os) {
            Ok(os) => os,
            Err(code) => return code,
        };
        // SAFETY: `out_report` and `request` were checked non-null above.
        let (req_ref, report_out) = unsafe { (&*request, &mut *out_report) };
        if let Err(code) = report_out.check_abi() {
            return code;
        }
        // SAFETY: `req_ref` pointers are validated inside `decode`.
        let (decoded_req, _) = match unsafe { req_ref.decode() } {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let report = match decoded_req.inspect_for_os(os) {
            Ok(r) => r,
            Err(err) => return map_plan_error(err),
        };
        // SAFETY: `report_out` and its optional `target_plans` buffer are caller-owned.
        unsafe {
            report_out.populate_from(&report);
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_request_inspect(
    request: *const CspaCaptureRequestRecord,
    out_report: *mut CspaPlanReportRecord,
) -> u32 {
    let os_code = records::encode_platform_os(PlatformOs::current());
    // SAFETY: Forwards exact caller pointers to `cspa_request_inspect_for_os`.
    unsafe { cspa_request_inspect_for_os(os_code, request, out_report) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_permission_report_init(
    out_report: *mut CspaPermissionReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_report.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_report` was checked non-null above.
        unsafe {
            *out_report = CspaPermissionReportRecord::default();
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

unsafe fn decode_optional_target_slice(
    targets: *const CaptureTargetRecord,
    target_count: usize,
) -> Result<Vec<CaptureTarget>, u32> {
    if target_count > 1024 || (target_count > 0 && targets.is_null()) {
        return Err(CSPA_STATUS_INVALID_ARGUMENT);
    }
    if target_count == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: `targets` is non-null and `target_count` is bounded (`1..=1024`).
    let raw = unsafe { std::slice::from_raw_parts(targets, target_count) };
    let mut out = Vec::with_capacity(target_count);
    for rec in raw {
        out.push(rec.decode()?);
    }
    Ok(out)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_permissions_check(
    targets: *const CaptureTargetRecord,
    target_count: usize,
    backends: *const BackendsRecord,
    out_report: *mut CspaPermissionReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_report.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_report` was checked non-null above.
        let dst = unsafe { &mut *out_report };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `targets` and `target_count` are validated inside `decode_optional_target_slice`.
        let decoded_targets = match unsafe { decode_optional_target_slice(targets, target_count) } {
            Ok(v) => v,
            Err(code) => return code,
        };
        let resolved_backends = if backends.is_null() {
            Backends::default()
        } else {
            // SAFETY: `backends` is non-null.
            match unsafe { (&*backends).decode() } {
                Ok(b) => b,
                Err(code) => return code,
            }
        };
        let report = match check_permissions_with_backends(&decoded_targets, &resolved_backends) {
            Ok(r) => r,
            Err(err) => return map_capability_error(err),
        };
        dst.populate_from(&report);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_permissions_request(
    targets: *const CaptureTargetRecord,
    target_count: usize,
    backends: *const BackendsRecord,
    timeout_ms: u64,
    open_settings_if_denied: u32,
    out_report: *mut CspaPermissionReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_report.is_null() || open_settings_if_denied > 1 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_report` was checked non-null above.
        let dst = unsafe { &mut *out_report };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `targets` and `target_count` are validated inside `decode_optional_target_slice`.
        let decoded_targets = match unsafe { decode_optional_target_slice(targets, target_count) } {
            Ok(v) => v,
            Err(code) => return code,
        };
        let resolved_backends = if backends.is_null() {
            Backends::default()
        } else {
            // SAFETY: `backends` is non-null.
            match unsafe { (&*backends).decode() } {
                Ok(b) => b,
                Err(code) => return code,
            }
        };
        let report = match request_permissions_with_options(
            &decoded_targets,
            &resolved_backends,
            Duration::from_millis(timeout_ms.max(1)),
            open_settings_if_denied != 0,
        ) {
            Ok(r) => r,
            Err(err) => return map_capability_error(err),
        };
        dst.populate_from(&report);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_request_check_permissions(
    request: *const CspaCaptureRequestRecord,
    out_report: *mut CspaPermissionReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if request.is_null() || out_report.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let (req_ref, dst) = unsafe { (&*request, &mut *out_report) };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `req_ref` inner pointers are validated inside `decode`.
        let (decoded_req, _) = match unsafe { req_ref.decode() } {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let report = match check_request_permissions(&decoded_req) {
            Ok(r) => r,
            Err(err) => return map_capability_error(err),
        };
        dst.populate_from(&report);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_request_request_permissions(
    request: *const CspaCaptureRequestRecord,
    timeout_ms: u64,
    open_settings_if_denied: u32,
    out_report: *mut CspaPermissionReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if request.is_null() || out_report.is_null() || open_settings_if_denied > 1 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let (req_ref, dst) = unsafe { (&*request, &mut *out_report) };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `req_ref` inner pointers are validated inside `decode`.
        let (decoded_req, _) = match unsafe { req_ref.decode() } {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let report = match request_for_capture_request(
            &decoded_req,
            Duration::from_millis(timeout_ms.max(1)),
            open_settings_if_denied != 0,
        ) {
            Ok(r) => r,
            Err(err) => return map_capability_error(err),
        };
        dst.populate_from(&report);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discover_query(
    query: *const CspaDiscoveryQueryRecord,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if query.is_null() || out_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `query` was checked non-null above.
        let q_rec = unsafe { &*query };
        let dq = match q_rec.decode_for_os(PlatformOs::current()) {
            Ok(dq) => dq,
            Err(code) => return code,
        };
        let catalog = match discover_query(&dq) {
            Ok(c) => c,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `out_targets`, `capacity`, and `out_count` are validated inside `write_target_slice`.
        unsafe { write_target_slice(&catalog.targets, out_targets, capacity, out_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_create(
    out_discovery: *mut *mut CspaDiscoveryHandle,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_discovery.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let handle = Box::new(CspaDiscoveryHandle {
            inner: Discovery::new(),
        });
        // SAFETY: `out_discovery` was checked non-null above.
        unsafe {
            *out_discovery = Box::into_raw(handle);
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_create_with_backends(
    backends: *const BackendsRecord,
    out_discovery: *mut *mut CspaDiscoveryHandle,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_discovery.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let resolved = if backends.is_null() {
            Backends::default()
        } else {
            // SAFETY: `backends` is non-null.
            match unsafe { (&*backends).decode() } {
                Ok(b) => b,
                Err(code) => return code,
            }
        };
        let inner = match Discovery::with_backends(resolved) {
            Ok(d) => d,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `out_discovery` was checked non-null above.
        unsafe {
            *out_discovery = Box::into_raw(Box::new(CspaDiscoveryHandle { inner }));
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_release(discovery: *mut CspaDiscoveryHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !discovery.is_null() {
            // SAFETY: `discovery` was allocated by `Box::into_raw` in `cspa_discovery_create*`.
            unsafe {
                drop(Box::from_raw(discovery));
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_query(
    discovery: *const CspaDiscoveryHandle,
    query: *const CspaDiscoveryQueryRecord,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || query.is_null() || out_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` and `query` were checked non-null above.
        let (disc, q_rec) = unsafe { (&(*discovery).inner, &*query) };
        let dq = match q_rec.decode_for_os(disc.service().os()) {
            Ok(dq) => dq,
            Err(code) => return code,
        };
        let catalog = match disc.discover_query(&dq) {
            Ok(c) => c,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `write_target_slice` validates `out_targets` and `capacity`.
        unsafe { write_target_slice(&catalog.targets, out_targets, capacity, out_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_applications(
    discovery: *const CspaDiscoveryHandle,
    active_only: u32,
    include_non_audio: u32,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || out_count.is_null() || active_only > 1 || include_non_audio > 1 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` was checked non-null above.
        let disc = unsafe { &(*discovery).inner };
        let apps = match disc.applications(active_only != 0, include_non_audio != 0) {
            Ok(v) => v,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `write_target_slice` validates `out_targets` and `capacity`.
        unsafe { write_target_slice(&apps, out_targets, capacity, out_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_processes(
    discovery: *const CspaDiscoveryHandle,
    active_only: u32,
    include_non_audio: u32,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || out_count.is_null() || active_only > 1 || include_non_audio > 1 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` was checked non-null above.
        let disc = unsafe { &(*discovery).inner };
        let procs = match disc.processes(active_only != 0, include_non_audio != 0) {
            Ok(v) => v,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `write_target_slice` validates `out_targets` and `capacity`.
        unsafe { write_target_slice(&procs, out_targets, capacity, out_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_output_devices(
    discovery: *const CspaDiscoveryHandle,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || out_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` was checked non-null above.
        let disc = unsafe { &(*discovery).inner };
        let devs = match disc.output_devices() {
            Ok(v) => v,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `write_target_slice` validates `out_targets` and `capacity`.
        unsafe { write_target_slice(&devs, out_targets, capacity, out_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_input_devices(
    discovery: *const CspaDiscoveryHandle,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || out_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` was checked non-null above.
        let disc = unsafe { &(*discovery).inner };
        let devs = match disc.input_devices() {
            Ok(v) => v,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `write_target_slice` validates `out_targets` and `capacity`.
        unsafe { write_target_slice(&devs, out_targets, capacity, out_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_find_app(
    discovery: *const CspaDiscoveryHandle,
    query: *const c_char,
    scope: u32,
    on_root_exit: u32,
    wait_timeout_ns: u64,
    out_target: *mut CaptureTargetRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || query.is_null() || out_target.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_target` and `discovery` were checked non-null above.
        let (disc, dst) = unsafe { (&(*discovery).inner, &mut *out_target) };
        if let Err(code) = dst.check_abi() {
            return code;
        }
        // SAFETY: `query` was checked non-null above.
        let Some(q_str) = (match unsafe { opt_cstr_to_str(query, records::CSPA_NAME_MAX_BYTES) } {
            Ok(v) => v,
            Err(code) => return code,
        }) else {
            return CSPA_STATUS_INVALID_ARGUMENT;
        };
        let scope_enum = match scope {
            CSPA_SCOPE_TREE => ProcessScope::Tree,
            CSPA_SCOPE_EXACT => ProcessScope::Exact,
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let exit_enum = match (on_root_exit, wait_timeout_ns) {
            (CSPA_ON_ROOT_EXIT_STOP, 0) => OnRootExit::Stop,
            (CSPA_ON_ROOT_EXIT_WAIT, t) if t > 0 => OnRootExit::Wait { wait_timeout_ns: t },
            _ => return CSPA_STATUS_INVALID_ARGUMENT,
        };
        let found = match disc.find_app(q_str, scope_enum, exit_enum) {
            Ok(t) => t,
            Err(FindAppError::NotFound) => return CSPA_STATUS_NO_SUCH_PROCESS,
            Err(FindAppError::AmbiguousMatch(_) | FindAppError::EmptyQuery) => {
                return CSPA_STATUS_INVALID_ARGUMENT;
            }
        };
        *dst = CaptureTargetRecord::from(&found);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_subscribe_query(
    discovery: *const CspaDiscoveryHandle,
    query: *const CspaDiscoveryQueryRecord,
    max_queue: usize,
    out_subscription: *mut *mut CspaDiscoverySubscriptionHandle,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() || query.is_null() || out_subscription.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` and `query` were checked non-null above.
        let (disc, q_rec) = unsafe { (&(*discovery).inner, &*query) };
        let dq = match q_rec.decode_for_os(disc.service().os()) {
            Ok(dq) => dq,
            Err(code) => return code,
        };
        let sub = match disc.subscribe_query(dq, max_queue) {
            Ok(s) => s,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `out_subscription` was checked non-null above.
        unsafe {
            *out_subscription = Box::into_raw(Box::new(CspaDiscoverySubscriptionHandle { sub }));
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_subscription_is_overflowed(
    subscription: *const CspaDiscoverySubscriptionHandle,
    out_overflowed: *mut u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if subscription.is_null() || out_overflowed.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let sub = unsafe { &(*subscription).sub };
        unsafe {
            *out_overflowed = u32::from(sub.is_overflowed());
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_subscription_poll(
    subscription: *const CspaDiscoverySubscriptionHandle,
    out_event: *mut CspaDiscoveryEventRecord,
    out_has_event: *mut u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if subscription.is_null() || out_event.is_null() || out_has_event.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: All three pointers were checked non-null above.
        let (sub, ev_out) = unsafe { (&(*subscription).sub, &mut *out_event) };
        if let Err(code) = ev_out.check_abi() {
            return code;
        }
        match sub.poll_event() {
            Some(ev) => {
                // SAFETY: `out_has_event` was checked non-null above.
                unsafe {
                    *out_has_event = 1;
                }
                ev_out.revision = ev.revision;
                ev_out.visibility = 0;
                ev_out.minimum_revision = 0;
                ev_out.subject.fill(0);
                ev_out.target = CaptureTargetRecord::default();
                match ev.kind {
                    DiscoveryEventKind::Added(t) => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_ADDED;
                        write_truncating_cstr(&mut ev_out.subject, Some(t.id()));
                        ev_out.target = CaptureTargetRecord::from(&t);
                    }
                    DiscoveryEventKind::Removed(id) => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_REMOVED;
                        write_truncating_cstr(&mut ev_out.subject, Some(&id));
                    }
                    DiscoveryEventKind::StateChanged(t) => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_STATE_CHANGED;
                        write_truncating_cstr(&mut ev_out.subject, Some(t.id()));
                        ev_out.target = CaptureTargetRecord::from(&t);
                    }
                    DiscoveryEventKind::DefaultDeviceChanged { device_id } => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_DEFAULT_DEVICE_CHANGED;
                        write_truncating_cstr(&mut ev_out.subject, Some(&device_id));
                    }
                    DiscoveryEventKind::DefaultInputChanged { device_id } => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_DEFAULT_INPUT_CHANGED;
                        write_truncating_cstr(&mut ev_out.subject, Some(&device_id));
                    }
                    DiscoveryEventKind::WatcherFailed { reason } => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_WATCHER_FAILED;
                        write_truncating_cstr(&mut ev_out.subject, Some(&reason));
                    }
                    DiscoveryEventKind::VisibilityChanged(vis) => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_VISIBILITY_CHANGED;
                        ev_out.visibility = match vis {
                            Visibility::Complete => 1,
                            Visibility::Limited => 2,
                        };
                    }
                    DiscoveryEventKind::ResnapshotRequired { minimum_revision } => {
                        ev_out.kind = CSPA_DISCOVERY_EVENT_RESNAPSHOT_REQUIRED;
                        ev_out.minimum_revision = minimum_revision;
                    }
                }
            }
            None => {
                // SAFETY: `out_has_event` was checked non-null above.
                unsafe {
                    *out_has_event = 0;
                }
            }
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_subscription_resnapshot(
    subscription: *const CspaDiscoverySubscriptionHandle,
    out_targets: *mut CaptureTargetRecord,
    capacity: usize,
    out_revision: *mut u64,
    out_target_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if subscription.is_null() || out_target_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `subscription` was checked non-null above.
        let sub = unsafe { &(*subscription).sub };
        let snap = match sub.resnapshot() {
            Ok(s) => s,
            Err(_) => return CSPA_STATUS_INTERNAL_ERROR,
        };
        if !out_revision.is_null() {
            // SAFETY: `out_revision` is non-null.
            unsafe {
                *out_revision = snap.cursor.revision;
            }
        }
        // SAFETY: `write_target_slice` validates `out_targets` and `capacity`.
        unsafe { write_target_slice(&snap.targets, out_targets, capacity, out_target_count) }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_discovery_subscription_release(
    subscription: *mut CspaDiscoverySubscriptionHandle,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !subscription.is_null() {
            // SAFETY: `subscription` was allocated by `Box::into_raw` in `cspa_discovery_subscribe_query`.
            unsafe {
                drop(Box::from_raw(subscription));
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_cancel_create(out_cancel: *mut *mut CspaCancelHandle) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_cancel.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `out_cancel` was checked non-null above.
        unsafe {
            *out_cancel = Box::into_raw(Box::new(CspaCancelHandle {
                inner: Cancellation::default(),
            }));
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_cancel_trigger(cancel: *mut CspaCancelHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !cancel.is_null() {
            // SAFETY: `cancel` was checked non-null above.
            unsafe {
                (*cancel).inner.cancel();
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_cancel_is_cancelled(cancel: *const CspaCancelHandle) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if cancel.is_null() {
            return 0;
        }
        // SAFETY: `cancel` was checked non-null above.
        u32::from(unsafe { (*cancel).inner.is_cancelled() })
    }))
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_cancel_release(cancel: *mut CspaCancelHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !cancel.is_null() {
            // SAFETY: `cancel` was allocated by `Box::into_raw` in `cspa_cancel_create`.
            unsafe {
                drop(Box::from_raw(cancel));
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_owner_retain(owner: *mut CspaOwnerHandle) -> *mut CspaOwnerHandle {
    catch_unwind(AssertUnwindSafe(|| {
        if owner.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `owner` was checked non-null above.
        let existing = unsafe { &*owner };
        Box::into_raw(Box::new(CspaOwnerHandle {
            block: existing.block.view(),
            first: existing.first,
            frames: existing.frames,
        }))
    }))
    .unwrap_or(ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_owner_release(owner: *mut CspaOwnerHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !owner.is_null() {
            // SAFETY: `owner` was allocated by `Box::into_raw`.
            unsafe {
                drop(Box::from_raw(owner));
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_block_release(block: *mut CspaBlockRecord) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !block.is_null() {
            // SAFETY: `block` was checked non-null above.
            let b = unsafe { &mut *block };
            if !b.retained_owner.is_null() {
                // SAFETY: `retained_owner` was allocated by `Box::into_raw`.
                unsafe {
                    drop(Box::from_raw(b.retained_owner));
                }
                b.retained_owner = ptr::null_mut();
            }
            b.bytes = ptr::null();
            b.byte_length = 0;
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_open(
    target: *const CaptureTargetRecord,
    overflow: u32,
    out_stream: *mut *mut CspaStreamHandle,
) -> u32 {
    // SAFETY: Forwards exact caller pointers with default `CSPA_PERMISSION_POLICY_AUTO_REQUEST`.
    unsafe {
        cspa_stream_open_with_policy(
            target,
            overflow,
            CSPA_PERMISSION_POLICY_AUTO_REQUEST,
            out_stream,
        )
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_open_with_policy(
    target: *const CaptureTargetRecord,
    overflow: u32,
    permission_policy: u32,
    out_stream: *mut *mut CspaStreamHandle,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if target.is_null() || out_stream.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `target` was checked non-null above.
        let decoded = match unsafe { (&*target).decode() } {
            Ok(t) => t,
            Err(code) => return code,
        };
        let overflow_enum = match decode_overflow(overflow) {
            Ok(o) => o,
            Err(code) => return code,
        };
        let policy_enum = match decode_permission_policy(permission_policy) {
            Ok(p) => p,
            Err(code) => return code,
        };
        let stream = match AudioStream::open_with_permission_policy(
            &decoded,
            ProcessTreeRange::default(),
            &Backends::default(),
            overflow_enum,
            TimingRequirement::Raw,
            policy_enum,
        ) {
            Ok(s) => s,
            Err(err) => return map_capability_error(err),
        };
        let default_cancel = stream.default_cancel();
        let handle = Box::new(CspaStreamHandle {
            stream: Mutex::new(stream),
            default_cancel,
            active_readers: AtomicUsize::new(0),
        });
        // SAFETY: `out_stream` was checked non-null above.
        unsafe {
            *out_stream = Box::into_raw(handle);
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_format(
    stream: *const CspaStreamHandle,
    out_format: *mut CspaPcmFormatRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if stream.is_null() || out_format.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let (st_handle, fmt_out) = unsafe { (&*stream, &mut *out_format) };
        if let Err(code) = fmt_out.check_abi() {
            return code;
        }
        let Ok(guard) = st_handle.stream.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        *fmt_out = CspaPcmFormatRecord::from(guard.format().as_ref());
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_read_into(
    stream: *mut CspaStreamHandle,
    destination: *mut u8,
    destination_bytes: usize,
    requested_frames: u64,
    timeout_ms: u64,
    cancel: *mut CspaCancelHandle,
    out_outcome: *mut CspaManagedReadOutcomeRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if stream.is_null()
            || destination.is_null()
            || destination_bytes == 0
            || out_outcome.is_null()
            || requested_frames == 0
        {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `stream` and `out_outcome` were checked non-null above.
        let (st_handle, out_ref) = unsafe { (&*stream, &mut *out_outcome) };
        if let Err(code) = out_ref.check_abi() {
            return code;
        }
        if st_handle
            .active_readers
            .compare_exchange(0, 1, SeqCst, SeqCst)
            .is_err()
        {
            return CSPA_STATUS_ALREADY_PENDING;
        }
        let cancel_tok = if cancel.is_null() {
            st_handle.default_cancel.clone()
        } else {
            // SAFETY: `cancel` is non-null.
            unsafe { (*cancel).inner.clone() }
        };
        // SAFETY: `destination` is non-null and caller declared `destination_bytes` writable bytes.
        let dst_slice = unsafe { std::slice::from_raw_parts_mut(destination, destination_bytes) };
        let res = st_handle
            .stream
            .lock()
            .map_err(|_| CSPA_STATUS_INTERNAL_ERROR)
            .and_then(|mut guard| {
                guard
                    .read_into_with_cancel(
                        dst_slice,
                        requested_frames as usize,
                        Duration::from_millis(timeout_ms),
                        cancel_tok,
                    )
                    .map_err(map_read_error)
            });
        st_handle.active_readers.fetch_sub(1, SeqCst);
        match res {
            Ok(read_res) => {
                populate_into_outcome_record(1, 1, None, read_res, destination, out_ref);
                CSPA_STATUS_OK
            }
            Err(code) => code,
        }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_stop(stream: *mut CspaStreamHandle, abort_mode: u32) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if stream.is_null() || abort_mode > 1 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let mode = if abort_mode != 0 {
            Stop::Abort
        } else {
            Stop::Drain
        };
        // SAFETY: `stream` was checked non-null above.
        let st_handle = unsafe { &*stream };
        let Ok(guard) = st_handle.stream.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        guard.stop(mode);
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_wait_cleanup(
    stream: *mut CspaStreamHandle,
    timeout_ms: u64,
    out_cleaned: *mut u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if stream.is_null() || out_cleaned.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let st_handle = unsafe { &*stream };
        let Ok(guard) = st_handle.stream.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        let cleaned = guard.wait_cleanup(Duration::from_millis(timeout_ms.max(1)));
        unsafe {
            *out_cleaned = u32::from(cleaned);
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_stream_release(stream: *mut CspaStreamHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !stream.is_null() {
            // SAFETY: `stream` was allocated by `Box::into_raw` in `cspa_stream_open`.
            unsafe {
                let handle = Box::from_raw(stream);
                if let Ok(guard) = handle.stream.lock() {
                    guard.stop(Stop::Abort);
                }
                drop(handle);
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_open(
    request: *const CspaCaptureRequestRecord,
    out_manager: *mut *mut CspaManagerHandle,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if request.is_null() || out_manager.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `request` was checked non-null above; inner pointers are validated inside `decode`.
        let (decoded_req, overflow) = match unsafe { (&*request).decode() } {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let timing = if decoded_req.require_precise_timing() {
            capture_system_or_process_audio::TimingRequirement::Precise
        } else {
            capture_system_or_process_audio::TimingRequirement::Raw
        };
        let mgr = match CaptureManager::open_with_options(decoded_req, overflow, timing) {
            Ok(m) => m,
            Err(err) => return map_capability_error(err),
        };
        // SAFETY: `out_manager` was checked non-null above.
        unsafe {
            *out_manager = Box::into_raw(Box::new(CspaManagerHandle::new(mgr)));
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_plan(
    manager: *const CspaManagerHandle,
    out_report: *mut CspaPlanReportRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_report.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let (mgr_handle, report_out) = unsafe { (&*manager, &mut *out_report) };
        if let Err(code) = report_out.check_abi() {
            return code;
        }
        let Ok(guard) = mgr_handle.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        // SAFETY: `report_out` and its optional `target_plans` buffer are caller-owned.
        unsafe {
            report_out.populate_from(guard.plan());
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_output_format(
    manager: *const CspaManagerHandle,
    out_format: *mut CspaPcmFormatRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_format.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let (mgr_handle, fmt_out) = unsafe { (&*manager, &mut *out_format) };
        if let Err(code) = fmt_out.check_abi() {
            return code;
        }
        let Ok(guard) = mgr_handle.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        let Some(fmt) = guard.output_format() else {
            return CSPA_STATUS_STOPPED;
        };
        *fmt_out = CspaPcmFormatRecord::from(fmt.as_ref());
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_working_format(
    manager: *const CspaManagerHandle,
    out_format: *mut CspaPcmFormatRecord,
    out_has_working_format: *mut u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_format.is_null() || out_has_working_format.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: All pointers were checked non-null above.
        let (mgr_handle, fmt_out) = unsafe { (&*manager, &mut *out_format) };
        if let Err(code) = fmt_out.check_abi() {
            return code;
        }
        let Ok(guard) = mgr_handle.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        if let Some(fmt) = guard.working_format() {
            *fmt_out = CspaPcmFormatRecord::from(fmt.as_ref());
            // SAFETY: `out_has_working_format` was checked non-null above.
            unsafe {
                *out_has_working_format = 1;
            }
        } else {
            // SAFETY: `out_has_working_format` was checked non-null above.
            unsafe {
                *out_has_working_format = 0;
            }
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_update_targets(
    manager: *mut CspaManagerHandle,
    targets: *const CaptureTargetRecord,
    target_count: usize,
    out_membership_version: *mut u64,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || targets.is_null() || target_count == 0 || target_count > 1024 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `manager` and `targets` were checked non-null above.
        let mgr_handle = unsafe { &*manager };
        let raw_targets = unsafe { std::slice::from_raw_parts(targets, target_count) };
        let mut decoded = Vec::with_capacity(target_count);
        for rec in raw_targets {
            match rec.decode() {
                Ok(t) => decoded.push(t),
                Err(code) => return code,
            }
        }
        let Ok(mut guard) = mgr_handle.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        let version = match guard.update_targets(decoded) {
            Ok(v) => v,
            Err(err) => return map_capability_error(err),
        };
        if !out_membership_version.is_null() {
            // SAFETY: `out_membership_version` is non-null.
            unsafe {
                *out_membership_version = version;
            }
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_read(
    manager: *mut CspaManagerHandle,
    requested_frames: u64,
    timeout_ms: u64,
    cancel: *mut CspaCancelHandle,
    out_outcome: *mut CspaManagedReadOutcomeRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_outcome.is_null() || requested_frames == 0 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `manager` and `out_outcome` were checked non-null above.
        let (mgr, out_ref) = unsafe { (&*manager, &mut *out_outcome) };
        if let Err(code) = out_ref.check_abi() {
            return code;
        }
        if mgr.callback_active.load(SeqCst)
            || mgr
                .active_readers
                .compare_exchange(0, 1, SeqCst, SeqCst)
                .is_err()
        {
            return CSPA_STATUS_ALREADY_PENDING;
        }
        let cancel_tok = if cancel.is_null() {
            mgr.default_cancel.clone()
        } else {
            // SAFETY: `cancel` is non-null.
            unsafe { (*cancel).inner.clone() }
        };
        let res = mgr
            .manager
            .lock()
            .map_err(|_| CSPA_STATUS_INTERNAL_ERROR)
            .and_then(|mut guard| {
                let read_res = guard
                    .read_with_cancel(
                        requested_frames as usize,
                        Duration::from_millis(timeout_ms),
                        cancel_tok,
                    )
                    .map_err(map_read_error)?;
                Ok((
                    guard.session_id(),
                    guard.segment_id(),
                    guard.last_block_metadata(),
                    read_res,
                ))
            });
        mgr.active_readers.fetch_sub(1, SeqCst);
        match res {
            Ok((sess, seg, meta, read_res)) => {
                populate_owned_outcome_record(sess, seg, meta, read_res, out_ref);
                CSPA_STATUS_OK
            }
            Err(code) => code,
        }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_read_into(
    manager: *mut CspaManagerHandle,
    destination: *mut u8,
    destination_bytes: usize,
    requested_frames: u64,
    timeout_ms: u64,
    cancel: *mut CspaCancelHandle,
    out_outcome: *mut CspaManagedReadOutcomeRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null()
            || destination.is_null()
            || destination_bytes == 0
            || out_outcome.is_null()
            || requested_frames == 0
        {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `manager` and `out_outcome` were checked non-null above.
        let (mgr, out_ref) = unsafe { (&*manager, &mut *out_outcome) };
        if let Err(code) = out_ref.check_abi() {
            return code;
        }
        if mgr.callback_active.load(SeqCst)
            || mgr
                .active_readers
                .compare_exchange(0, 1, SeqCst, SeqCst)
                .is_err()
        {
            return CSPA_STATUS_ALREADY_PENDING;
        }
        let cancel_tok = if cancel.is_null() {
            mgr.default_cancel.clone()
        } else {
            // SAFETY: `cancel` is non-null.
            unsafe { (*cancel).inner.clone() }
        };
        // SAFETY: `destination` is non-null and caller declared `destination_bytes` writable bytes.
        let dst_slice = unsafe { std::slice::from_raw_parts_mut(destination, destination_bytes) };
        let res = mgr
            .manager
            .lock()
            .map_err(|_| CSPA_STATUS_INTERNAL_ERROR)
            .and_then(|mut guard| {
                let read_res = guard
                    .read_into_with_cancel(
                        dst_slice,
                        requested_frames as usize,
                        Duration::from_millis(timeout_ms),
                        cancel_tok,
                    )
                    .map_err(map_read_error)?;
                Ok((
                    guard.session_id(),
                    guard.segment_id(),
                    guard.last_block_metadata(),
                    read_res,
                ))
            });
        mgr.active_readers.fetch_sub(1, SeqCst);
        match res {
            Ok((sess, seg, meta, read_res)) => {
                populate_into_outcome_record(sess, seg, meta, read_res, destination, out_ref);
                CSPA_STATUS_OK
            }
            Err(code) => code,
        }
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_input_count(
    manager: *const CspaManagerHandle,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let mgr = unsafe { &*manager };
        let Ok(guard) = mgr.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        let count = guard.input_handles().len().max(1);
        unsafe {
            *out_count = count;
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_input_status(
    manager: *const CspaManagerHandle,
    index: usize,
    out_status: *mut CspaInputStatusRecord,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_status.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let (mgr, st_out) = unsafe { (&*manager, &mut *out_status) };
        if let Err(code) = st_out.check_abi() {
            return code;
        }
        let Ok(guard) = mgr.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        if let Some(h) = guard.input_handles().get(index) {
            fill_input_status_from_handle(h, st_out);
            return CSPA_STATUS_OK;
        }
        if index != 0 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let native_fmt = guard.output_format().cloned();
        st_out.target_index = 0;
        st_out.backend = encode_backend(guard.selected_backend());
        st_out.participation_state = CSPA_PARTICIPATION_ACTIVE;
        st_out.failure_code = 0;
        st_out.native_rate_hz = native_fmt.as_ref().map_or(0, |f| f.rate());
        st_out.native_channels = native_fmt.as_ref().map_or(0, |f| u32::from(f.channels()));
        st_out.native_sample_format = native_fmt
            .as_ref()
            .map_or(0, |f| encode_sample_type(f.sample()));
        st_out.mixer_rate_hz = st_out.native_rate_hz;
        st_out.mixer_channels = st_out.native_channels;
        st_out.is_resampled = 0;
        st_out.is_identity_bypass = 1;
        st_out.reserved = 0;
        st_out.queued_packets = 0;
        st_out.dropped_frames = 0;
        st_out.clipped_samples = 0;
        st_out.nonfinite_samples = 0;
        st_out.drift_slips = 0;
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_start_callback(
    manager: *mut CspaManagerHandle,
    requested_frames: u64,
    timeout_ms: u64,
    callback: CspaManagedCallbackFn,
    user_data: *mut c_void,
    on_context_release: CspaContextReleaseFn,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || callback.is_none() || requested_frames == 0 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let cb_fn = callback.unwrap();
        // SAFETY: `manager` was checked non-null above.
        let mgr = unsafe { &*manager };
        if mgr.active_readers.load(SeqCst) > 0
            || mgr
                .callback_active
                .compare_exchange(false, true, SeqCst, SeqCst)
                .is_err()
        {
            return CSPA_STATUS_ALREADY_PENDING;
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_flag.clone();
        let cb_cancel = Cancellation::default();
        let cb_cancel_worker = cb_cancel.clone();
        let manager_arc = mgr.manager.clone();
        let cb_active = mgr.callback_active.clone();
        let cb_invocations = mgr.callback_invocations.clone();
        let user_data_addr = user_data as usize;

        let Ok(join_handle) = thread::Builder::new()
            .name("cspa-ffi-callback".into())
            .spawn(move || {
                while !stop_clone.load(SeqCst) {
                    let res = manager_arc
                        .lock()
                        .map_err(|_| CSPA_STATUS_INTERNAL_ERROR)
                        .and_then(|mut guard| {
                            let read_res = guard
                                .read_with_cancel(
                                    requested_frames as usize,
                                    Duration::from_millis(timeout_ms),
                                    cb_cancel_worker.clone(),
                                )
                                .map_err(map_read_error)?;
                            Ok((
                                guard.session_id(),
                                guard.segment_id(),
                                guard.last_block_metadata(),
                                read_res,
                            ))
                        });
                    match res {
                        Ok((sess, seg, meta, read_res)) => {
                            if stop_clone.load(SeqCst)
                                || matches!(read_res.reason, PartialReason::Cancelled)
                            {
                                break;
                            }
                            if read_res.frames == 0
                                && matches!(
                                    read_res.reason,
                                    PartialReason::Timeout | PartialReason::WouldBlock
                                )
                            {
                                continue;
                            }
                            let is_terminal = matches!(
                                read_res.reason,
                                PartialReason::Eof | PartialReason::Failure(_)
                            ) && read_res.frames == 0;
                            let mut outcome = CspaManagedReadOutcomeRecord {
                                struct_size: size_of::<CspaManagedReadOutcomeRecord>() as u32,
                                schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
                                partial_reason: 0,
                                boundary_kind: 0,
                                has_mixed_metadata: 0,
                                reserved: 0,
                                session: 0,
                                segment: 0,
                                actual_frames: 0,
                                copy_bytes: 0,
                                copy_destinations: 0,
                                block: CspaBlockRecord {
                                    header: CspaHeaderRecord::default(),
                                    bytes: ptr::null(),
                                    byte_length: 0,
                                    retained_owner: ptr::null_mut(),
                                },
                                mixed_metadata: MixedBlockMetadataRecord::from(
                                    capture_system_or_process_audio::MixedBlockMetadata::default(),
                                ),
                            };
                            populate_owned_outcome_record(sess, seg, meta, read_res, &mut outcome);
                            cb_invocations.fetch_add(1, SeqCst);
                            let cb_status = catch_unwind(AssertUnwindSafe(|| {
                                // SAFETY: `outcome` is valid on the worker stack and `user_data`
                                // remains retained until `stop_join_and_release_context` joins this worker.
                                unsafe { cb_fn(&outcome, user_data_addr as *mut c_void) }
                            }))
                            .unwrap_or(CSPA_STATUS_CALLBACK_FAILURE);
                            if !outcome.block.retained_owner.is_null() {
                                // SAFETY: Release the worker's temporary owner ref after callback return.
                                unsafe {
                                    cspa_block_release(&mut outcome.block);
                                }
                            }
                            if cb_status != CSPA_STATUS_OK {
                                if let Ok(mut guard) = manager_arc.lock() {
                                    guard.stop(Stop::Abort);
                                }
                                stop_clone.store(true, SeqCst);
                                break;
                            }
                            if is_terminal {
                                stop_clone.store(true, SeqCst);
                                break;
                            }
                        }
                        Err(_) => {
                            stop_clone.store(true, SeqCst);
                            break;
                        }
                    }
                }
                cb_active.store(false, SeqCst);
            })
        else {
            mgr.callback_active.store(false, SeqCst);
            return CSPA_STATUS_INTERNAL_ERROR;
        };

        if let Ok(mut guard) = mgr.callback_ctrl.lock() {
            *guard = Some(FfiCallbackControl {
                stop_flag,
                cb_cancel,
                join_handle: Some(join_handle),
                user_data_addr,
                on_context_release,
                released: false,
            });
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_stop_callback(manager: *mut CspaManagerHandle) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `manager` was checked non-null above.
        let mgr = unsafe { &*manager };
        mgr.join_callback_if_safe();
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_callback_invocations(
    manager: *const CspaManagerHandle,
    out_count: *mut usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_count.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let mgr = unsafe { &*manager };
        unsafe {
            *out_count = mgr.callback_invocations.load(SeqCst);
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_stop(
    manager: *mut CspaManagerHandle,
    abort_mode: u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || abort_mode > 1 {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        let mode = if abort_mode != 0 {
            Stop::Abort
        } else {
            Stop::Drain
        };
        // SAFETY: `manager` was checked non-null above.
        let mgr = unsafe { &*manager };
        if let Ok(mut guard) = mgr.callback_ctrl.lock()
            && let Some(ctrl) = guard.as_mut()
        {
            ctrl.stop_flag.store(true, SeqCst);
            ctrl.cb_cancel.cancel();
        }
        let status = if let Ok(mut guard) = mgr.manager.lock() {
            guard.stop(mode);
            CSPA_STATUS_OK
        } else {
            CSPA_STATUS_INTERNAL_ERROR
        };
        if abort_mode != 0 {
            mgr.join_callback_if_safe();
        }
        status
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_wait_cleanup(
    manager: *mut CspaManagerHandle,
    timeout_ms: u64,
    out_cleaned: *mut u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if manager.is_null() || out_cleaned.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: Both pointers were checked non-null above.
        let mgr = unsafe { &*manager };
        mgr.join_callback_if_safe();
        let Ok(mut guard) = mgr.manager.lock() else {
            return CSPA_STATUS_INTERNAL_ERROR;
        };
        let cleaned = guard.wait_cleanup(Duration::from_millis(timeout_ms.max(1)));
        unsafe {
            *out_cleaned = u32::from(cleaned);
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_manager_release(manager: *mut CspaManagerHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !manager.is_null() {
            // SAFETY: `manager` was allocated by `Box::into_raw` in `cspa_manager_open`.
            let mgr = unsafe { Box::from_raw(manager) };
            if let Ok(mut guard) = mgr.manager.lock() {
                guard.stop(Stop::Abort);
            }
            mgr.join_callback_if_safe();
            drop(mgr);
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_test_simulate_panic_containment(should_panic: u32) -> u32 {
    catch_unwind(|| {
        if should_panic != 0 {
            panic!("deliberate simulated panic inside FFI boundary");
        }
        CSPA_STATUS_OK
    })
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cspa_test_discovery_publish_synthetic(
    discovery: *const CspaDiscoveryHandle,
    count: usize,
    out_revision: *mut u64,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if discovery.is_null() {
            return CSPA_STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: `discovery` was checked non-null above.
        let svc = unsafe { (*discovery).inner.service() };
        let mut rev = svc.current_revision();
        for idx in 0..count {
            rev = svc.publish_event(DiscoveryEventKind::Removed(
                format!("process:{idx}:1").into(),
            ));
        }
        if !out_revision.is_null() {
            // SAFETY: `out_revision` is non-null.
            unsafe {
                *out_revision = rev;
            }
        }
        CSPA_STATUS_OK
    }))
    .unwrap_or(CSPA_STATUS_INTERNAL_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn ffi_crate_has_zero_os_cfg_attributes_and_validates_abi() {
        let win_cfg = concat!("cfg(", "windows)");
        let os_cfg = concat!("cfg(", "target_os");
        let win_macro = concat!("cfg!(", "windows)");
        let os_macro = concat!("cfg!(", "target_os");
        for (name, src) in [
            ("src/lib.rs", include_str!("lib.rs")),
            ("src/records.rs", include_str!("records.rs")),
        ] {
            for pat in [win_cfg, os_cfg, win_macro, os_macro] {
                assert!(
                    !src.contains(pat),
                    "{name} must not contain OS conditional compilation `{pat}`"
                );
            }
        }

        // SAFETY: Pure scalar FFI functions.
        unsafe {
            assert_eq!(cspa_abi_check(0, 1), CSPA_STATUS_OK);
            assert_eq!(cspa_abi_check(0, 0), CSPA_STATUS_ABI_MISMATCH);
            assert_eq!(cspa_abi_check(1, 1), CSPA_STATUS_ABI_MISMATCH);
            assert_eq!(cspa_test_simulate_panic_containment(0), CSPA_STATUS_OK);
            assert_eq!(
                cspa_test_simulate_panic_containment(1),
                CSPA_STATUS_INTERNAL_ERROR
            );
        }
    }

    #[test]
    fn cspa_manager_read_into_runs_zero_steady_state_allocations_over_1000_blocks() {
        let mut t_out = CaptureTargetRecord::default();
        // SAFETY: Valid stack pointer with initialized ABI header.
        assert_eq!(
            unsafe { cspa_target_default_output(CSPA_DEFAULT_DEVICE_PIN, 1.0, &mut t_out) },
            CSPA_STATUS_OK
        );

        let mix_cfg = CspaMixerConfigRecord {
            silence_on_idle: 1,
            ..CspaMixerConfigRecord::default()
        };

        let mut req = CspaCaptureRequestRecord {
            struct_size: 0,
            schema_revision: 0,
            tree_strategy: ProcessTreeRangeRecord::default(),
            combine_mode: 0,
            input_conversion: 0,
            on_format_change: 0,
            allow_reroute: 0,
            require_precise_timing: 0,
            overflow: 0,
            permission_policy: 0,
            reserved: 0,
            targets: ptr::null(),
            target_count: 0,
            backends: ptr::null(),
            converter: ptr::null(),
            mixer_config: ptr::null(),
            budgets: ptr::null(),
        };
        // SAFETY: Valid stack pointers.
        assert_eq!(
            unsafe { cspa_capture_request_init(&mut req, &t_out, 1) },
            CSPA_STATUS_OK
        );
        req.combine_mode = CSPA_COMBINE_MODE_CSPA_MIXER;
        req.mixer_config = &mix_cfg;

        let mut mgr: *mut CspaManagerHandle = ptr::null_mut();
        // SAFETY: Valid request and output handle pointer.
        assert_eq!(unsafe { cspa_manager_open(&req, &mut mgr) }, CSPA_STATUS_OK);
        assert!(!mgr.is_null());

        let mut fmt = CspaPcmFormatRecord {
            struct_size: size_of::<CspaPcmFormatRecord>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            sample_format: 0,
            byte_order: 0,
            rate_hz: 0,
            channels: 0,
        };
        // SAFETY: Valid manager handle and initialized format record.
        assert_eq!(
            unsafe { cspa_manager_output_format(mgr, &mut fmt) },
            CSPA_STATUS_OK
        );
        let bpf = (fmt.channels as usize) * 4;
        let mut dst = vec![0u8; 256 * bpf];
        let mut outcome = CspaManagedReadOutcomeRecord {
            struct_size: size_of::<CspaManagedReadOutcomeRecord>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            partial_reason: 0,
            boundary_kind: 0,
            has_mixed_metadata: 0,
            reserved: 0,
            session: 0,
            segment: 0,
            actual_frames: 0,
            copy_bytes: 0,
            copy_destinations: 0,
            block: CspaBlockRecord {
                header: CspaHeaderRecord::default(),
                bytes: ptr::null(),
                byte_length: 0,
                retained_owner: ptr::null_mut(),
            },
            mixed_metadata: MixedBlockMetadataRecord::from(
                capture_system_or_process_audio::MixedBlockMetadata::default(),
            ),
        };

        for _ in 0..4 {
            // SAFETY: Valid manager handle, destination buffer, and outcome struct.
            assert_eq!(
                unsafe {
                    cspa_manager_read_into(
                        mgr,
                        dst.as_mut_ptr(),
                        dst.len(),
                        128,
                        50,
                        ptr::null_mut(),
                        &mut outcome,
                    )
                },
                CSPA_STATUS_OK
            );
            assert!(outcome.block.retained_owner.is_null());
        }

        let allocs = allocation_counter::measure(|| {
            for _ in 0..1_000 {
                // SAFETY: Valid manager handle, destination buffer, and outcome struct.
                let st = unsafe {
                    cspa_manager_read_into(
                        mgr,
                        dst.as_mut_ptr(),
                        dst.len(),
                        128,
                        20,
                        ptr::null_mut(),
                        &mut outcome,
                    )
                };
                assert_eq!(st, CSPA_STATUS_OK);
                assert!(outcome.actual_frames > 0 && outcome.actual_frames <= 128);
            }
        });
        assert_eq!(
            allocs.count_total, 0,
            "cspa_manager_read_into must allocate 0 bytes over 1,000 warmed blocks (got {} allocs / {} bytes)",
            allocs.count_total, allocs.bytes_total
        );

        let drop_t0 = Instant::now();
        // SAFETY: Valid manager handle.
        assert_eq!(unsafe { cspa_manager_stop(mgr, 1) }, CSPA_STATUS_OK);
        let mut cleaned = 0u32;
        assert_eq!(
            unsafe { cspa_manager_wait_cleanup(mgr, 2000, &mut cleaned) },
            CSPA_STATUS_OK
        );
        assert_eq!(cleaned, 1);
        unsafe {
            cspa_manager_release(mgr);
        }
        let drop_ms = drop_t0.elapsed().as_millis();
        assert!(drop_ms <= 2_000);
        println!(
            "{{\"event\":\"ffi-read-into-zero-alloc\",\"warmed_blocks\":1000,\"allocs\":{},\"bytes\":{},\"cleanup_ms\":{}}}",
            allocs.count_total, allocs.bytes_total, drop_ms
        );
    }

    #[test]
    fn cspa_permissions_check_and_request_validate_abi_and_target_domains() {
        assert_eq!(size_of::<CspaCaptureRequestRecord>(), 96);
        assert_eq!(size_of::<CspaPlanReportRecord>(), 112);
        assert_eq!(size_of::<CspaPermissionReportRecord>(), 104);

        let mut rep = CspaPermissionReportRecord::default();
        // SAFETY: Valid stack pointer.
        assert_eq!(
            unsafe { cspa_permission_report_init(&mut rep) },
            CSPA_STATUS_OK
        );
        assert_eq!(
            unsafe { cspa_permissions_check(ptr::null(), 0, ptr::null(), &mut rep) },
            CSPA_STATUS_OK
        );
        assert_eq!(rep.entry_count, CSPA_PERMISSION_KIND_COUNT as u32);
        assert_eq!(rep.entries[0].kind, CSPA_PERMISSION_KIND_MICROPHONE);
        assert_eq!(rep.entries[1].kind, CSPA_PERMISSION_KIND_SYSTEM_AUDIO);
        assert_eq!(rep.entries[2].kind, CSPA_PERMISSION_KIND_SCREEN_RECORDING);

        let mut bad_rep = rep;
        bad_rep.struct_size = 16;
        assert_eq!(
            unsafe { cspa_permissions_check(ptr::null(), 0, ptr::null(), &mut bad_rep) },
            CSPA_STATUS_ABI_MISMATCH
        );

        let mut t_mic = CaptureTargetRecord::default();
        assert_eq!(
            unsafe { cspa_target_microphone(CSPA_DEFAULT_DEVICE_FOLLOW, 1.0, &mut t_mic) },
            CSPA_STATUS_OK
        );
        let mut req = CspaCaptureRequestRecord {
            struct_size: 0,
            schema_revision: 0,
            tree_strategy: ProcessTreeRangeRecord::default(),
            combine_mode: 0,
            input_conversion: 0,
            on_format_change: 0,
            allow_reroute: 0,
            require_precise_timing: 0,
            overflow: 0,
            permission_policy: 0,
            reserved: 0,
            targets: ptr::null(),
            target_count: 0,
            backends: ptr::null(),
            converter: ptr::null(),
            mixer_config: ptr::null(),
            budgets: ptr::null(),
        };
        assert_eq!(
            unsafe { cspa_capture_request_init(&mut req, &t_mic, 1) },
            CSPA_STATUS_OK
        );
        req.permission_policy = CSPA_PERMISSION_POLICY_CHECK_ONLY;
        assert_eq!(
            unsafe { cspa_request_check_permissions(&req, &mut rep) },
            CSPA_STATUS_OK
        );
        assert_eq!(rep.entries[0].required, 1);
        assert_eq!(rep.entries[1].required, 0);
        assert_eq!(
            unsafe { cspa_request_request_permissions(&req, 500, 0, &mut rep) },
            CSPA_STATUS_OK
        );
    }
}
