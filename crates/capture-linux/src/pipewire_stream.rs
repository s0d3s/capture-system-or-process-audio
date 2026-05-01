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

                (lib.pw_stream_destroy)(stream);
                (lib.pw_core_disconnect)(core);
                (lib.pw_context_destroy)(context);
                (lib.pw_thread_loop_destroy)(tloop);
            }
        });

        match init_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                let control = reader.control_handle();
                Ok(PipeWireStream {
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
                let cleanup = StartupCleanup::new(stop, worker);
                let _ = cleanup.wait(Duration::from_millis(500));
                Err(CapabilityError::ServerUnavailable)
            }
        }
    }

    /// Start a `Manager`-owned `PlatformTopologyController` that creates a virtual
    /// `support.null-audio-sink` and maintains passive non-lingering `FL`/`FR` port links
    /// for all eligible nodes matching `targets` (`scope = "tree" | "exact"`).
    pub fn start_pipewire_topology_controller(
        paths: &LinuxClientPaths,
        targets: Vec<CaptureTarget>,
        max_links: usize,
    ) -> Result<PlatformTopologyController, CapabilityError> {
        if targets.is_empty() {
            return Err(CapabilityError::InvalidTarget);
        }
        for target in &targets {
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

        let lib = Arc::new(PipeWireLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PipeWire, paths.pipewire_min_version)
        })?);
        let seq = MIX_SINK_COUNTER.fetch_add(1, SeqCst);
        let sink_name = format!("cspa-mix-{}-{seq}", std::process::id());
        let topology = Arc::new(Mutex::new(
            NativeTreeMixTopology::new(max_links.clamp(1, 256))
                .map_err(|_| CapabilityError::CapacityExceeded)?,
        ));
        let dirty = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let created_links_count = Arc::new(AtomicU64::new(0));
        let active_links_count = Arc::new(AtomicU64::new(0));

        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), CapabilityError>>(1);
        let sink_name_worker = sink_name.clone();
        let shared_targets: Arc<Mutex<Vec<Option<CaptureTarget>>>> =
            Arc::new(Mutex::new(targets.iter().cloned().map(Some).collect()));
        let targets_worker = shared_targets.clone();
        let topology_worker = topology.clone();
        let dirty_worker = dirty.clone();
        let stop_worker = stop.clone();
        let created_worker = created_links_count.clone();
        let active_worker = active_links_count.clone();

        let worker = thread::spawn(move || {
            struct BoundObj {
                proxy: *mut c_void,
                _hook: Box<SpaHook>,
            }

            struct GraphState<'a> {
                lib: &'a PipeWireLibrary,
                registry: *mut c_void,
                sink_name: String,
                sink_node_id: Option<u32>,
                sink_in_fl: Option<u32>,
                sink_in_fr: Option<u32>,
                client_app_pids: BTreeMap<u32, u32>,
                client_sec_pids: BTreeMap<u32, u32>,
                output_nodes: BTreeMap<u32, (Option<u32>, Option<u32>)>,
                output_ports: BTreeMap<u32, (u32, usize)>,
                bound_objs: BTreeMap<u32, BoundObj>,
                dirty: Arc<AtomicBool>,
            }

            static NODE_EVENTS: PwNodeEvents = PwNodeEvents {
                version: 0,
                info: Some(on_node_info),
                param: None,
            };

            static CLIENT_EVENTS: PwClientEvents = PwClientEvents {
                version: 0,
                info: Some(on_client_info),
                permissions: None,
            };

            unsafe extern "C" fn on_node_info(data: *mut c_void, info: *const PwNodeInfo) {
                // SAFETY: `data` points to `GraphState` and `info` is valid during callback.
                unsafe {
                    if info.is_null() {
                        return;
                    }
                    let st = &mut *(data as *mut GraphState<'_>);
                    let inf = &*info;
                    if inf.props.is_null() {
                        return;
                    }
                    let app_pid = SpaDict::lookup(inf.props, "application.process.id")
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    let client_id = SpaDict::lookup(inf.props, "client.id")
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    let entry = st.output_nodes.entry(inf.id).or_insert((None, None));
                    if client_id.is_some() {
                        entry.0 = client_id;
                    }
                    if app_pid.is_some() {
                        entry.1 = app_pid;
                    }
                    st.dirty.store(true, SeqCst);
                }
            }

            unsafe extern "C" fn on_client_info(data: *mut c_void, info: *const PwClientInfo) {
                // SAFETY: `data` points to `GraphState` and `info` is valid during callback.
                unsafe {
                    if info.is_null() {
                        return;
                    }
                    let st = &mut *(data as *mut GraphState<'_>);
                    let inf = &*info;
                    if inf.props.is_null() {
                        return;
                    }
                    if let Some(pid_txt) = SpaDict::lookup(inf.props, "application.process.id")
                        && let Ok(pid) = pid_txt.trim().parse::<u32>()
                    {
                        st.client_app_pids.insert(inf.id, pid);
                    }
                    if let Some(sec_txt) = SpaDict::lookup(inf.props, "pipewire.sec.pid")
                        && let Ok(sec_pid) = sec_txt.trim().parse::<u32>()
                    {
                        st.client_sec_pids.insert(inf.id, sec_pid);
                    }
                    st.dirty.store(true, SeqCst);
                }
            }

            unsafe extern "C" fn on_global(
                data: *mut c_void,
                id: u32,
                _permissions: u32,
                type_ptr: *const c_char,
                _version: u32,
                props: *const SpaDict,
            ) {
                // SAFETY: `data` points to `GraphState`.
                unsafe {
                    if type_ptr.is_null() || props.is_null() {
                        return;
                    }
                    let st = &mut *(data as *mut GraphState<'_>);
                    let Ok(type_str) = CStr::from_ptr(type_ptr).to_str() else {
                        return;
                    };
                    match type_str {
                        "PipeWire:Interface:Client" => {
                            if let Some(pid_txt) = SpaDict::lookup(props, "application.process.id")
                                && let Ok(pid) = pid_txt.trim().parse::<u32>()
                            {
                                st.client_app_pids.insert(id, pid);
                            }
                            if let Some(sec_txt) = SpaDict::lookup(props, "pipewire.sec.pid")
                                && let Ok(sec_pid) = sec_txt.trim().parse::<u32>()
                            {
                                st.client_sec_pids.insert(id, sec_pid);
                            }
                            let proxy = st.lib.registry_bind(
                                st.registry,
                                id,
                                c"PipeWire:Interface:Client".as_ptr(),
                                3,
                                0,
                            );
                            if !proxy.is_null() {
                                let mut hook = Box::new(SpaHook::default());
                                (st.lib.pw_proxy_add_object_listener)(
                                    proxy,
                                    &mut *hook,
                                    (&CLIENT_EVENTS as *const PwClientEvents).cast(),
                                    data,
                                );
                                st.bound_objs.insert(id, BoundObj { proxy, _hook: hook });
                            }
                            st.dirty.store(true, SeqCst);
                        }
                        "PipeWire:Interface:Node" => {
                            let name = SpaDict::lookup(props, "node.name").unwrap_or_default();
                            if name == st.sink_name {
                                st.sink_node_id = Some(id);
                                st.dirty.store(true, SeqCst);
                                return;
                            }
                            let media_class =
                                SpaDict::lookup(props, "media.class").unwrap_or_default();
                            if media_class == "Stream/Output/Audio" {
                                let client_id = SpaDict::lookup(props, "client.id")
                                    .and_then(|s| s.trim().parse::<u32>().ok());
                                let app_pid = SpaDict::lookup(props, "application.process.id")
                                    .and_then(|s| s.trim().parse::<u32>().ok());
                                st.output_nodes.insert(id, (client_id, app_pid));
                                let proxy = st.lib.registry_bind(
                                    st.registry,
                                    id,
                                    c"PipeWire:Interface:Node".as_ptr(),
                                    3,
                                    0,
                                );
                                if !proxy.is_null() {
                                    let mut hook = Box::new(SpaHook::default());
                                    (st.lib.pw_proxy_add_object_listener)(
                                        proxy,
                                        &mut *hook,
                                        (&NODE_EVENTS as *const PwNodeEvents).cast(),
                                        data,
                                    );
                                    st.bound_objs.insert(id, BoundObj { proxy, _hook: hook });
                                }
                                st.dirty.store(true, SeqCst);
                            }
                        }
                        "PipeWire:Interface:Port" => {
                            let Some(node_id) = SpaDict::lookup(props, "node.id")
                                .and_then(|s| s.trim().parse::<u32>().ok())
                            else {
                                return;
                            };
                            let direction =
                                SpaDict::lookup(props, "port.direction").unwrap_or_default();
                            let channel =
                                SpaDict::lookup(props, "audio.channel").unwrap_or_default();
                            let ch_idx = if channel == "FL" || channel == "MONO" {
                                Some(0)
                            } else if channel == "FR" {
                                Some(1)
                            } else {
                                None
                            };
                            let Some(ch) = ch_idx else {
                                return;
                            };
                            if Some(node_id) == st.sink_node_id && direction == "in" {
                                if ch == 0 {
                                    st.sink_in_fl = Some(id);
                                } else {
                                    st.sink_in_fr = Some(id);
                                }
                                st.dirty.store(true, SeqCst);
                            } else if direction == "out" {
                                st.output_ports.insert(id, (node_id, ch));
                                st.dirty.store(true, SeqCst);
                            }
                        }
                        _ => {}
                    }
                }
            }

            unsafe extern "C" fn on_global_remove(data: *mut c_void, id: u32) {
                // SAFETY: `data` points to `GraphState`.
                unsafe {
                    let st = &mut *(data as *mut GraphState<'_>);
                    st.output_nodes.remove(&id);
                    st.output_ports.remove(&id);
                    st.client_app_pids.remove(&id);
                    if let Some(bound) = st.bound_objs.remove(&id)
                        && !bound.proxy.is_null()
                    {
                        (st.lib.pw_proxy_destroy)(bound.proxy);
                    }
                    st.dirty.store(true, SeqCst);
                }
            }

            // SAFETY: owns `pw_thread_loop`, `pw_context`, `pw_core`, `registry`, `sink`, and `link` proxies.
            unsafe {
                let tloop = (lib.pw_thread_loop_new)(c"cspa-pw-topo".as_ptr(), std::ptr::null());
                if tloop.is_null() {
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }
                let loop_obj = (lib.pw_thread_loop_get_loop)(tloop);
                let context = (lib.pw_context_new)(loop_obj, std::ptr::null_mut(), 0);
                if context.is_null() || (lib.pw_thread_loop_start)(tloop) < 0 {
                    if !context.is_null() {
                        (lib.pw_context_destroy)(context);
                    }
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                (lib.pw_thread_loop_lock)(tloop);
                let core = (lib.pw_context_connect)(context, std::ptr::null_mut(), 0);
                if core.is_null() {
                    (lib.pw_thread_loop_unlock)(tloop);
                    (lib.pw_thread_loop_stop)(tloop);
                    (lib.pw_context_destroy)(context);
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }
                let registry = lib.core_get_registry(core, 3, 0);
                let mut cb_state = Box::new(GraphState {
                    lib: &lib,
                    registry,
                    sink_name: sink_name_worker.clone(),
                    sink_node_id: None,
                    sink_in_fl: None,
                    sink_in_fr: None,
                    client_app_pids: BTreeMap::new(),
                    client_sec_pids: BTreeMap::new(),
                    output_nodes: BTreeMap::new(),
                    output_ports: BTreeMap::new(),
                    bound_objs: BTreeMap::new(),
                    dirty: dirty_worker.clone(),
                });
                let reg_events = PwRegistryEvents {
                    version: 0,
                    global: Some(on_global),
                    global_remove: Some(on_global_remove),
                };
                let mut reg_hook = SpaHook::default();
                lib.registry_add_listener(
                    registry,
                    &mut reg_hook,
                    &reg_events,
                    (&mut *cb_state as *mut GraphState<'_>).cast(),
                );

                let sink_props = lib.new_properties(&[
                    ("factory.name", "support.null-audio-sink"),
                    ("node.name", sink_name_worker.as_str()),
                    ("media.class", "Stream/Input/Audio"),
                    ("node.virtual", "true"),
                    ("object.linger", "false"),
                    ("audio.channels", "2"),
                    ("audio.position", "[ FL FR ]"),
                ]);
                let sink_proxy = lib.core_create_object(
                    core,
                    c"adapter".as_ptr(),
                    c"PipeWire:Interface:Node".as_ptr(),
                    3,
                    &(*sink_props).dict,
                    0,
                );
                (lib.pw_properties_free)(sink_props);
                let _ = lib.core_sync(core, 0, 1);
                (lib.pw_thread_loop_unlock)(tloop);

                if sink_proxy.is_null() {
                    (lib.pw_thread_loop_stop)(tloop);
                    (lib.pw_proxy_destroy)(registry);
                    (lib.pw_core_disconnect)(core);
                    (lib.pw_context_destroy)(context);
                    (lib.pw_thread_loop_destroy)(tloop);
                    let _ = ready_tx.send(Err(CapabilityError::ServerUnavailable));
                    return;
                }

                let mut link_proxies: BTreeMap<Arc<str>, *mut c_void> = BTreeMap::new();

                let mut reconcile_once = || {
                    let Ok(proc_map) = collect_proc_stat_table(Path::new("/proc")) else {
                        return;
                    };

                    (lib.pw_thread_loop_lock)(tloop);
                    let (Some(in_fl), Some(in_fr)) = (cb_state.sink_in_fl, cb_state.sink_in_fr)
                    else {
                        cb_state.dirty.store(true, SeqCst);
                        (lib.pw_thread_loop_unlock)(tloop);
                        return;
                    };
                    let sink_inputs = [in_fl, in_fr];
                    let mut changed = false;

                    {
                        let active_set: std::collections::BTreeSet<Arc<str>> = topology_worker
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .active_sources()
                            .into_iter()
                            .collect();
                        let mut to_remove = Vec::new();
                        for key in link_proxies.keys() {
                            if !active_set.contains(key) {
                                to_remove.push(key.clone());
                            }
                        }
                        for key in to_remove {
                            if let Some(proxy) = link_proxies.remove(&key)
                                && !proxy.is_null()
                            {
                                (lib.pw_proxy_destroy)(proxy);
                                changed = true;
                            }
                        }
                    }

                    for (&out_port_id, &(out_node_id, ch)) in &cb_state.output_ports {
                        let Some(&(client_id, node_app_pid)) =
                            cb_state.output_nodes.get(&out_node_id)
                        else {
                            continue;
                        };
                        let Some(node_pid) = crate::discovery::resolve_pipewire_node_pid(
                            node_app_pid,
                            client_id,
                            &cb_state.client_app_pids,
                            &cb_state.client_sec_pids,
                        ) else {
                            continue;
                        };

                        let current_targets = targets_worker
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let mut matching_targets = Vec::new();
                        for (t_idx, target_opt) in current_targets.iter().enumerate() {
                            let Some(target) = target_opt else {
                                continue;
                            };
                            if let Some(root_pid) = target.pid() {
                                let root_id = ProcessIdentity {
                                    pid: root_pid,
                                    generation: target.process_start_time().unwrap_or(0),
                                };
                                let exact_only = target.scope() == ProcessScope::Exact;
                                if pid_belongs_to_target(&proc_map, root_id, exact_only, node_pid)
                                    == Ok(true)
                                {
                                    matching_targets.push(t_idx);
                                }
                            }
                        }
                        if matching_targets.is_empty() {
                            continue;
                        }

                        let in_port_id = sink_inputs[ch];
                        let link_key: Arc<str> =
                            format!("{out_node_id}:{out_port_id}->{in_port_id}").into();
                        let mut created_in_topo = false;
                        {
                            let mut topo =
                                topology_worker.lock().unwrap_or_else(|e| e.into_inner());
                            for &t_idx in &matching_targets {
                                if let Ok(is_new) = topo.attach_source(t_idx, link_key.clone())
                                    && is_new
                                {
                                    created_in_topo = true;
                                }
                            }
                            active_worker.store(topo.active_link_count() as u64, SeqCst);
                        }

                        if created_in_topo && !link_proxies.contains_key(&link_key) {
                            let out_node_s = out_node_id.to_string();
                            let out_port_s = out_port_id.to_string();
                            let in_node_s = cb_state.sink_node_id.unwrap_or(0).to_string();
                            let in_port_s = in_port_id.to_string();
                            let lprops = lib.new_properties(&[
                                ("object.linger", "false"),
                                ("link.output.node", out_node_s.as_str()),
                                ("link.output.port", out_port_s.as_str()),
                                ("link.input.node", in_node_s.as_str()),
                                ("link.input.port", in_port_s.as_str()),
                            ]);
                            let lproxy = lib.core_create_object(
                                core,
                                c"link-factory".as_ptr(),
                                c"PipeWire:Interface:Link".as_ptr(),
                                3,
                                &(*lprops).dict,
                                0,
                            );
                            (lib.pw_properties_free)(lprops);
                            if !lproxy.is_null() {
                                link_proxies.insert(link_key, lproxy);
                                created_worker.fetch_add(1, SeqCst);
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        let _ = lib.core_sync(core, 0, 2);
                    }
                    (lib.pw_thread_loop_unlock)(tloop);
                };

                // Wait up to 500ms for virtual sink FL/FR input ports and initial node links.
                let wait_start = Instant::now();
                loop {
                    let _ = dirty_worker.swap(false, SeqCst);
                    reconcile_once();
                    let linked = active_worker.load(SeqCst);
                    if linked > 0 || wait_start.elapsed() >= Duration::from_millis(500) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let _ = ready_tx.send(Ok(()));

                while !stop_worker.load(SeqCst) {
                    if dirty_worker.swap(false, SeqCst) {
                        reconcile_once();
                    }
                    thread::sleep(Duration::from_millis(10));
                }

                (lib.pw_thread_loop_lock)(tloop);
                for (_k, proxy) in link_proxies {
                    if !proxy.is_null() {
                        (lib.pw_proxy_destroy)(proxy);
                    }
                }
                for (_id, bound) in std::mem::take(&mut cb_state.bound_objs) {
                    if !bound.proxy.is_null() {
                        (lib.pw_proxy_destroy)(bound.proxy);
                    }
                }
                (lib.pw_proxy_destroy)(sink_proxy);
                (lib.pw_proxy_destroy)(registry);
                (lib.pw_core_disconnect)(core);
                (lib.pw_thread_loop_unlock)(tloop);
                (lib.pw_thread_loop_stop)(tloop);
                (lib.pw_context_destroy)(context);
                (lib.pw_thread_loop_destroy)(tloop);
            }
        });

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(PlatformTopologyController {
                sink_name,
                targets,
                shared_targets,
                topology,
                dirty,
                stop,
                worker: Mutex::new(Some(worker)),
                created_links_count,
                active_links_count,
            }),
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

#[cfg(target_os = "linux")]
pub use imp::{
    open_pipewire_point, open_pipewire_source_point, start_pipewire_topology_controller,
};

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub fn open_pipewire_point_with_format(
    paths: &LinuxClientPaths,
    _target_object: Option<&str>,
    _capture_sink: bool,
    _format: Format,
    _clock_domain: u64,
    _overflow: Overflow,
    _timing: TimingRequirement,
    _budget: &StorageBudget,
    _observed: bool,
) -> Result<PipeWireStream, CapabilityError> {
    let _ = linux_endpoint_clock_domain("stub");
    Err(
        crate::loader::ClientLoadError::LibraryAbsent(paths.pipewire_so.clone())
            .into_capability_error(BackendId::PipeWire, paths.pipewire_min_version),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn open_pipewire_source_point(
    paths: &LinuxClientPaths,
    _source_name: Option<&str>,
    _format_override: Option<Format>,
    _overflow: Overflow,
    _timing: TimingRequirement,
    _budget: &StorageBudget,
    _observed: bool,
) -> Result<PipeWireStream, CapabilityError> {
    Err(
        crate::loader::ClientLoadError::LibraryAbsent(paths.pipewire_so.clone())
            .into_capability_error(BackendId::PipeWire, paths.pipewire_min_version),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn open_pipewire_point(
    paths: &LinuxClientPaths,
    _target_object: Option<&str>,
    _capture_sink: bool,
    _overflow: Overflow,
    _timing: TimingRequirement,
    _budget: &StorageBudget,
    _observed: bool,
) -> Result<PipeWireStream, CapabilityError> {
    let _ = (
        POOL_SLOTS,
        QUEUE_SLOTS,
        MAX_FRAMES_PER_SLOT,
        SESSION_METADATA_BYTES,
        &MIX_SINK_COUNTER,
        StartupCleanup::new,
        publish_linux_packet
            as fn(
                &mut Producer,
                &[u8],
                bool,
                bool,
                usize,
                Option<u64>,
                u64,
                &mut u64,
                &mut u64,
                &mut u64,
                Overflow,
                Option<Instant>,
            ),
    );
    let _ = (
        collect_proc_stat_table,
        inspect_process_in_proc_root,
        pid_belongs_to_target,
    );
    let _ = (
        std::mem::size_of::<BTreeMap<u32, u32>>(),
        Path::new(""),
        ProcessIdentity {
            pid: 1,
            generation: 1,
        },
        ProcessScope::Tree,
        TargetKind::Process,
        Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48_000,
            2,
            Layout::Unknown,
        ),
        StorageLimits::new(8, 4096, 8, 262_144),
        Pool::new_reserved,
        ordering::channel,
        ReadStream::new,
    );
    Err(
        crate::loader::ClientLoadError::LibraryAbsent(paths.pipewire_so.clone())
            .into_capability_error(BackendId::PipeWire, paths.pipewire_min_version),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn start_pipewire_topology_controller(
    paths: &LinuxClientPaths,
    _targets: Vec<CaptureTarget>,
    _max_links: usize,
) -> Result<PlatformTopologyController, CapabilityError> {
    Err(
        crate::loader::ClientLoadError::LibraryAbsent(paths.pipewire_so.clone())
            .into_capability_error(BackendId::PipeWire, paths.pipewire_min_version),
    )
}
