use crate::backend::{InputTelemetryRow, PlatformCaptureSession, default_stereo_f32_format};
use crate::dsp::amplitude_to_db;
use crate::wav::decode_pcm_to_stereo_f32;
use capture_system_or_process_audio::{
    CaptureRequest, DitherMode, Format, MixedBlockMetadata, OutputConverterConfig, Overflow,
    SampleType, Stop,
};
use std::path::PathBuf;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct SegmentMarker {
    pub segment_index: usize,
    pub frame_offset: usize,
    pub targets_summary: String,
}

#[derive(Clone, Debug)]
pub struct MemoryTake {
    pub id: usize,
    pub title: String,
    pub targets_summary: String,
    pub format: Format,
    pub raw_pcm_bytes: Vec<u8>,
    pub preview_stereo_f32: Vec<f32>,
    pub total_frames: usize,
    pub segments: Vec<SegmentMarker>,
    pub peak_dbfs: [f32; 2],
    pub max_skew_ns: Option<u64>,
    pub clipped_samples: u64,
    pub drift_slips: u64,
    pub saved_path: Option<PathBuf>,
}

impl MemoryTake {
    pub fn duration_secs(&self) -> f32 {
        let rate = self.format.rate().max(1) as f32;
        (self.total_frames as f32) / rate
    }

    pub fn memory_bytes(&self) -> usize {
        self.raw_pcm_bytes.len() + self.preview_stereo_f32.len() * 4
    }
}

pub enum WorkerCommand {
    SyncMonitor {
        enabled: bool,
        targets_summary: String,
        request: Option<CaptureRequest>,
        overflow: Overflow,
        synthetic: bool,
    },
    StartRecording {
        take_id: usize,
        title: String,
        targets_summary: String,
        request: Option<CaptureRequest>,
        overflow: Overflow,
        synthetic: bool,
    },
    HotSwap {
        targets_summary: String,
        request: Option<CaptureRequest>,
        overflow: Overflow,
        synthetic: bool,
    },
    StopRecording,
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct LiveCaptureTelemetry {
    pub recording_active: bool,
    pub take_id: usize,
    pub total_frames: usize,
    pub total_bytes: usize,
    pub segment_count: usize,
    pub active_targets_summary: String,
    pub format: Format,
    pub metadata: MixedBlockMetadata,
    pub input_rows: Vec<InputTelemetryRow>,
    pub stereo_samples: Vec<f32>,
}

pub enum WorkerEvent {
    Telemetry(LiveCaptureTelemetry),
    SessionOpened,
    HotSwapped {
        segment_index: usize,
        frame_offset: usize,
        targets_summary: String,
    },
    TakeCompleted(Arc<MemoryTake>),
    StatusMessage(String),
    Error(String),
}

pub struct CaptureWorkerHandle {
    cmd_tx: Sender<WorkerCommand>,
    evt_rx: Receiver<WorkerEvent>,
    join: Option<JoinHandle<()>>,
}

impl Default for CaptureWorkerHandle {
    fn default() -> Self {
        Self::spawn()
    }
}

impl CaptureWorkerHandle {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
        let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();
        let join = thread::spawn(move || worker_loop(cmd_rx, evt_tx));
        Self {
            cmd_tx,
            evt_rx,
            join: Some(join),
        }
    }

    pub fn send(&self, cmd: WorkerCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn try_recv(&self) -> Option<WorkerEvent> {
        self.evt_rx.try_recv().ok()
    }
}

impl Drop for CaptureWorkerHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

struct ActiveTakeBuffer {
    take_id: usize,
    title: String,
    targets_summary: String,
    format: Format,
    raw_pcm_bytes: Vec<u8>,
    preview_stereo_f32: Vec<f32>,
    total_frames: usize,
    segments: Vec<SegmentMarker>,
    peak_abs: [f32; 2],
    max_skew_ns: Option<u64>,
    clipped_samples: u64,
    drift_slips: u64,
}

impl ActiveTakeBuffer {
    fn new(take_id: usize, title: String, targets_summary: String, format: Format) -> Self {
        let segments = vec![SegmentMarker {
            segment_index: 1,
            frame_offset: 0,
            targets_summary: targets_summary.clone(),
        }];
        Self {
            take_id,
            title,
            targets_summary,
            format,
            raw_pcm_bytes: Vec::with_capacity(48_000 * 8 * 5),
            preview_stereo_f32: Vec::with_capacity(48_000 * 2 * 5),
            total_frames: 0,
            segments,
            peak_abs: [0.0, 0.0],
            max_skew_ns: None,
            clipped_samples: 0,
            drift_slips: 0,
        }
    }

    fn record_hot_swap(&mut self, targets_summary: String) -> SegmentMarker {
        self.targets_summary = targets_summary.clone();
        let next_idx = self.segments.len() + 1;
        let marker = SegmentMarker {
            segment_index: next_idx,
            frame_offset: self.total_frames,
            targets_summary,
        };
        self.segments.push(marker.clone());
        marker
    }

    fn append_chunk(
        &mut self,
        pcm_bytes: &[u8],
        frames: usize,
        meta: &MixedBlockMetadata,
    ) -> Vec<f32> {
        self.raw_pcm_bytes.extend_from_slice(pcm_bytes);
        self.total_frames = self.total_frames.saturating_add(frames);
        let prev_len = self.preview_stereo_f32.len();
        decode_pcm_to_stereo_f32(pcm_bytes, &self.format, &mut self.preview_stereo_f32);
        let new_stereo = self.preview_stereo_f32[prev_len..].to_vec();
        for pair in new_stereo.as_chunks::<2>().0 {
            self.peak_abs[0] = self.peak_abs[0].max(pair[0].abs());
            self.peak_abs[1] = self.peak_abs[1].max(pair[1].abs());
        }
        if let Some(skew) = meta.skew_estimate_ns {
            self.max_skew_ns = Some(self.max_skew_ns.map_or(skew, |prev| prev.max(skew)));
        }
        self.clipped_samples = self.clipped_samples.max(meta.clipped_samples);
        self.drift_slips = self.drift_slips.max(meta.drift_slips);
        new_stereo
    }

    fn finalize(self) -> MemoryTake {
        MemoryTake {
            id: self.take_id,
            title: self.title,
            targets_summary: self.targets_summary,
            format: self.format,
            raw_pcm_bytes: self.raw_pcm_bytes,
            preview_stereo_f32: self.preview_stereo_f32,
            total_frames: self.total_frames,
            segments: self.segments,
            peak_dbfs: [
                amplitude_to_db(self.peak_abs[0]),
                amplitude_to_db(self.peak_abs[1]),
            ],
            max_skew_ns: self.max_skew_ns,
            clipped_samples: self.clipped_samples,
            drift_slips: self.drift_slips,
            saved_path: None,
        }
    }
}

fn open_session_if_requested(
    request: Option<CaptureRequest>,
    overflow: Overflow,
    synthetic: bool,
) -> Result<(Option<PlatformCaptureSession>, Format), String> {
    let fallback_fmt = default_stereo_f32_format();

    let Some(req) = request else {
        return Ok((None, fallback_fmt));
    };
    let session = if synthetic {
        PlatformCaptureSession::open_synthetic(&req)?
    } else {
        PlatformCaptureSession::open_native(req, overflow)?
    };
    let fmt = session.format().clone();
    Ok((Some(session), fmt))
}

fn worker_loop(cmd_rx: Receiver<WorkerCommand>, evt_tx: Sender<WorkerEvent>) {
    let mut active_session: Option<PlatformCaptureSession> = None;
    let mut active_take: Option<ActiveTakeBuffer> = None;
    let mut monitor_enabled = false;
    let mut monitor_summary = String::new();
    let mut last_request: Option<CaptureRequest> = None;
    let mut last_overflow = Overflow::DropOldest;
    let mut last_synthetic = false;
    let mut monitor_frames = 0usize;
    let mut monitor_bytes = 0usize;

    loop {
        loop {
            match cmd_rx.try_recv() {
                Ok(WorkerCommand::SyncMonitor {
                    enabled,
                    targets_summary,
                    request,
                    overflow,
                    synthetic,
                }) => {
                    monitor_enabled = enabled;
                    monitor_summary = targets_summary;
                    last_request = request.clone();
                    last_overflow = overflow;
                    last_synthetic = synthetic;

                    if active_take.is_none() {
                        if let Some(mut old_sess) = active_session.take() {
                            old_sess.stop_and_cleanup(Stop::Drain);
                        }
                        monitor_frames = 0;
                        monitor_bytes = 0;
                        if monitor_enabled {
                            match open_session_if_requested(request, overflow, synthetic) {
                                Ok((sess, _)) => {
                                    if sess.is_some() && !synthetic {
                                        let _ = evt_tx.send(WorkerEvent::SessionOpened);
                                    }
                                    active_session = sess;
                                }
                                Err(err) => {
                                    let _ = evt_tx.send(WorkerEvent::Error(format!(
                                        "Live monitor open failed: {err}"
                                    )));
                                }
                            }
                        }
                    }
                }
                Ok(WorkerCommand::StartRecording {
                    take_id,
                    title,
                    targets_summary,
                    request,
                    overflow,
                    synthetic,
                }) => {
                    if let Some(old_take) = active_take.take()
                        && old_take.total_frames > 0
                    {
                        let _ =
                            evt_tx.send(WorkerEvent::TakeCompleted(Arc::new(old_take.finalize())));
                    }

                    let can_reuse_monitor = active_session.is_some()
                        && last_request == request
                        && last_overflow == overflow
                        && last_synthetic == synthetic;

                    monitor_summary = targets_summary.clone();
                    last_request = request.clone();
                    last_overflow = overflow;
                    last_synthetic = synthetic;

                    if can_reuse_monitor && let Some(ref sess) = active_session {
                        let fmt = sess.format().clone();
                        active_take = Some(ActiveTakeBuffer::new(
                            take_id,
                            title,
                            targets_summary.clone(),
                            fmt,
                        ));
                        let _ = evt_tx.send(WorkerEvent::StatusMessage(format!(
                            "Recording started ({targets_summary})"
                        )));
                    } else {
                        if let Some(mut old_sess) = active_session.take() {
                            old_sess.stop_and_cleanup(Stop::Drain);
                        }
                        match open_session_if_requested(request, overflow, synthetic) {
                            Ok((sess, fmt)) => {
                                if sess.is_some() && !synthetic {
                                    let _ = evt_tx.send(WorkerEvent::SessionOpened);
                                }
                                active_session = sess;
                                active_take = Some(ActiveTakeBuffer::new(
                                    take_id,
                                    title,
                                    targets_summary.clone(),
                                    fmt,
                                ));
                                let _ = evt_tx.send(WorkerEvent::StatusMessage(format!(
                                    "Recording started ({targets_summary})"
                                )));
                            }
                            Err(err) => {
                                let _ = evt_tx.send(WorkerEvent::Error(format!(
                                    "Failed to open capture session: {err}"
                                )));
                            }
                        }
                    }
                }
                Ok(WorkerCommand::HotSwap {
                    targets_summary,
                    request,
                    overflow,
                    synthetic,
                }) => {
                    monitor_summary = targets_summary.clone();
                    last_request = request.clone();
                    last_overflow = overflow;
                    last_synthetic = synthetic;

                    if let Some(take_buf) = active_take.as_mut() {
                        if let Some(mut req) = request {
                            // Keep the take format stable without disqualifying native graph mixing
                            // when the take already uses 48 kHz stereo F32.
                            let req_effective_matches_take = match req.output_converter() {
                                Some(conv) => conv.target_format == take_buf.format,
                                None => {
                                    take_buf.format.sample() == SampleType::F32
                                        && take_buf.format.rate() == 48_000
                                        && take_buf.format.channels() == 2
                                }
                            };
                            if !req_effective_matches_take {
                                req = req.with_output_converter(Some(OutputConverterConfig::new(
                                    take_buf.format.clone(),
                                    DitherMode::Tpdf,
                                )));
                            }

                            let swap_result = if let Some(sess) = active_session.as_mut() {
                                sess.hot_swap(req, overflow)
                            } else {
                                open_session_if_requested(Some(req), overflow, synthetic).map(
                                    |(new_sess, _)| {
                                        active_session = new_sess;
                                    },
                                )
                            };

                            match swap_result {
                                Ok(()) => {
                                    if !synthetic {
                                        let _ = evt_tx.send(WorkerEvent::SessionOpened);
                                    }
                                    let marker = take_buf.record_hot_swap(targets_summary.clone());
                                    let _ = evt_tx.send(WorkerEvent::HotSwapped {
                                        segment_index: marker.segment_index,
                                        frame_offset: marker.frame_offset,
                                        targets_summary,
                                    });
                                }
                                Err(err) => {
                                    let _ = evt_tx.send(WorkerEvent::Error(format!(
                                        "Hot-swap failed: {err}"
                                    )));
                                }
                            }
                        } else {
                            if let Some(mut old_sess) = active_session.take() {
                                if let Ok(tail) = old_sess.read_next(480, Duration::from_millis(10))
                                    && tail.frames > 0
                                {
                                    let _ = take_buf.append_chunk(
                                        &tail.pcm_bytes,
                                        tail.frames,
                                        &tail.metadata,
                                    );
                                }
                                old_sess.stop_and_cleanup(Stop::Drain);
                            }
                            let marker = take_buf.record_hot_swap(targets_summary.clone());
                            let _ = evt_tx.send(WorkerEvent::HotSwapped {
                                segment_index: marker.segment_index,
                                frame_offset: marker.frame_offset,
                                targets_summary,
                            });
                        }
                    } else if monitor_enabled {
                        if let Some(req) = request {
                            let res = if let Some(sess) = active_session.as_mut() {
                                sess.hot_swap(req, overflow)
                            } else {
                                open_session_if_requested(Some(req), overflow, synthetic).map(
                                    |(new_sess, _)| {
                                        active_session = new_sess;
                                    },
                                )
                            };
                            if let Err(err) = res {
                                let _ = evt_tx.send(WorkerEvent::Error(format!(
                                    "Live monitor update failed: {err}"
                                )));
                            } else if !synthetic {
                                let _ = evt_tx.send(WorkerEvent::SessionOpened);
                            }
                        } else if let Some(mut old_sess) = active_session.take() {
                            old_sess.stop_and_cleanup(Stop::Drain);
                        }
                    } else if let Some(mut old_sess) = active_session.take() {
                        old_sess.stop_and_cleanup(Stop::Drain);
                    }
                }
                Ok(WorkerCommand::StopRecording) => {
                    if let Some(sess) = active_session.as_mut()
                        && let Some(take_buf) = active_take.as_mut()
                        && let Ok(tail) = sess.read_next(480, Duration::from_millis(15))
                        && tail.frames > 0
                    {
                        let _ = take_buf.append_chunk(&tail.pcm_bytes, tail.frames, &tail.metadata);
                    }
                    if let Some(take_buf) = active_take.take() {
                        let completed = Arc::new(take_buf.finalize());
                        let _ = evt_tx.send(WorkerEvent::TakeCompleted(completed));
                    }
                    if !monitor_enabled && let Some(mut sess) = active_session.take() {
                        sess.stop_and_cleanup(Stop::Drain);
                    }
                    monitor_frames = 0;
                    monitor_bytes = 0;
                }
                Ok(WorkerCommand::Shutdown) => {
                    if let Some(mut sess) = active_session.take() {
                        sess.stop_and_cleanup(Stop::Abort);
                    }
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some(mut sess) = active_session.take() {
                        sess.stop_and_cleanup(Stop::Abort);
                    }
                    return;
                }
            }
        }

        if let Some(sess) = active_session.as_mut() {
            let tick_start = Instant::now();
            match sess.read_next(480, Duration::from_millis(25)) {
                Ok(outcome) => {
                    if let Some(take_buf) = active_take.as_mut() {
                        let stereo = if outcome.frames > 0 {
                            take_buf.append_chunk(
                                &outcome.pcm_bytes,
                                outcome.frames,
                                &outcome.metadata,
                            )
                        } else {
                            Vec::new()
                        };
                        let _ = evt_tx.send(WorkerEvent::Telemetry(LiveCaptureTelemetry {
                            recording_active: true,
                            take_id: take_buf.take_id,
                            total_frames: take_buf.total_frames,
                            total_bytes: take_buf.raw_pcm_bytes.len(),
                            segment_count: take_buf.segments.len(),
                            active_targets_summary: take_buf.targets_summary.clone(),
                            format: take_buf.format.clone(),
                            metadata: outcome.metadata,
                            input_rows: outcome.input_rows,
                            stereo_samples: stereo,
                        }));
                    } else {
                        let mut stereo = Vec::new();
                        if outcome.frames > 0 {
                            monitor_frames = monitor_frames.saturating_add(outcome.frames);
                            monitor_bytes = monitor_bytes.saturating_add(outcome.pcm_bytes.len());
                            decode_pcm_to_stereo_f32(
                                &outcome.pcm_bytes,
                                &outcome.format,
                                &mut stereo,
                            );
                        }
                        let _ = evt_tx.send(WorkerEvent::Telemetry(LiveCaptureTelemetry {
                            recording_active: false,
                            take_id: 0,
                            total_frames: monitor_frames,
                            total_bytes: monitor_bytes,
                            segment_count: 1,
                            active_targets_summary: monitor_summary.clone(),
                            format: outcome.format,
                            metadata: outcome.metadata,
                            input_rows: outcome.input_rows,
                            stereo_samples: stereo,
                        }));
                    }

                    if outcome.eof {
                        if let Some(mut finished_sess) = active_session.take() {
                            finished_sess.stop_and_cleanup(Stop::Drain);
                        }
                        let _ = evt_tx.send(WorkerEvent::StatusMessage(
                            "All active targets exited; session parked (toggle source or press Stop)".to_owned(),
                        ));
                    } else if outcome.frames == 0 {
                        let elapsed = tick_start.elapsed();
                        if elapsed < Duration::from_millis(10) {
                            thread::sleep(Duration::from_millis(10) - elapsed);
                        }
                    } else if matches!(sess, PlatformCaptureSession::Synthetic { .. }) {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                Err(err) => {
                    let _ = evt_tx.send(WorkerEvent::Error(format!("Capture read error: {err}")));
                    thread::sleep(Duration::from_millis(20));
                }
            }
        } else {
            thread::sleep(Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_system_or_process_audio::schema::{CaptureTarget, DefaultDeviceMode};

    #[test]
    fn worker_records_in_memory_take_and_preserves_hot_swap_segments() {
        let worker = CaptureWorkerHandle::spawn();
        let req1 = CaptureRequest::new(vec![CaptureTarget::default_output(
            DefaultDeviceMode::Follow,
        )]);
        worker.send(WorkerCommand::StartRecording {
            take_id: 1,
            title: "Test Take".to_owned(),
            targets_summary: "Default Output".to_owned(),
            request: Some(req1),
            overflow: Overflow::DropOldest,
            synthetic: true,
        });

        thread::sleep(Duration::from_millis(45));

        let req2 = CaptureRequest::new(vec![
            CaptureTarget::default_output(DefaultDeviceMode::Follow),
            CaptureTarget::default_output(DefaultDeviceMode::Follow),
        ]);
        worker.send(WorkerCommand::HotSwap {
            targets_summary: "Default Output x2".to_owned(),
            request: Some(req2),
            overflow: Overflow::DropOldest,
            synthetic: true,
        });

        thread::sleep(Duration::from_millis(45));
        worker.send(WorkerCommand::StopRecording);

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut completed_take = None;
        let mut saw_hot_swap = false;
        while Instant::now() < deadline {
            if let Some(evt) = worker.try_recv() {
                match evt {
                    WorkerEvent::HotSwapped { segment_index, .. } => {
                        assert_eq!(segment_index, 2);
                        saw_hot_swap = true;
                    }
                    WorkerEvent::TakeCompleted(take) => {
                        completed_take = Some(take);
                        break;
                    }
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }

        assert!(saw_hot_swap, "expected HotSwapped event");
        let take = completed_take.expect("expected completed MemoryTake");
        assert!(take.total_frames > 0);
        assert_eq!(take.segments.len(), 2);
        assert!(
            take.saved_path.is_none(),
            "takes must stay in RAM by default"
        );
    }
}
