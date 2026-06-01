#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::capabilities::check_os_version_floor_for_target;
use crate::core::{
    StorageLimits,
    manager::{StorageBudget, StorageReservation},
    measurements::Trace,
    ordering::{self, ControlHandle, Failure, Overflow, Stop},
    planner::candidate_backends_for_target,
    reads::{Cancellation, ReadError, ReadResult, ReadStream},
    schema::{
        AggregateManagerBudgets, BackendId, Backends, CaptureTarget, Format, PermissionPolicy,
        PlatformOs, ProcessIdentity, ProcessScope, ProcessTreeRange, TargetKind,
    },
    storage::Pool,
    stream_runtime::{
        EndReason, ReadySignal, StreamReadContext, poll_read_stream, poll_read_stream_into,
        signal_stream_stop,
    },
    traits::{AudioStreamBackend, CapabilityError, OpenableStreamBackend, TimingRequirement},
};
use crate::coreaudio_stream::{
    CoreAudioInputStream, CoreAudioTapStream, MAX_FRAMES_PER_SLOT, SESSION_METADATA_BYTES,
    open_coreaudio_input_point_with_auth_override, open_coreaudio_tap_point,
    pack_planar_or_interleaved_f32_into_bytes,
};
use crate::discovery::{query_macos_default_input_uid_now, query_macos_default_output_uid_now};
use crate::proc_info::inspect_bsd_process;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering::SeqCst},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct ScreenCaptureKitStream {
    format: Arc<Format>,
    trace: Option<Arc<Trace>>,
    reads: ReadStream,
    control: ControlHandle,
    default_cancel: Cancellation,
    armed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    ready: Arc<ReadySignal>,
    ended: Arc<AtomicU32>,
    worker: Mutex<Option<JoinHandle<()>>>,
    _reservation: StorageReservation,
}

impl ScreenCaptureKitStream {
    pub(crate) fn arm(&self) {
        self.armed.store(true, SeqCst);
    }

    pub(crate) fn poll_read_with_cancel(
        &self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.arm();
        poll_read_stream(
            StreamReadContext {
                reads: &self.reads,
                ready: &self.ready,
                trace: self.trace.as_ref(),
            },
            frames,
            timeout,
            cancel,
        )
    }

    pub(crate) fn poll_read_into_with_cancel(
        &self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.arm();
        poll_read_stream_into(
            StreamReadContext {
                reads: &self.reads,
                ready: &self.ready,
                trace: self.trace.as_ref(),
            },
            self.format.clone(),
            destination,
            frames,
            timeout,
            cancel,
        )
    }
}

impl AudioStreamBackend for ScreenCaptureKitStream {
    #[inline]
    fn format(&self) -> &Arc<Format> {
        &self.format
    }

    #[inline]
    fn trace(&self) -> Option<&Arc<Trace>> {
        self.trace.as_ref()
    }

    #[inline]
    fn control_handle(&self) -> ControlHandle {
        self.control.clone()
    }

    #[inline]
    fn default_cancel(&self) -> Cancellation {
        self.default_cancel.clone()
    }

    fn end_reason(&self) -> Option<EndReason> {
        if let Some(failure) = self.control.snapshot().failure {
            return Some(EndReason::Failed(failure));
        }
        match self.ended.load(SeqCst) {
            1 => Some(EndReason::Stopped),
            2 => Some(EndReason::TargetExited),
            3 => Some(EndReason::Failed(Failure::Backend)),
            _ => None,
        }
    }

    #[inline]
    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.poll_read_with_cancel(frames, timeout, cancel)
    }

    #[inline]
    fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.poll_read_into_with_cancel(destination, frames, timeout, cancel)
    }

    fn stop(&self, mode: Stop) {
        let _ = self.ended.compare_exchange(0, 1, SeqCst, SeqCst);
        signal_stream_stop(&self.control, &self.stop, &self.ready, mode);
    }

    fn resources_done(&self) -> bool {
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(JoinHandle::is_finished)
            && guard.take().unwrap().join().is_err()
        {
            self.control.fail(Failure::Backend);
        }
        guard.is_none() && self.control.resources_done()
    }

    fn wait_cleanup(&self, timeout: Duration) -> bool {
        self.stop.store(true, SeqCst);
        self.ready.notify();
        let started = Instant::now();
        while !self.resources_done() && started.elapsed() < timeout {
            thread::sleep(Duration::from_millis(1));
        }
        self.resources_done()
    }
}

impl Drop for ScreenCaptureKitStream {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    use crate::core::{
        ordering::Producer,
        schema::{ByteOrder, Channel, Layout, SampleType},
    };
    use crate::coreaudio_stream::{POOL_SLOTS, QUEUE_SLOTS, publish_macos_packet};
    use crate::native_bridge::NativeSckStreamHandle;
    use std::ffi::{c_float, c_void};
    use std::sync::atomic::AtomicU64;

    struct SckCallbackCtx {
        armed: Arc<AtomicBool>,
        producer: Mutex<Producer>,
        policy: Overflow,
        ready: Arc<ReadySignal>,
        sequence: AtomicU64,
        segment: AtomicU64,
        frame_cursor: AtomicU64,
        scratch: Mutex<Vec<u8>>,
    }

    unsafe extern "C" fn on_sck_packet(
        user_data: *mut c_void,
        num_buffers: u32,
        buffer_ptrs: *const *const c_float,
        buffer_channels: *const u32,
        frames: u32,
        timestamp_ns: u64,
        timestamp_valid: u32,
    ) {
        // SAFETY: `user_data` points to `SckCallbackCtx` pinned for the SCStream's lifetime;
        // `scratch` is preallocated (`MAX_FRAMES_PER_SLOT * 8` bytes), so 0 heap allocations occur.
        unsafe {
            if user_data.is_null()
                || buffer_ptrs.is_null()
                || buffer_channels.is_null()
                || frames == 0
            {
                return;
            }
            let ctx = &*(user_data as *const SckCallbackCtx);
            if !ctx.armed.load(SeqCst) {
                return;
            }
            let mut prod = ctx.producer.lock().unwrap_or_else(|e| e.into_inner());
            let mut scratch = ctx.scratch.lock().unwrap_or_else(|e| e.into_inner());
            let acquired = prod.trace().map(|_| Instant::now());
            let total_chans = prod.pool().format().channels() as usize;
            let bpf = prod.pool().format().bytes_per_frame();
            let ts = (timestamp_valid != 0 && timestamp_ns > 0).then_some(timestamp_ns);

            let nbufs = (num_buffers as usize).min(8);
            let mut rem = frames as usize;
            let mut frame_off = 0usize;
            while rem > 0 {
                let chunk_frames = rem.min(MAX_FRAMES_PER_SLOT);
                let chunk_bytes = chunk_frames * bpf;
                let mut planes: [(&[f32], usize); 8] = [(&[], 0); 8];
                for (i, slot) in planes.iter_mut().enumerate().take(nbufs) {
                    let ptr = *buffer_ptrs.add(i);
                    let ch = (*buffer_channels.add(i)) as usize;
                    if ptr.is_null() || ch == 0 {
                        return;
                    }
                    let slice =
                        std::slice::from_raw_parts(ptr.add(frame_off * ch), chunk_frames * ch);
                    *slot = (slice, ch);
                }
                if pack_planar_or_interleaved_f32_into_bytes(
                    &planes[..nbufs],
                    chunk_frames,
                    total_chans,
                    &mut scratch[..chunk_bytes],
                )
                .is_ok()
                {
                    let mut seq = ctx.sequence.load(SeqCst);
                    let mut seg = ctx.segment.load(SeqCst);
                    let mut cur = ctx.frame_cursor.load(SeqCst);
                    publish_macos_packet(
                        &mut prod,
                        &scratch[..chunk_bytes],
                        false,
                        false,
                        chunk_frames,
                        ts,
                        2,
                        &mut seq,
                        &mut seg,
                        &mut cur,
                        ctx.policy,
                        acquired,
                    );
                    ctx.sequence.store(seq, SeqCst);
                    ctx.segment.store(seg, SeqCst);
                    ctx.frame_cursor.store(cur, SeqCst);
                }
                rem -= chunk_frames;
                frame_off += chunk_frames;
            }
            ctx.ready.notify();
        }
    }

    pub fn open_sck_app_point(
        target_identity: ProcessIdentity,
        exclude_current_process: bool,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<ScreenCaptureKitStream, CapabilityError> {
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }
        let rec = inspect_bsd_process(target_identity.pid)?;
        if target_identity.generation != 0 && rec.start_time_us != target_identity.generation {
            return Err(CapabilityError::PidReused);
        }

        let format = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .map_err(|_| CapabilityError::InvalidTarget)?,
        );
        let stride = format.bytes_per_frame();
        let limits = StorageLimits::new(
            POOL_SLOTS,
            MAX_FRAMES_PER_SLOT,
            stride,
            POOL_SLOTS * MAX_FRAMES_PER_SLOT * stride,
        )
        .map_err(|_| CapabilityError::CapacityExceeded)?;
        let trace = if observed {
            Some(Arc::new(Trace::new_compact()))
        } else {
            None
        };
        let metadata_bytes = trace.as_ref().map_or(6024usize, |t| {
            6024usize
                .saturating_add(t.storage_bytes())
                .min(SESSION_METADATA_BYTES)
        });
        let reservation = budget
            .reserve(limits, metadata_bytes)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let retained_reservation = reservation.retain();
        let pool = Pool::new_reserved(limits, MAX_FRAMES_PER_SLOT, format.clone(), reservation)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let (producer, reader) = if let Some(ref tr) = trace {
            ordering::channel_observed(pool, QUEUE_SLOTS, tr.clone())
        } else {
            ordering::channel(pool, QUEUE_SLOTS)
        }
        .map_err(|_| CapabilityError::CapacityExceeded)?;

        let armed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(ReadySignal::new());
        let ended = Arc::new(AtomicU32::new(0));
        let armed_worker = armed.clone();
        let stop_worker = stop.clone();
        let ready_worker = ready.clone();
        let ended_worker = ended.clone();
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), CapabilityError>>(1);

        let worker = thread::spawn(move || {
            let cb_ctx = Box::new(SckCallbackCtx {
                armed: armed_worker,
                producer: Mutex::new(producer),
                policy: overflow,
                ready: ready_worker.clone(),
                sequence: AtomicU64::new(0),
                segment: AtomicU64::new(1),
                frame_cursor: AtomicU64::new(0),
                scratch: Mutex::new(vec![0u8; MAX_FRAMES_PER_SLOT * stride]),
            });
            let cb_ptr: *mut c_void =
                (&*cb_ctx as *const SckCallbackCtx as *mut SckCallbackCtx).cast();

            let mut handle = match NativeSckStreamHandle::open(
                target_identity.pid,
                exclude_current_process,
                on_sck_packet,
                cb_ptr,
            ) {
                Ok(h) => h,
                Err(err) => {
                    let _ = init_tx.send(Err(err));
                    return;
                }
            };
            let _ = init_tx.send(Ok(()));

            while !stop_worker.load(SeqCst) {
                let stopping = cb_ctx
                    .producer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .control_handle()
                    .stopping();
                if stopping {
                    break;
                }
                match inspect_bsd_process(target_identity.pid) {
                    Ok(cur)
                        if target_identity.generation == 0
                            || cur.start_time_us == target_identity.generation => {}
                    _ => {
                        let _ = ended_worker.compare_exchange(0, 2, SeqCst, SeqCst);
                        break;
                    }
                }
                unsafe extern "C" {
                    fn cspa_mac_pump_runloop_ms(ms: u32);
                }
                // SAFETY: pumps `CFRunLoopGetCurrent()` for `10 ms` in default mode.
                unsafe {
                    cspa_mac_pump_runloop_ms(10);
                }
            }

            handle.close();
            let _ = ended_worker.compare_exchange(0, 1, SeqCst, SeqCst);
            cb_ctx
                .producer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close();
            ready_worker.notify();
        });

        match init_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                let control = reader.control_handle();
                Ok(ScreenCaptureKitStream {
                    format,
                    trace,
                    reads: ReadStream::new(reader),
                    control,
                    default_cancel: Cancellation::default(),
                    armed,
                    stop,
                    ready,
                    ended,
                    worker: Mutex::new(Some(worker)),
                    _reservation: retained_reservation,
                })
            }
            Ok(Err(err)) => {
                stop.store(true, SeqCst);
                let _ = worker.join();
                Err(err)
            }
            Err(_) => {
                stop.store(true, SeqCst);
                let _ = worker.join();
                Err(CapabilityError::ServerUnavailable)
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::open_sck_app_point;

#[cfg(not(target_os = "macos"))]
pub fn open_sck_app_point(
    _target_identity: ProcessIdentity,
    _exclude_current_process: bool,
    _overflow: Overflow,
    _timing: TimingRequirement,
    _budget: &StorageBudget,
    _observed: bool,
) -> Result<ScreenCaptureKitStream, CapabilityError> {
    let _ = (
        MAX_FRAMES_PER_SLOT,
        SESSION_METADATA_BYTES,
        StorageLimits::new(8, 4096, 8, 262_144),
        Pool::new_reserved,
        ordering::channel,
        ReadStream::new,
        pack_planar_or_interleaved_f32_into_bytes,
    );
    Err(CapabilityError::ServerUnavailable)
}

/// Owns a single backend session without dynamic process/device monitoring.
pub enum PlatformStream {
    CoreAudio(CoreAudioTapStream),
    CoreAudioInput(CoreAudioInputStream),
    ScreenCaptureKit(ScreenCaptureKitStream),
}

impl PlatformStream {
    pub fn open(target: &CaptureTarget, overflow: Overflow) -> Result<Self, CapabilityError> {
        Self::open_with_options(
            target,
            ProcessTreeRange::default(),
            &Backends::default(),
            overflow,
            TimingRequirement::Raw,
        )
    }

    pub fn open_with_options(
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

    pub fn open_with_permission_policy(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
        permission_policy: PermissionPolicy,
    ) -> Result<Self, CapabilityError> {
        target.validate()?;
        tree_range.validate()?;
        backends.validate()?;
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }

        let mac_backends = backends.for_os(PlatformOs::MacOs);
        let candidates = candidate_backends_for_target(PlatformOs::MacOs, mac_backends, target)
            .map_err(CapabilityError::from)?;

        #[cfg(target_os = "macos")]
        let os_ver = crate::native_bridge::os_version();
        #[cfg(not(target_os = "macos"))]
        let os_ver = (15, 7, 9);

        let budget = StorageBudget::new(2, AggregateManagerBudgets::default().max_total_pcm_memory)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let clean_uid = target.clean_device_id().map(str::to_owned);
        let mut first_err: Option<CapabilityError> = None;

        for backend in candidates {
            if let Err(floor_err) =
                check_os_version_floor_for_target(backend, target.kind(), os_ver)
            {
                if first_err.is_none() {
                    first_err = Some(floor_err);
                }
                continue;
            }
            let res = match (backend, target.kind()) {
                (BackendId::CoreAudio, TargetKind::Device | TargetKind::DefaultOutput) => {
                    let uid = clean_uid
                        .clone()
                        .or_else(query_macos_default_output_uid_now);
                    open_coreaudio_tap_point(
                        Vec::new(),
                        false,
                        None,
                        uid,
                        false,
                        overflow,
                        timing,
                        &budget,
                        false,
                    )
                    .map(Self::CoreAudio)
                }
                (BackendId::CoreAudio, TargetKind::InputDevice | TargetKind::DefaultInput) => {
                    let uid = clean_uid.clone().or_else(query_macos_default_input_uid_now);
                    open_coreaudio_input_point_with_auth_override(
                        uid,
                        None,
                        permission_policy.allows_prompt(),
                        overflow,
                        timing,
                        &budget,
                        false,
                    )
                    .map(Self::CoreAudioInput)
                }
                (BackendId::ScreenCaptureKit, TargetKind::Application) => {
                    if target.scope() != ProcessScope::Exact {
                        Err(CapabilityError::UnsupportedScope)
                    } else {
                        let id = target
                            .process_identity()
                            .ok_or(CapabilityError::InvalidTarget)?;
                        let _ = inspect_bsd_process(id.pid)?;
                        crate::permissions::maybe_auto_request_sck_permission(permission_policy);
                        open_sck_app_point(id, false, overflow, timing, &budget, false)
                            .map(Self::ScreenCaptureKit)
                    }
                }
                (BackendId::ScreenCaptureKit, _) => Err(CapabilityError::UnsupportedTargetKind),
                _ => Err(CapabilityError::NotImplemented),
            };
            match res {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    let allow_fb = err.allows_cross_backend_fallback();
                    if first_err.is_none() {
                        first_err = Some(err.clone());
                    }
                    if !allow_fb {
                        return Err(err);
                    }
                }
            }
        }

        Err(first_err.unwrap_or(CapabilityError::NotImplemented))
    }

    pub(crate) fn arm(&self) {
        match self {
            Self::CoreAudio(s) => s.arm(),
            Self::CoreAudioInput(s) => s.arm(),
            Self::ScreenCaptureKit(s) => s.arm(),
        }
    }
}

impl AudioStreamBackend for PlatformStream {
    #[inline]
    fn format(&self) -> &Arc<Format> {
        match self {
            Self::CoreAudio(s) => s.format(),
            Self::CoreAudioInput(s) => s.format(),
            Self::ScreenCaptureKit(s) => s.format(),
        }
    }

    #[inline]
    fn trace(&self) -> Option<&Arc<Trace>> {
        match self {
            Self::CoreAudio(s) => s.trace(),
            Self::CoreAudioInput(s) => s.trace(),
            Self::ScreenCaptureKit(s) => s.trace(),
        }
    }

    #[inline]
    fn control_handle(&self) -> ControlHandle {
        match self {
            Self::CoreAudio(s) => s.control_handle(),
            Self::CoreAudioInput(s) => s.control_handle(),
            Self::ScreenCaptureKit(s) => s.control_handle(),
        }
    }

    #[inline]
    fn default_cancel(&self) -> Cancellation {
        match self {
            Self::CoreAudio(s) => s.default_cancel(),
            Self::CoreAudioInput(s) => s.default_cancel(),
            Self::ScreenCaptureKit(s) => s.default_cancel(),
        }
    }

    #[inline]
    fn end_reason(&self) -> Option<EndReason> {
        match self {
            Self::CoreAudio(s) => s.end_reason(),
            Self::CoreAudioInput(s) => s.end_reason(),
            Self::ScreenCaptureKit(s) => s.end_reason(),
        }
    }

    #[inline]
    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        match self {
            Self::CoreAudio(s) => s.read_with_cancel(frames, timeout, cancel),
            Self::CoreAudioInput(s) => s.read_with_cancel(frames, timeout, cancel),
            Self::ScreenCaptureKit(s) => s.read_with_cancel(frames, timeout, cancel),
        }
    }

    #[inline]
    fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        match self {
            Self::CoreAudio(s) => s.read_into_with_cancel(destination, frames, timeout, cancel),
            Self::CoreAudioInput(s) => {
                s.read_into_with_cancel(destination, frames, timeout, cancel)
            }
            Self::ScreenCaptureKit(s) => {
                s.read_into_with_cancel(destination, frames, timeout, cancel)
            }
        }
    }

    #[inline]
    fn stop(&self, mode: Stop) {
        match self {
            Self::CoreAudio(s) => s.stop(mode),
            Self::CoreAudioInput(s) => s.stop(mode),
            Self::ScreenCaptureKit(s) => s.stop(mode),
        }
    }

    #[inline]
    fn resources_done(&self) -> bool {
        match self {
            Self::CoreAudio(s) => s.resources_done(),
            Self::CoreAudioInput(s) => s.resources_done(),
            Self::ScreenCaptureKit(s) => s.resources_done(),
        }
    }

    #[inline]
    fn wait_cleanup(&self, timeout: Duration) -> bool {
        match self {
            Self::CoreAudio(s) => s.wait_cleanup(timeout),
            Self::CoreAudioInput(s) => s.wait_cleanup(timeout),
            Self::ScreenCaptureKit(s) => s.wait_cleanup(timeout),
        }
    }
}

impl OpenableStreamBackend for PlatformStream {
    #[inline]
    fn open_with_permission_policy(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
        permission_policy: PermissionPolicy,
    ) -> Result<Self, CapabilityError> {
        Self::open_with_permission_policy(
            target,
            tree_range,
            backends,
            overflow,
            timing,
            permission_policy,
        )
    }
}
