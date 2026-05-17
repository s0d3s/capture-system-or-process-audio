#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::core::{
    StorageLimits,
    manager::{NativeTreeMixTopology, StorageBudget, StorageReservation},
    measurements::Trace,
    ordering::{self, ControlHandle, Failure, Overflow, Producer, Stop},
    reads::{Cancellation, ReadError, ReadResult, ReadStream},
    schema::{
        ByteOrder, CaptureTarget, Channel, Format, Header, Layout, Loss, ProcessIdentity,
        ProcessScope, Provenance, SampleType, Stamp, TargetKind,
    },
    storage::Pool,
    stream_runtime::{
        EndReason, ReadySignal, StreamReadContext, endpoint_clock_domain, poll_read_stream,
        poll_read_stream_into, signal_stream_stop,
    },
    traits::{AudioStreamBackend, CapabilityError, TimingRequirement, TopologyControllerBackend},
};
use crate::discovery::collect_live_coreaudio_process_objects;
use crate::proc_info::pid_belongs_to_target;
use std::collections::BTreeSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::SeqCst},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) const POOL_SLOTS: usize = 8;
pub(crate) const QUEUE_SLOTS: usize = 4;
pub(crate) const MAX_FRAMES_PER_SLOT: usize = 4096;
pub(crate) const SESSION_METADATA_BYTES: usize = 16_384;

/// Pack planar or interleaved samples into interleaved bytes without heap allocation.
pub fn pack_planar_or_interleaved_f32_into_bytes(
    channel_planes: &[(&[f32], usize)],
    frames: usize,
    total_channels: usize,
    dst_interleaved_bytes: &mut [u8],
) -> Result<usize, CapabilityError> {
    if frames == 0 || total_channels == 0 {
        return Ok(0);
    }
    let total_samples = frames
        .checked_mul(total_channels)
        .ok_or(CapabilityError::InvalidTarget)?;
    let required_bytes = total_samples
        .checked_mul(4)
        .ok_or(CapabilityError::InvalidTarget)?;
    if dst_interleaved_bytes.len() < required_bytes {
        return Err(CapabilityError::CapacityExceeded);
    }

    if channel_planes.len() == 1 && channel_planes[0].1 == total_channels {
        let plane = channel_planes[0].0;
        if plane.len() < total_samples {
            return Err(CapabilityError::InvalidTarget);
        }
        for (i, &sample) in plane[..total_samples].iter().enumerate() {
            let clean = if sample.is_finite() { sample } else { 0.0 };
            dst_interleaved_bytes[i * 4..(i + 1) * 4].copy_from_slice(&clean.to_le_bytes());
        }
        return Ok(required_bytes);
    }

    let mut ch_sum = 0usize;
    for &(plane, chs) in channel_planes {
        if chs == 0 || plane.len() < frames.saturating_mul(chs) {
            return Err(CapabilityError::InvalidTarget);
        }
        ch_sum += chs;
    }
    if ch_sum != total_channels {
        return Err(CapabilityError::InvalidTarget);
    }

    for frame in 0..frames {
        let mut out_ch = 0usize;
        for &(plane, chs) in channel_planes {
            let base = frame * chs;
            for c in 0..chs {
                let sample = plane[base + c];
                let clean = if sample.is_finite() { sample } else { 0.0 };
                let byte_idx = (frame * total_channels + out_ch) * 4;
                dst_interleaved_bytes[byte_idx..byte_idx + 4].copy_from_slice(&clean.to_le_bytes());
                out_ch += 1;
            }
        }
    }
    Ok(required_bytes)
}

pub fn mac_input_clock_domain(device_uid: &str) -> u64 {
    endpoint_clock_domain(device_uid, 3)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_macos_packet(
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

/// Manager-owned Core Audio native graph topology controller.
pub struct PlatformTopologyController {
    targets: Vec<CaptureTarget>,
    shared_targets: Arc<Mutex<Vec<CaptureTarget>>>,
    topology: Arc<Mutex<NativeTreeMixTopology>>,
    dirty: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    active_objects_count: Arc<AtomicU64>,
    update_count: Arc<AtomicU64>,
}

impl PlatformTopologyController {
    pub fn active_objects_count(&self) -> u64 {
        self.active_objects_count.load(SeqCst)
    }

    pub fn update_count(&self) -> u64 {
        self.update_count.load(SeqCst)
    }

    pub(crate) fn is_finished(&self) -> bool {
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = guard.take().unwrap().join();
        }
        guard.is_none()
    }

    /// Update the tap's process objects without reopening the stream, preserving shared `AudioObjectID`s.
    pub(crate) fn update_targets_with_stream(
        &mut self,
        new_targets: Vec<CaptureTarget>,
        stream: &CoreAudioTapStream,
    ) -> Result<Vec<Arc<str>>, CapabilityError> {
        if new_targets.is_empty() {
            return Err(CapabilityError::InvalidTarget);
        }
        let mut old_used = vec![false; self.targets.len()];
        let mut mapping = Vec::with_capacity(new_targets.len());
        for target in &new_targets {
            let mut matched = None;
            for (old_idx, old_target) in self.targets.iter().enumerate() {
                if !old_used[old_idx]
                    && old_target.id() == target.id()
                    && old_target.scope() == target.scope()
                {
                    old_used[old_idx] = true;
                    matched = Some(old_idx);
                    break;
                }
            }
            mapping.push(matched);
        }

        let (unlinked, objs) = {
            let mut shared = self
                .shared_targets
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut topo = self.topology.lock().unwrap_or_else(|e| e.into_inner());
            let unlinked = topo.reconfigure_targets(&mapping);
            let objs = reconcile_target_audio_objects(&new_targets, &mut topo)?;
            *shared = new_targets.clone();
            self.active_objects_count
                .store(topo.active_link_count() as u64, SeqCst);
            (unlinked, objs)
        };

        #[cfg(target_os = "macos")]
        if stream.update_tap_process_objects(&objs).is_ok() {
            self.update_count.fetch_add(1, SeqCst);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = &objs;
            self.update_count.fetch_add(1, SeqCst);
        }

        let next_root = if new_targets.len() == 1 {
            new_targets[0].process_identity()
        } else {
            None
        };
        stream.set_root_identity(next_root);

        self.dirty.store(true, SeqCst);
        if let Some(ref handle) = *self.worker.lock().unwrap_or_else(|e| e.into_inner()) {
            handle.thread().unpark();
        }
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
        let removed = self
            .topology
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retire_target(target_index);
        self.active_objects_count.store(
            self.topology
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active_link_count() as u64,
            SeqCst,
        );
        self.dirty.store(true, SeqCst);
        if let Some(ref handle) = *self.worker.lock().unwrap_or_else(|e| e.into_inner()) {
            handle.thread().unpark();
        }
        removed
    }

    fn shutdown(&self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

impl Drop for PlatformTopologyController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Resolve live Core Audio process objects for the targets and register them in the topology.
pub fn reconcile_target_audio_objects(
    targets: &[CaptureTarget],
    topology: &mut NativeTreeMixTopology,
) -> Result<Vec<u32>, CapabilityError> {
    let (proc_table, live_objs) = collect_live_coreaudio_process_objects()?;
    let mut unique_objs = BTreeSet::new();
    for (target_idx, target) in targets.iter().enumerate() {
        if !matches!(target.kind(), TargetKind::Process | TargetKind::Application) {
            continue;
        }
        let root_id = target
            .process_identity()
            .ok_or(CapabilityError::InvalidTarget)?;
        let exact_only = target.scope() == ProcessScope::Exact;
        let _ = pid_belongs_to_target(&proc_table, root_id, true, root_id.pid)?;

        for obj in &live_objs {
            if pid_belongs_to_target(&proc_table, root_id, exact_only, obj.pid)? {
                let key = format!("ca-obj-{}", obj.object_id);
                let _ = topology.attach_source(target_idx, key);
                unique_objs.insert(obj.object_id);
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(direct_obj) = crate::native_bridge::translate_pid_to_process_object(root_id.pid)
            && proc_table.contains_key(&root_id.pid)
        {
            let key = format!("ca-obj-{direct_obj}");
            let _ = topology.attach_source(target_idx, key);
            unique_objs.insert(direct_obj);
        }
    }
    Ok(unique_objs.into_iter().collect())
}

pub struct CoreAudioTapStream {
    format: Arc<Format>,
    trace: Option<Arc<Trace>>,
    reads: ReadStream,
    control: ControlHandle,
    default_cancel: Cancellation,
    armed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    ready: Arc<ReadySignal>,
    ended: Arc<AtomicU32>,
    root_identity: Arc<Mutex<Option<ProcessIdentity>>>,
    #[cfg(target_os = "macos")]
    tap_handle: Arc<Mutex<Option<crate::native_bridge::NativeCoreAudioTapHandle>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    _reservation: StorageReservation,
}

impl CoreAudioTapStream {
    pub(crate) fn arm(&self) {
        self.armed.store(true, SeqCst);
    }

    pub(crate) fn set_root_identity(&self, root: Option<ProcessIdentity>) {
        *self.root_identity.lock().unwrap_or_else(|e| e.into_inner()) = root;
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn update_tap_process_objects(
        &self,
        objects: &[u32],
    ) -> Result<(), CapabilityError> {
        let guard = self.tap_handle.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref h) = *guard {
            h.update_processes(objects)
        } else {
            Err(CapabilityError::ServerUnavailable)
        }
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

    #[cfg(any(test, feature = "native-tests"))]
    pub fn from_synthetic_packets(
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
                publish_macos_packet(
                    &mut producer,
                    &bytes,
                    false,
                    false,
                    frames,
                    (ts_ns > 0).then_some(ts_ns),
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
            root_identity: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            tap_handle: Arc::new(Mutex::new(None)),
            worker: Mutex::new(Some(worker)),
            _reservation: retained_reservation,
        })
    }
}

impl AudioStreamBackend for CoreAudioTapStream {
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

impl Drop for CoreAudioTapStream {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
    }
}

/// Core Audio HAL input stream using `AudioDeviceCreateIOProcID` on the input scope.
pub struct CoreAudioInputStream {
    format: Arc<Format>,
    device_uid: Arc<str>,
    clock_domain: u64,
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

impl CoreAudioInputStream {
    pub(crate) fn arm(&self) {
        self.armed.store(true, SeqCst);
    }

    pub fn device_uid(&self) -> &str {
        &self.device_uid
    }

    pub fn clock_domain(&self) -> u64 {
        self.clock_domain
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

impl AudioStreamBackend for CoreAudioInputStream {
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

impl Drop for CoreAudioInputStream {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    use crate::native_bridge::{NativeCoreAudioInputHandle, NativeCoreAudioTapHandle};
    use crate::proc_info::inspect_bsd_process;
    use std::ffi::{c_float, c_void};

    pub struct CallbackCtx {
        pub armed: Arc<AtomicBool>,
        pub clock_domain: u64,
        pub producer: Mutex<Producer>,
        pub policy: Overflow,
        pub ready: Arc<ReadySignal>,
        pub sequence: AtomicU64,
        pub segment: AtomicU64,
        pub frame_cursor: AtomicU64,
        pub scratch: Mutex<Vec<u8>>,
    }

    pub unsafe extern "C" fn on_coreaudio_packet(
        user_data: *mut c_void,
        num_buffers: u32,
        buffer_ptrs: *const *const c_float,
        buffer_channels: *const u32,
        frames: u32,
        timestamp_ns: u64,
        timestamp_valid: u32,
    ) {
        // SAFETY: `user_data` points to `CallbackCtx` pinned for the tap's lifetime;
        // `scratch` is preallocated (`MAX_FRAMES_PER_SLOT * stride` bytes), so 0 heap allocations occur.
        unsafe {
            if user_data.is_null()
                || buffer_ptrs.is_null()
                || buffer_channels.is_null()
                || frames == 0
            {
                return;
            }
            let ctx = &*(user_data as *const CallbackCtx);
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
                        ctx.clock_domain,
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

    struct InputCallbackSlot {
        producer: Producer,
        scratch: Vec<u8>,
        clock_domain: u64,
    }

    struct InputCallbackCtx {
        armed: Arc<AtomicBool>,
        slot: Mutex<Option<InputCallbackSlot>>,
        policy: Overflow,
        ready: Arc<ReadySignal>,
        sequence: AtomicU64,
        segment: AtomicU64,
        frame_cursor: AtomicU64,
    }

    unsafe extern "C" fn on_coreaudio_input_packet(
        user_data: *mut c_void,
        num_buffers: u32,
        buffer_ptrs: *const *const c_float,
        buffer_channels: *const u32,
        frames: u32,
        timestamp_ns: u64,
        timestamp_valid: u32,
    ) {
        // SAFETY: `user_data` points to `InputCallbackCtx` pinned across the HAL IOProc lifetime;
        // while `armed == false`, returns immediately without locking; once armed, `slot` is preallocated (`0` heap allocs).
        unsafe {
            if user_data.is_null()
                || buffer_ptrs.is_null()
                || buffer_channels.is_null()
                || frames == 0
            {
                return;
            }
            let ctx = &*(user_data as *const InputCallbackCtx);
            if !ctx.armed.load(SeqCst) {
                return;
            }
            let mut guard = ctx.slot.lock().unwrap_or_else(|e| e.into_inner());
            let Some(ref mut inner) = *guard else {
                return;
            };
            let acquired = inner.producer.trace().map(|_| Instant::now());
            let total_chans = inner.producer.pool().format().channels() as usize;
            let bpf = inner.producer.pool().format().bytes_per_frame();
            let ts = (timestamp_valid != 0 && timestamp_ns > 0).then_some(timestamp_ns);
            let clock_domain = inner.clock_domain;

