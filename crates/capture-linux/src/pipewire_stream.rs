#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::core::{
    StorageLimits,
    manager::{NativeTreeMixTopology, StorageBudget, StorageReservation},
    measurements::Trace,
    ordering::{self, ControlHandle, Failure, Overflow, Producer, Stop},
    reads::{Cancellation, ReadError, ReadResult, ReadStream},
    schema::{
        BackendId, ByteOrder, CaptureTarget, Format, Header, Layout, Loss, ProcessIdentity,
        ProcessScope, Provenance, SampleType, Stamp, TargetKind,
    },
    storage::Pool,
    stream_runtime::{
        EndReason, ReadySignal, StartupCleanup, StreamReadContext, endpoint_clock_domain,
        poll_read_stream, poll_read_stream_into, signal_stream_stop,
    },
    traits::{AudioStreamBackend, CapabilityError, TimingRequirement, TopologyControllerBackend},
};
use crate::loader::LinuxClientPaths;
use crate::procfs::{collect_proc_stat_table, inspect_process_in_proc_root, pid_belongs_to_target};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::SeqCst},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POOL_SLOTS: usize = 8;
const QUEUE_SLOTS: usize = 4;
const MAX_FRAMES_PER_SLOT: usize = 4096;
const SESSION_METADATA_BYTES: usize = 16_384;

static MIX_SINK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Manager-owned virtual sink and process membership controller; streams do not own it.
pub struct PlatformTopologyController {
    sink_name: String,
    targets: Vec<CaptureTarget>,
    shared_targets: Arc<Mutex<Vec<Option<CaptureTarget>>>>,
    topology: Arc<Mutex<NativeTreeMixTopology>>,
    dirty: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    created_links_count: Arc<AtomicU64>,
    active_links_count: Arc<AtomicU64>,
}

impl PlatformTopologyController {
    pub fn sink_name(&self) -> &str {
        &self.sink_name
    }

    pub fn created_links_count(&self) -> u64 {
        self.created_links_count.load(SeqCst)
    }

    pub fn active_links_count(&self) -> u64 {
        self.active_links_count.load(SeqCst)
    }

    pub fn is_finished(&self) -> bool {
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
        guard.is_none()
    }

    /// Update targets without reopening the stream, retaining shared links and unlinking retired ones.
    pub fn update_targets(
        &mut self,
        new_targets: Vec<CaptureTarget>,
    ) -> Result<Vec<Arc<str>>, CapabilityError> {
        if new_targets.is_empty() {
            return Err(CapabilityError::InvalidTarget);
        }
        for target in &new_targets {
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

        let mut old_used = vec![false; self.targets.len()];
        let mut mapping = Vec::with_capacity(new_targets.len());
        for target in &new_targets {
            let mut matched = None;
            for (old_idx, old_target) in self.targets.iter().enumerate() {
                if !old_used[old_idx]
                    && old_target.logical_id() == target.logical_id()
                    && old_target.scope() == target.scope()
                {
                    old_used[old_idx] = true;
                    matched = Some(old_idx);
                    break;
                }
            }
            mapping.push(matched);
        }

        let unlinked = {
            let mut shared = self
                .shared_targets
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut topo = self.topology.lock().unwrap_or_else(|e| e.into_inner());
            let unlinked = topo.reconfigure_targets(&mapping);
            *shared = new_targets.iter().cloned().map(Some).collect();
            self.active_links_count
                .store(topo.active_link_count() as u64, SeqCst);
            unlinked
        };
        self.dirty.store(true, SeqCst);
        self.targets = new_targets;
        Ok(unlinked)
    }
}

impl TopologyControllerBackend for PlatformTopologyController {
    #[inline]
    fn targets(&self) -> &[CaptureTarget] {
        &self.targets
    }

    fn active_topology_link_count(&self) -> usize {
        self.topology
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_link_count()
    }

    fn retire_target(&self, target_index: usize) -> Vec<Arc<str>> {
        if let Some(slot) = self
            .shared_targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(target_index)
        {
            *slot = None;
        }
        let removed = self
            .topology
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retire_target(target_index);
        self.active_links_count.store(
            self.topology
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active_link_count() as u64,
            SeqCst,
        );
        self.dirty.store(true, SeqCst);
        removed
    }

    fn shutdown(&self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PlatformTopologyController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct PipeWireStream {
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

impl PipeWireStream {
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

    #[cfg(test)]
    pub(crate) fn from_synthetic_packets(
        format: Arc<Format>,
        clock_domain: u64,
        packets: Vec<(Vec<u8>, usize, u64)>,
        budget: &StorageBudget,
    ) -> Result<Self, CapabilityError> {
        let stride = format.bytes_per_frame();
        let limits = StorageLimits::new(
            POOL_SLOTS,
            MAX_FRAMES_PER_SLOT,
            stride,
            POOL_SLOTS * MAX_FRAMES_PER_SLOT * stride,
        )
        .map_err(|_| CapabilityError::CapacityExceeded)?;
        let reservation = budget
            .reserve(limits, 6024)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let retained_reservation = reservation.retain();
        let pool = Pool::new_reserved(limits, MAX_FRAMES_PER_SLOT, format.clone(), reservation)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let (mut producer, reader) =
            ordering::channel(pool, QUEUE_SLOTS).map_err(|_| CapabilityError::CapacityExceeded)?;
        let control = reader.control_handle();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let ready = Arc::new(ReadySignal::new());
        let ready_bg = ready.clone();
        let ended = Arc::new(AtomicU32::new(0));
        let ended_bg = ended.clone();
        let worker = thread::spawn(move || {
            let mut sequence = 0u64;
            let mut segment = 1u64;
            let mut cursor = 0u64;
            for (bytes, frames, ts_ns) in packets {
                if stop_bg.load(SeqCst) || producer.control_handle().stopping() {
                    break;
                }
                while !stop_bg.load(SeqCst) && !producer.control_handle().stopping() {
                    let snap = producer.control_handle().snapshot();
                    if snap.queued < QUEUE_SLOTS as u64 {
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                if stop_bg.load(SeqCst) || producer.control_handle().stopping() {
                    break;
                }
                publish_linux_packet(
                    &mut producer,
                    &bytes,
                    false,
                    false,
                    frames,
                    Some(ts_ns),
                    clock_domain,
                    &mut sequence,
                    &mut segment,
                    &mut cursor,
                    Overflow::DropOldest,
                    None,
                );
                ready_bg.notify();
            }
            let _ = ended_bg.compare_exchange(0, 1, SeqCst, SeqCst);
            producer.close();
            ready_bg.notify();
        });
        Ok(Self {
            format,
            trace: None,
            reads: ReadStream::new(reader),
            control,
            default_cancel: Cancellation::default(),
            armed: Arc::new(AtomicBool::new(true)),
            stop,
            ready,
            ended,
            worker: Mutex::new(Some(worker)),
            _reservation: retained_reservation,
        })
    }
}

impl AudioStreamBackend for PipeWireStream {
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

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
    }
}

pub(crate) fn linux_endpoint_clock_domain(name: &str) -> u64 {
    endpoint_clock_domain(name, 2)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_linux_packet(
    producer: &mut Producer,
    bytes: &[u8],
    silent: bool,
    discontinuity: bool,
    frames: usize,
    timestamp_ns: Option<u64>,
    clock_domain: u64,
    sequence: &mut u64,
    segment: &mut u64,
    frame_cursor: &mut u64,
    policy: Overflow,
    acquired: Option<Instant>,
) {
    if discontinuity {
        *sequence = sequence.saturating_add(1);
        *segment = segment.saturating_add(1);
        let _ = producer.gap(
            *sequence,
            *segment,
            Loss {
                frames: None,
                rate_hz: producer.pool().format().rate(),
            },
        );
    }
    *sequence = sequence.saturating_add(1);
    if let Some(acq) = acquired
        && let Some(trace) = producer.trace()
    {
        trace.record_at(crate::core::measurements::Stage::Acquired, *sequence, acq);
    }
    let pos = *frame_cursor;
    *frame_cursor = frame_cursor.saturating_add(frames as u64);
    let header = Header {
        sequence: *sequence,
        segment: *segment,
        generation: producer.pool().generation(),
        frames,
        sample_position: Some(pos),
        timestamp: timestamp_ns.filter(|&ns| ns > 0).map(|ns| Stamp {
            ticks: ns,
            ticks_per_second: 1_000_000_000,
            clock_domain,
            provenance: Provenance::Native,
            uncertainty_ns: None,
        }),
    };
    if silent {
        let _ = producer.offer_silence(header, policy);
    } else {
        let _ = producer.offer(bytes, header, policy);
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    use crate::loader::{
        PA_SAMPLE_FLOAT32LE, PA_SAMPLE_S16LE, PA_SAMPLE_S24LE, PA_SAMPLE_S32LE, PipeWireLibrary,
        PwBuffer, PwClientEvents, PwClientInfo, PwNodeEvents, PwNodeInfo, PwRegistryEvents,
        PwStreamEvents, PwTime, SpaDict, SpaHook, build_spa_audio_raw_pod,
    };
    use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

    const PW_DIRECTION_INPUT: c_uint = 0;
    const PW_ID_ANY: u32 = 0xffff_ffff;
    const PW_STREAM_FLAG_AUTOCONNECT: c_uint = 1 << 0;
    const PW_STREAM_FLAG_MAP_BUFFERS: c_uint = 1 << 2;
    const PW_STREAM_FLAG_RT_PROCESS: c_uint = 1 << 4;
    const PW_STREAM_STATE_ERROR: c_int = -1;
    const PW_STREAM_STATE_STREAMING: c_int = 4;
    const SPA_CHUNK_FLAG_CORRUPTED: i32 = 1 << 0;
    const SPA_CHUNK_FLAG_EMPTY: i32 = 1 << 1;

    pub fn open_pipewire_point(
        paths: &LinuxClientPaths,
        target_object: Option<&str>,
        capture_sink: bool,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<PipeWireStream, CapabilityError> {
        let format = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48_000,
            2,
            Layout::Known(Arc::from([
                crate::core::schema::Channel::FrontLeft,
                crate::core::schema::Channel::FrontRight,
            ])),
        )
        .map_err(|_| CapabilityError::InvalidTarget)?;
        let clock_domain = if capture_sink {
            1
        } else {
            linux_endpoint_clock_domain(target_object.unwrap_or("pw-default-source"))
        };
        open_pipewire_point_with_format(
            paths,
            target_object,
            capture_sink,
            format,
            clock_domain,
            overflow,
            timing,
            budget,
            observed,
        )
    }

    pub fn open_pipewire_source_point(
        paths: &LinuxClientPaths,
        source_name: Option<&str>,
        format_override: Option<Format>,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<PipeWireStream, CapabilityError> {
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }
        if source_name.is_some_and(|s| s.is_empty() || s.ends_with(".monitor")) {
            return Err(CapabilityError::InvalidTarget);
        }
        let graph = crate::discovery::collect_pipewire_graph(paths)?;
        let resolved_name = match source_name {
            Some(explicit) => {
                if !graph.sources.iter().any(|(_, n, _)| n == explicit) {
                    return Err(CapabilityError::InvalidTarget);
                }
                explicit.to_owned()
            }
            None => graph
                .default_source_name
                .ok_or(CapabilityError::ServerUnavailable)?,
        };

        let resolved_format = match format_override {
            Some(fmt) => fmt,
            None => {
                let pa_match = crate::discovery::collect_pulse_full_state(paths)
                    .ok()
                    .and_then(|(_, sources, _, _)| {
                        sources
                            .into_iter()
                            .find(|s| !s.is_monitor && s.name == resolved_name)
                    });
                if let Some(src) = pa_match {
                    let sample = match src.sample_format {
                        PA_SAMPLE_S16LE => SampleType::I16,
                        PA_SAMPLE_S24LE => SampleType::I24,
                        PA_SAMPLE_S32LE => SampleType::I32,
                        PA_SAMPLE_FLOAT32LE => SampleType::F32,
                        _ => SampleType::F32,
                    };
                    let (ch, layout) = if src.channels == 1 {
                        (
                            1u16,
                            Layout::Known(Arc::from([crate::core::schema::Channel::Mono])),
                        )
                    } else {
                        (
                            2u16,
                            Layout::Known(Arc::from([
                                crate::core::schema::Channel::FrontLeft,
                                crate::core::schema::Channel::FrontRight,
                            ])),
                        )
                    };
                    let rate = if (8_000..=192_000).contains(&src.rate_hz) {
                        src.rate_hz
                    } else {
                        48_000
                    };
                    Format::new(sample, ByteOrder::Little, rate, ch, layout)
                        .map_err(|_| CapabilityError::InvalidTarget)?
                } else {
                    Format::new(
                        SampleType::F32,
                        ByteOrder::Little,
                        48_000,
                        2,
                        Layout::Known(Arc::from([
                            crate::core::schema::Channel::FrontLeft,
                            crate::core::schema::Channel::FrontRight,
                        ])),
                    )
                    .map_err(|_| CapabilityError::InvalidTarget)?
                }
            }
        };

        open_pipewire_point_with_format(
            paths,
            Some(&resolved_name),
            false,
            resolved_format,
            linux_endpoint_clock_domain(&resolved_name),
            overflow,
            timing,
            budget,
            observed,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_pipewire_point_with_format(
        paths: &LinuxClientPaths,
        target_object: Option<&str>,
        capture_sink: bool,
        format: Format,
        clock_domain: u64,
        overflow: Overflow,
        timing: TimingRequirement,
        budget: &StorageBudget,
        observed: bool,
    ) -> Result<PipeWireStream, CapabilityError> {
        if timing == TimingRequirement::Precise {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }
        if !capture_sink && target_object.is_some_and(|t| t.ends_with(".monitor")) {
            return Err(CapabilityError::InvalidTarget);
        }
        if capture_sink
            && let Some(explicit_sink) = target_object
            && !explicit_sink.starts_with("cspa-mix-")
            && let Ok(graph) = crate::discovery::collect_pipewire_graph(paths)
            && !graph.sinks.iter().any(|(_, n, _)| n == explicit_sink)
        {
            return Err(CapabilityError::InvalidTarget);
        }
        let pod = build_spa_audio_raw_pod(format.sample(), format.rate(), format.channels())?;
        let lib = Arc::new(PipeWireLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PipeWire, paths.pipewire_min_version)
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
        let target_str = target_object.map(str::to_owned);
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), CapabilityError>>(1);

        let worker = thread::spawn(move || {
            struct RtContext {
                stream: *mut c_void,
                dequeue_fn: unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer,
                queue_fn: unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int,
                get_time_fn: unsafe extern "C" fn(*mut c_void, *mut PwTime, usize) -> c_int,
                producer: Producer,
                policy: Overflow,
                armed: Arc<AtomicBool>,
                ready: Arc<ReadySignal>,
                clock_domain: u64,
                sequence: u64,
                segment: u64,
                frame_cursor: u64,
                failed: AtomicBool,
                streaming: AtomicBool,
            }

            unsafe extern "C" fn on_state_changed(
                data: *mut c_void,
                _old: c_int,
                state: c_int,
                _error: *const c_char,
            ) {
                // SAFETY: `data` points to `RtContext`.
                unsafe {
                    let ctx = &*(data as *const RtContext);
                    if state == PW_STREAM_STATE_STREAMING {
                        ctx.streaming.store(true, SeqCst);
                    } else if state == PW_STREAM_STATE_ERROR {
                        ctx.failed.store(true, SeqCst);
                    }
                }
            }

            unsafe extern "C" fn on_process(data: *mut c_void) {
                // SAFETY: `data` points to `RtContext` pinned for the stream's lifetime;
                // no heap allocations occur inside this RT callback.
                unsafe {
                    let ctx = &mut *(data as *mut RtContext);
                    ctx.streaming.store(true, SeqCst);
                    let buf = (ctx.dequeue_fn)(ctx.stream);
                    if buf.is_null() {
                        return;
                    }
                    if !ctx.armed.load(SeqCst) {
                        let _ = (ctx.queue_fn)(ctx.stream, buf);
                        return;
                    }
                    let acquired = ctx.producer.trace().map(|_| Instant::now());
                    let pw_buf = &*buf;
                    if !pw_buf.buffer.is_null() {
                        let spa_buf = &*pw_buf.buffer;
                        if spa_buf.n_datas >= 1 && !spa_buf.datas.is_null() {
                            let plane = &*spa_buf.datas;
                            if !plane.chunk.is_null() {
                                let chunk = &*plane.chunk;
                                let bytes = chunk.size as usize;
                                let offset = chunk.offset as usize;
                                let bpf = ctx.producer.pool().format().bytes_per_frame();
                                if bytes > 0 && bytes.is_multiple_of(bpf) {
                                    let total_frames = bytes / bpf;
                                    let mut time_info = PwTime::default();
                                    let ts_ns = if (ctx.get_time_fn)(
                                        ctx.stream,
                                        &mut time_info,
                                        std::mem::size_of::<PwTime>(),
                                    ) == 0
                                        && time_info.now > 0
                                    {
                                        Some(time_info.now as u64)
                                    } else {
                                        None
                                    };
                                    let is_empty = (chunk.flags & SPA_CHUNK_FLAG_EMPTY) != 0
                                        || plane.data.is_null();
                                    let is_corrupt = (chunk.flags & SPA_CHUNK_FLAG_CORRUPTED) != 0;
                                    let mut rem = total_frames;
                                    let mut byte_off = offset;
                                    let mut first = true;
                                    while rem > 0 {
                                        let chunk_frames = rem.min(MAX_FRAMES_PER_SLOT);
                                        let chunk_bytes = chunk_frames * bpf;
                                        if is_empty {
                                            publish_linux_packet(
                                                &mut ctx.producer,
                                                &[],
                                                true,
                                                first && is_corrupt,
                                                chunk_frames,
                                                ts_ns,
                                                ctx.clock_domain,
                                                &mut ctx.sequence,
                                                &mut ctx.segment,
                                                &mut ctx.frame_cursor,
                                                ctx.policy,
                                                acquired,
                                            );
                                        } else if byte_off
                                            .checked_add(chunk_bytes)
                                            .is_some_and(|end| end <= plane.maxsize as usize)
                                        {
                                            let src_ptr = (plane.data as *const u8).add(byte_off);
                                            let slice =
                                                std::slice::from_raw_parts(src_ptr, chunk_bytes);
                                            publish_linux_packet(
                                                &mut ctx.producer,
                                                slice,
                                                false,
                                                first && is_corrupt,
                                                chunk_frames,
                                                ts_ns,
                                                ctx.clock_domain,
                                                &mut ctx.sequence,
                                                &mut ctx.segment,
                                                &mut ctx.frame_cursor,
                                                ctx.policy,
                                                acquired,
                                            );
                                        }
                                        first = false;
                                        rem -= chunk_frames;
                                        byte_off += chunk_bytes;
                                    }
                                    ctx.ready.notify();
                                }
                            }
                        }
                    }
                    let _ = (ctx.queue_fn)(ctx.stream, buf);
                }
            }

            // SAFETY: owns `pw_thread_loop`, `pw_context`, `pw_core`, and `pw_stream` for this session.
            unsafe {
                let tloop = (lib.pw_thread_loop_new)(c"cspa-pw-stream".as_ptr(), std::ptr::null());
                if tloop.is_null() {
                    let _ = init_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }
                let loop_obj = (lib.pw_thread_loop_get_loop)(tloop);
                let context = (lib.pw_context_new)(loop_obj, std::ptr::null_mut(), 0);
                if context.is_null() || (lib.pw_thread_loop_start)(tloop) < 0 {
                    if !context.is_null() {
                        (lib.pw_context_destroy)(context);
                    }
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = init_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }
                (lib.pw_thread_loop_lock)(tloop);
                let core = (lib.pw_context_connect)(context, std::ptr::null_mut(), 0);
                if core.is_null() {
                    (lib.pw_thread_loop_unlock)(tloop);
                    (lib.pw_thread_loop_stop)(tloop);
                    (lib.pw_context_destroy)(context);
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = init_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                let mut prop_pairs = vec![
                    ("media.type", "Audio"),
                    ("media.category", "Capture"),
                    ("node.want-driver", "false"),
                    ("node.dont-reconnect", "true"),
                ];
                if capture_sink {
                    prop_pairs.push(("stream.capture.sink", "true"));
                }
                if let Some(ref target) = target_str {
                    prop_pairs.push(("target.object", target.as_str()));
                }
                let props = lib.new_properties(&prop_pairs);
                let stream = (lib.pw_stream_new)(core, c"cspa-capture".as_ptr(), props);
                if stream.is_null() {
                    (lib.pw_core_disconnect)(core);
                    (lib.pw_thread_loop_unlock)(tloop);
                    (lib.pw_thread_loop_stop)(tloop);
                    (lib.pw_context_destroy)(context);
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = init_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                let mut rt_ctx = Box::new(RtContext {
                    stream,
                    dequeue_fn: lib.pw_stream_dequeue_buffer,
                    queue_fn: lib.pw_stream_queue_buffer,
                    get_time_fn: lib.pw_stream_get_time_n,
                    producer,
                    policy: overflow,
                    armed: armed_worker,
                    ready: ready_worker.clone(),
                    clock_domain,
                    sequence: 0,
                    segment: 1,
                    frame_cursor: 0,
                    failed: AtomicBool::new(false),
                    streaming: AtomicBool::new(false),
                });
                let events = PwStreamEvents {
                    version: 2,
                    destroy: None,
                    state_changed: Some(on_state_changed),
                    control_info: None,
                    io_changed: None,
                    param_changed: None,
                    add_buffer: None,
                    remove_buffer: None,
                    process: Some(on_process),
                    drained: None,
                    command: None,
                    trigger_done: None,
                };
                let mut stream_hook = SpaHook::default();
                let rt_ptr: *mut c_void = (&mut *rt_ctx as *mut RtContext).cast();
                (lib.pw_stream_add_listener)(stream, &mut stream_hook, &events, rt_ptr);

                let mut params: [*const c_void; 1] = [pod.0.as_ptr().cast()];
                let flags = PW_STREAM_FLAG_AUTOCONNECT
                    | PW_STREAM_FLAG_MAP_BUFFERS
                    | PW_STREAM_FLAG_RT_PROCESS;
                let conn_rc = (lib.pw_stream_connect)(
                    stream,
                    PW_DIRECTION_INPUT,
                    PW_ID_ANY,
                    flags,
                    params.as_mut_ptr(),
                    1,
                );
                (lib.pw_thread_loop_unlock)(tloop);

                if conn_rc < 0 {
                    (lib.pw_thread_loop_stop)(tloop);
                    (lib.pw_stream_destroy)(stream);
                    (lib.pw_core_disconnect)(core);
                    (lib.pw_context_destroy)(context);
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = init_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                let start_wait = Instant::now();
                while !rt_ctx.streaming.load(SeqCst)
                    && !rt_ctx.failed.load(SeqCst)
                    && start_wait.elapsed() < Duration::from_secs(3)
                {
                    thread::sleep(Duration::from_millis(5));
                }
                if rt_ctx.failed.load(SeqCst) {
                    (lib.pw_thread_loop_stop)(tloop);
                    (lib.pw_stream_destroy)(stream);
                    (lib.pw_core_disconnect)(core);
                    (lib.pw_context_destroy)(context);
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = init_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                let _ = init_tx.send(Ok(()));

                while !stop_worker.load(SeqCst)
                    && !rt_ctx.producer.control_handle().stopping()
                    && !rt_ctx.failed.load(SeqCst)
                {
                    thread::sleep(Duration::from_millis(5));
                }

                (lib.pw_thread_loop_lock)(tloop);
                let _ = (lib.pw_stream_disconnect)(stream);
                (lib.pw_thread_loop_unlock)(tloop);
                (lib.pw_thread_loop_stop)(tloop);

                if rt_ctx.failed.load(SeqCst) {
                    let _ = ended_worker.compare_exchange(0, 3, SeqCst, SeqCst);
                    rt_ctx.producer.fail_capture(Failure::Backend);
                } else {
                    let _ = ended_worker.compare_exchange(0, 1, SeqCst, SeqCst);
                }
                rt_ctx.producer.close();
                ready_worker.notify();

