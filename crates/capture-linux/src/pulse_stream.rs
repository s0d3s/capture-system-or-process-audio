#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::capabilities::PulseServerKind;
use crate::core::{
    StorageLimits,
    manager::{StorageBudget, StorageReservation},
    measurements::Trace,
    ordering::{self, ControlHandle, Failure, Overflow, Producer, Stop},
    reads::{Cancellation, ReadError, ReadResult, ReadStream},
    schema::{
        AggregateManagerBudgets, BackendId, Backends, ByteOrder, CaptureTarget, Channel, Format,
        Layout, PlatformOs, ProcessTreeRange, SampleType, TargetKind,
    },
    storage::Pool,
    stream_runtime::{
        EndReason, ReadySignal, StreamReadContext, poll_read_stream, poll_read_stream_into,
        signal_stream_stop,
    },
    traits::{AudioStreamBackend, CapabilityError, OpenableStreamBackend, TimingRequirement},
};
use crate::discovery::{collect_pulse_full_state, query_linux_default_sink_now};
use crate::loader::{LinuxClientPaths, probe_pulse_server};
use crate::pipewire_stream::{
    PipeWireStream, linux_endpoint_clock_domain, open_pipewire_point, open_pipewire_source_point,
    publish_linux_packet,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering::SeqCst},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POOL_SLOTS: usize = 8;
const QUEUE_SLOTS: usize = 4;
const MAX_FRAMES_PER_SLOT: usize = 4096;
const SESSION_METADATA_BYTES: usize = 16_384;

pub struct PulseStream {
    format: Arc<Format>,
    trace: Option<Arc<Trace>>,
    reads: ReadStream,
    control: ControlHandle,
    default_cancel: Cancellation,
    armed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    ready: Arc<ReadySignal>,
    ended: Arc<AtomicU32>,
    #[cfg_attr(not(any(test, feature = "native-tests")), allow(dead_code))]
    server_kind: PulseServerKind,
    worker: Mutex<Option<JoinHandle<()>>>,
    _reservation: StorageReservation,
}

impl PulseStream {
    pub(crate) fn arm(&self) {
        self.armed.store(true, SeqCst);
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub(crate) fn server_kind(&self) -> PulseServerKind {
        self.server_kind
    }

    pub(crate) fn poll_read_with_cancel(
        &self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.arm();
        let ctx = StreamReadContext {
            reads: &self.reads,
            ready: &self.ready,
            trace: self.trace.as_ref(),
        };
        poll_read_stream(ctx, frames, timeout, cancel)
    }

    pub(crate) fn poll_read_into_with_cancel(
        &self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.arm();
        let ctx = StreamReadContext {
            reads: &self.reads,
            ready: &self.ready,
            trace: self.trace.as_ref(),
        };
        poll_read_stream_into(
            ctx,
            self.format.clone(),
            destination,
            frames,
            timeout,
            cancel,
        )
    }
}

impl AudioStreamBackend for PulseStream {
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

    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.poll_read_with_cancel(frames, timeout, cancel)
    }

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
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
        guard.is_none()
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

impl Drop for PulseStream {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    use crate::loader::{
        PA_CONTEXT_FAILED, PA_CONTEXT_NOAUTOSPAWN, PA_CONTEXT_READY, PA_CONTEXT_TERMINATED,
        PA_SAMPLE_FLOAT32LE, PA_SAMPLE_S16LE, PA_SAMPLE_S24LE, PA_SAMPLE_S32LE,
        PA_STREAM_ADJUST_LATENCY, PA_STREAM_FAILED, PA_STREAM_READY, PA_STREAM_TERMINATED,
        PaBufferAttr, PaSampleSpec, PulseLibrary,
    };
    use std::ffi::{CString, c_int, c_void};

    pub fn open_pulse_point(
        paths: &LinuxClientPaths,
        monitor_source: Option<&str>,
        monitor_sink_input_index: Option<u32>,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<PulseStream, CapabilityError> {
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }
        let server_info = probe_pulse_server(paths)?;
        if monitor_sink_input_index.is_none()
            && let Some(explicit_sink) = monitor_source
            && !explicit_sink.ends_with(".monitor")
            && let Ok((devs, _, _, _)) = collect_pulse_full_state(paths)
            && !devs.iter().any(|d| d.name == explicit_sink)
        {
            return Err(CapabilityError::InvalidTarget);
        }
        let resolved_source = match monitor_source {
            Some(name) if name.ends_with(".monitor") => Some(name.to_owned()),
            Some(name) => Some(format!("{name}.monitor")),
            None => server_info
                .default_sink_name
                .as_ref()
                .map(|s| format!("{s}.monitor")),
        };
        let format = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48_000,
            2,
            Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
        )
        .map_err(|_| CapabilityError::InvalidTarget)?;
        open_pulse_stream_internal(
            paths,
            server_info.server_kind,
            resolved_source,
            monitor_sink_input_index,
            format,
            1,
            overflow,
            timing,
            budget,
            observed,
        )
    }

    pub fn open_pulse_source_point(
        paths: &LinuxClientPaths,
        source_name: Option<&str>,
        format_override: Option<Format>,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<PulseStream, CapabilityError> {
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }
        if source_name.is_some_and(|s| s.is_empty() || s.ends_with(".monitor")) {
            return Err(CapabilityError::InvalidTarget);
        }
        let server_info = probe_pulse_server(paths)?;
        let (_devs, sources, _pids, _si) = collect_pulse_full_state(paths)?;
        let matched = match source_name {
            Some(explicit) => sources
                .into_iter()
                .find(|s| s.name == explicit && !s.is_monitor)
                .ok_or(CapabilityError::InvalidTarget)?,
            None => sources
                .iter()
                .find(|s| s.is_default && !s.is_monitor)
                .or_else(|| sources.iter().find(|s| !s.is_monitor))
                .cloned()
                .ok_or(CapabilityError::ServerUnavailable)?,
        };
        let format = match format_override {
            Some(fmt) => fmt,
            None => {
                let sample = match matched.sample_format {
                    PA_SAMPLE_S16LE => SampleType::I16,
                    PA_SAMPLE_S24LE => SampleType::I24,
                    PA_SAMPLE_S32LE => SampleType::I32,
                    PA_SAMPLE_FLOAT32LE => SampleType::F32,
                    _ => SampleType::F32,
                };
                let (ch, layout) = if matched.channels == 1 {
                    (1u16, Layout::Known(Arc::from([Channel::Mono])))
                } else {
                    (
                        2u16,
                        Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
                    )
                };
                let rate = if (8_000..=192_000).contains(&matched.rate_hz) {
                    matched.rate_hz
                } else {
                    48_000
                };
                Format::new(sample, ByteOrder::Little, rate, ch, layout)
                    .map_err(|_| CapabilityError::InvalidTarget)?
            }
        };
        let domain = linux_endpoint_clock_domain(&matched.name);
        open_pulse_stream_internal(
            paths,
            server_info.server_kind,
            Some(matched.name),
            None,
            format,
            domain,
            overflow,
            timing,
            budget,
            observed,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open_pulse_stream_internal(
        paths: &LinuxClientPaths,
        server_kind: PulseServerKind,
        resolved_source: Option<String>,
        monitor_sink_input_index: Option<u32>,
        format: Format,
        clock_domain: u64,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<PulseStream, CapabilityError> {
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }
        let pa_fmt = match format.sample() {
            SampleType::I16 => PA_SAMPLE_S16LE,
            SampleType::I24 => PA_SAMPLE_S24LE,
            SampleType::I32 => PA_SAMPLE_S32LE,
            SampleType::F32 => PA_SAMPLE_FLOAT32LE,
        };
        let pa_channels =
            u8::try_from(format.channels()).map_err(|_| CapabilityError::InvalidTarget)?;
        let pa_rate = format.rate();
        let coalesce_frames = ((pa_rate / 100) as usize).clamp(64, 960);

        let lib = Arc::new(PulseLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PulseAudio, paths.pulse_min_version)
        })?);
        let format = Arc::new(format);
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
        let ready = Arc::new(ReadySignal::default());
        let ended = Arc::new(AtomicU32::new(0));
        let armed_worker = armed.clone();
        let stop_worker = stop.clone();
        let ready_worker = ready.clone();
        let ended_worker = ended.clone();

        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), CapabilityError>>(1);

        let worker = thread::spawn(move || {
            struct PaReadCtx {
                mainloop: *mut c_void,
                signal_fn: unsafe extern "C" fn(*mut c_void, c_int),
                peek_fn: unsafe extern "C" fn(*mut c_void, *mut *const c_void, *mut usize) -> c_int,
                drop_fn: unsafe extern "C" fn(*mut c_void) -> c_int,
                get_time_fn: unsafe extern "C" fn(*mut c_void, *mut u64) -> c_int,
                producer: Producer,
                policy: Overflow,
                ready: Arc<ReadySignal>,
                clock_domain: u64,
                coalesce_frames: usize,
                sequence: u64,
                segment: u64,
                frame_cursor: u64,
                armed: Arc<AtomicBool>,
                coalesce_buf: [u8; 960 * 8],
                coalesce_len: usize,
                failed: AtomicBool,
            }

            unsafe extern "C" fn on_ctx_state(_ctx: *mut c_void, userdata: *mut c_void) {
                // SAFETY: `userdata` points to `PaReadCtx`.
                unsafe {
                    let ctx = &*(userdata as *const PaReadCtx);
                    (ctx.signal_fn)(ctx.mainloop, 0);
                }
            }

            unsafe extern "C" fn on_stream_state(_stream: *mut c_void, userdata: *mut c_void) {
                // SAFETY: `userdata` points to `PaReadCtx`.
                unsafe {
                    let ctx = &*(userdata as *const PaReadCtx);
                    (ctx.signal_fn)(ctx.mainloop, 0);
                }
            }

            unsafe extern "C" fn on_stream_read(
                stream: *mut c_void,
                _nbytes: usize,
                userdata: *mut c_void,
            ) {
                // SAFETY: `userdata` points to `PaReadCtx`; no heap allocations occur inside callback.
                unsafe {
                    let ctx = &mut *(userdata as *mut PaReadCtx);
                    loop {
                        let mut data_ptr: *const c_void = std::ptr::null();
                        let mut nbytes: usize = 0;
                        if (ctx.peek_fn)(stream, &mut data_ptr, &mut nbytes) < 0 {
                            ctx.failed.store(true, SeqCst);
                            break;
                        }
                        if nbytes == 0 {
                            break;
                        }
                        if !ctx.armed.load(SeqCst) {
                            let _ = (ctx.drop_fn)(stream);
                            continue;
                        }
                        let acquired = ctx.producer.trace().map(|_| Instant::now());
                        let bpf = ctx.producer.pool().format().bytes_per_frame();
                        if nbytes.is_multiple_of(bpf) {
                            let mut usec: u64 = 0;
                            let ts_ns = if (ctx.get_time_fn)(stream, &mut usec) == 0 && usec > 0 {
                                Some(usec.saturating_mul(1_000))
                            } else {
                                None
                            };
                            let target_chunk_bytes = ctx.coalesce_frames * bpf;
                            let mut byte_off = 0usize;
                            while byte_off < nbytes {
                                let space = target_chunk_bytes - ctx.coalesce_len;
                                let take = space.min(nbytes - byte_off);
                                if data_ptr.is_null() {
                                    ctx.coalesce_buf[ctx.coalesce_len..ctx.coalesce_len + take]
                                        .fill(0);
                                } else {
                                    let src = (data_ptr as *const u8).add(byte_off);
                                    let slice = std::slice::from_raw_parts(src, take);
                                    ctx.coalesce_buf[ctx.coalesce_len..ctx.coalesce_len + take]
                                        .copy_from_slice(slice);
                                }
                                ctx.coalesce_len += take;
                                byte_off += take;
                                if ctx.coalesce_len == target_chunk_bytes {
                                    publish_linux_packet(
                                        &mut ctx.producer,
                                        &ctx.coalesce_buf[..target_chunk_bytes],
                                        false,
                                        false,
                                        ctx.coalesce_frames,
                                        ts_ns,
                                        ctx.clock_domain,
                                        &mut ctx.sequence,
                                        &mut ctx.segment,
                                        &mut ctx.frame_cursor,
                                        ctx.policy,
                                        acquired,
                                    );
                                    ctx.coalesce_len = 0;
                                    ctx.ready.notify();
                                }
                            }
                        }
                        let _ = (ctx.drop_fn)(stream);
                    }
                }
            }

            // SAFETY: owns `pa_threaded_mainloop`, `pa_context`, and `pa_stream` for this session.
            unsafe {
                let mainloop = (lib.pa_threaded_mainloop_new)();
                if mainloop.is_null() {
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }
                let api = (lib.pa_threaded_mainloop_get_api)(mainloop);
                let context = (lib.pa_context_new)(api, c"cspa-pa-stream".as_ptr());
                if context.is_null() {
                    (lib.pa_threaded_mainloop_free)(mainloop);
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                let mut read_ctx = Box::new(PaReadCtx {
                    mainloop,
                    signal_fn: lib.pa_threaded_mainloop_signal,
                    peek_fn: lib.pa_stream_peek,
                    drop_fn: lib.pa_stream_drop,
                    get_time_fn: lib.pa_stream_get_time,
                    producer,
                    policy: overflow,
                    ready: ready_worker.clone(),
                    clock_domain,
                    coalesce_frames,
                    sequence: 0,
                    segment: 1,
                    frame_cursor: 0,
                    armed: armed_worker,
                    coalesce_buf: [0u8; 960 * 8],
                    coalesce_len: 0,
                    failed: AtomicBool::new(false),
                });
                let ctx_ptr: *mut c_void = (&mut *read_ctx as *mut PaReadCtx).cast();
                (lib.pa_context_set_state_callback)(context, Some(on_ctx_state), ctx_ptr);

                if (lib.pa_context_connect)(
                    context,
                    std::ptr::null(),
                    PA_CONTEXT_NOAUTOSPAWN,
                    std::ptr::null(),
                ) < 0
                    || (lib.pa_threaded_mainloop_start)(mainloop) < 0
                {
                    (lib.pa_context_set_state_callback)(context, None, std::ptr::null_mut());
                    (lib.pa_context_disconnect)(context);
                    (lib.pa_context_unref)(context);
                    (lib.pa_threaded_mainloop_free)(mainloop);
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                (lib.pa_threaded_mainloop_lock)(mainloop);
                loop {
                    let st = (lib.pa_context_get_state)(context);
                    if st == PA_CONTEXT_READY {
                        break;
                    }
                    if st == PA_CONTEXT_FAILED || st == PA_CONTEXT_TERMINATED {
                        (lib.pa_context_set_state_callback)(context, None, std::ptr::null_mut());
                        (lib.pa_threaded_mainloop_unlock)(mainloop);
                        (lib.pa_threaded_mainloop_stop)(mainloop);
                        (lib.pa_context_disconnect)(context);
                        (lib.pa_context_unref)(context);
                        (lib.pa_threaded_mainloop_free)(mainloop);
                        let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                        return;
                    }
                    (lib.pa_threaded_mainloop_wait)(mainloop);
                }

                let spec = PaSampleSpec {
                    format: pa_fmt,
                    rate: pa_rate,
                    channels: pa_channels,
                };
                let stream = (lib.pa_stream_new)(
                    context,
                    c"cspa-pa-capture".as_ptr(),
                    &spec,
                    std::ptr::null(),
                );
                if stream.is_null() {
                    (lib.pa_context_set_state_callback)(context, None, std::ptr::null_mut());
                    (lib.pa_context_disconnect)(context);
                    (lib.pa_threaded_mainloop_unlock)(mainloop);
                    (lib.pa_threaded_mainloop_stop)(mainloop);
                    (lib.pa_context_unref)(context);
                    (lib.pa_threaded_mainloop_free)(mainloop);
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                if let Some(sink_input_idx) = monitor_sink_input_index
                    && (lib.pa_stream_set_monitor_stream)(stream, sink_input_idx) < 0
                {
                    (lib.pa_stream_unref)(stream);
                    (lib.pa_context_set_state_callback)(context, None, std::ptr::null_mut());
                    (lib.pa_context_disconnect)(context);
                    (lib.pa_threaded_mainloop_unlock)(mainloop);
                    (lib.pa_threaded_mainloop_stop)(mainloop);
                    (lib.pa_context_unref)(context);
                    (lib.pa_threaded_mainloop_free)(mainloop);
                    let _ = ready_tx.send(Err(CapabilityError::InvalidTarget));
                    return;
                }

                (lib.pa_stream_set_state_callback)(stream, Some(on_stream_state), ctx_ptr);
                (lib.pa_stream_set_read_callback)(stream, Some(on_stream_read), ctx_ptr);

                let chunk_bytes_u32 = (coalesce_frames * stride) as u32;
                let attr = PaBufferAttr {
                    maxlength: (QUEUE_SLOTS as u32) * chunk_bytes_u32,
                    tlength: u32::MAX,
                    prebuf: u32::MAX,
                    minreq: u32::MAX,
                    fragsize: chunk_bytes_u32,
                };
                let dev_cstring = resolved_source
                    .as_deref()
                    .and_then(|s| CString::new(s).ok());
                let dev_ptr = if monitor_sink_input_index.is_some() {
                    std::ptr::null()
                } else {
                    dev_cstring
                        .as_ref()
                        .map_or(std::ptr::null(), |c| c.as_ptr())
                };

