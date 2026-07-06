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

