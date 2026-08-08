use crate::capture_worker::MemoryTake;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst},
    mpsc::{self, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct PlaybackStatus {
    pub active: bool,
    pub paused: bool,
    pub take_id: Option<usize>,
    pub current_frame: usize,
    pub total_frames: usize,
    pub sample_rate: u32,
}

impl PlaybackStatus {
    pub fn progress_ratio(&self) -> f32 {
        if self.total_frames == 0 {
            0.0
        } else {
            (self.current_frame as f32 / self.total_frames as f32).clamp(0.0, 1.0)
        }
    }

    pub fn position_secs(&self) -> f32 {
        let rate = self.sample_rate.max(1) as f32;
        (self.current_frame as f32) / rate
    }

    pub fn duration_secs(&self) -> f32 {
        let rate = self.sample_rate.max(1) as f32;
        (self.total_frames as f32) / rate
    }
}

enum PlaybackCmd {
    Play(Arc<MemoryTake>),
    TogglePause,
    SeekRatio(f32),
    Stop,
    Shutdown,
}

struct SharedPlaybackState {
    active: AtomicBool,
    paused: AtomicBool,
    take_id: AtomicUsize,
    current_frame_bits: AtomicU64,
    total_frames: AtomicUsize,
    sample_rate: AtomicUsize,
    vis_samples: Mutex<Vec<f32>>,
    last_error: Mutex<Option<String>>,
}

impl Default for SharedPlaybackState {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            take_id: AtomicUsize::new(0),
            current_frame_bits: AtomicU64::new(0.0f64.to_bits()),
            total_frames: AtomicUsize::new(0),
            sample_rate: AtomicUsize::new(48_000),
            vis_samples: Mutex::new(Vec::with_capacity(4096)),
            last_error: Mutex::new(None),
        }
    }
}

pub struct PlaybackEngine {
    cmd_tx: Sender<PlaybackCmd>,
    shared: Arc<SharedPlaybackState>,
    join: Option<JoinHandle<()>>,
}

impl Default for PlaybackEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackEngine {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<PlaybackCmd>();
        let shared = Arc::new(SharedPlaybackState::default());
        let shared_worker = shared.clone();
        let join = thread::spawn(move || playback_worker_loop(cmd_rx, shared_worker));
        Self {
            cmd_tx,
            shared,
            join: Some(join),
        }
    }

    pub fn play(&self, take: Arc<MemoryTake>) {
        let _ = self.cmd_tx.send(PlaybackCmd::Play(take));
    }

    pub fn toggle_pause(&self) {
        let _ = self.cmd_tx.send(PlaybackCmd::TogglePause);
    }

    pub fn seek_ratio(&self, ratio: f32) {
        let _ = self
            .cmd_tx
            .send(PlaybackCmd::SeekRatio(ratio.clamp(0.0, 1.0)));
    }

    pub fn stop(&self) {
        let _ = self.cmd_tx.send(PlaybackCmd::Stop);
    }

    pub fn status(&self) -> PlaybackStatus {
        let active = self.shared.active.load(SeqCst);
        let paused = self.shared.paused.load(SeqCst);
        let take_id_raw = self.shared.take_id.load(SeqCst);
        let pos_f64 = f64::from_bits(self.shared.current_frame_bits.load(SeqCst));
        let total = self.shared.total_frames.load(SeqCst);
        let rate = self.shared.sample_rate.load(SeqCst) as u32;
        PlaybackStatus {
            active,
            paused,
            take_id: (take_id_raw > 0).then_some(take_id_raw),
            current_frame: (pos_f64 as usize).min(total),
            total_frames: total,
            sample_rate: rate.max(1),
        }
    }

    pub fn drain_visual_samples(&self, out: &mut Vec<f32>) {
        if let Ok(mut guard) = self.shared.vis_samples.lock()
            && !guard.is_empty()
        {
            out.extend_from_slice(&guard);
            guard.clear();
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared
            .last_error
            .lock()
            .ok()
            .and_then(|mut g| g.take())
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(PlaybackCmd::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn playback_worker_loop(cmd_rx: mpsc::Receiver<PlaybackCmd>, shared: Arc<SharedPlaybackState>) {
    let mut active_stream: Option<cpal::Stream> = None;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            PlaybackCmd::Play(take) => {
                active_stream = None;
                if take.total_frames == 0 || take.preview_stereo_f32.len() < 2 {
                    shared.active.store(false, SeqCst);
                    if let Ok(mut err) = shared.last_error.lock() {
                        *err = Some("Selected take is empty (0 frames)".to_owned());
                    }
                    continue;
                }
                shared.take_id.store(take.id, SeqCst);
                shared.total_frames.store(take.total_frames, SeqCst);
                shared
                    .sample_rate
                    .store(take.format.rate() as usize, SeqCst);
                shared.current_frame_bits.store(0.0f64.to_bits(), SeqCst);
                shared.paused.store(false, SeqCst);
                shared.active.store(true, SeqCst);

                match build_cpal_output_stream(take, shared.clone()) {
                    Ok(stream) => {
                        if let Err(err) = stream.play() {
                            shared.active.store(false, SeqCst);
                            if let Ok(mut e) = shared.last_error.lock() {
                                *e = Some(format!("Audio playback start failed: {err}"));
                            }
                        } else {
                            active_stream = Some(stream);
                        }
                    }
                    Err(err) => {
                        shared.active.store(false, SeqCst);
                        if let Ok(mut e) = shared.last_error.lock() {
                            *e = Some(err);
                        }
                    }
                }
            }
            PlaybackCmd::TogglePause => {
                if shared.active.load(SeqCst) {
                    let prev = shared.paused.load(SeqCst);
                    shared.paused.store(!prev, SeqCst);
                }
            }
            PlaybackCmd::SeekRatio(ratio) => {
                let total = shared.total_frames.load(SeqCst);
                if total > 0 {
                    let target = ((total as f64) * f64::from(ratio)).clamp(0.0, (total - 1) as f64);
                    shared.current_frame_bits.store(target.to_bits(), SeqCst);
                    if !shared.active.load(SeqCst) && active_stream.is_some() {
                        shared.active.store(true, SeqCst);
                    }
                }
            }
            PlaybackCmd::Stop => {
                active_stream = None;
                shared.active.store(false, SeqCst);
                shared.paused.store(false, SeqCst);
                shared.current_frame_bits.store(0.0f64.to_bits(), SeqCst);
            }
            PlaybackCmd::Shutdown => {
                drop(active_stream.take());
                shared.active.store(false, SeqCst);
                break;
            }
        }
    }
}

fn build_cpal_output_stream(
    take: Arc<MemoryTake>,
    shared: Arc<SharedPlaybackState>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "No default audio output device found".to_owned())?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("Failed to query output device config: {e}"))?;

    let config: cpal::StreamConfig = supported.into();
    let out_channels = usize::from(config.channels.max(1));
    let out_rate = f64::from(config.sample_rate.max(1));
    let src_rate = f64::from(take.format.rate().max(1));
    let step = src_rate / out_rate;

    let err_shared = shared.clone();
    let err_fn = move |err| {
        if let Ok(mut guard) = err_shared.last_error.lock() {
            *guard = Some(format!("Output stream error: {err}"));
        }
    };

    match supported.sample_format() {
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill_output_f32(data, out_channels, step, &take, &shared);
                },
                err_fn,
                Some(Duration::from_secs(2)),
            )
            .map_err(|e| format!("Failed to build f32 output stream: {e}")),
        cpal::SampleFormat::I16 => device
            .build_output_stream(
                config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let mut temp = vec![0.0f32; data.len()];
                    fill_output_f32(&mut temp, out_channels, step, &take, &shared);
                    for (dst, &s) in data.iter_mut().zip(temp.iter()) {
                        *dst = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                err_fn,
                Some(Duration::from_secs(2)),
            )
            .map_err(|e| format!("Failed to build i16 output stream: {e}")),
        cpal::SampleFormat::U16 => device
            .build_output_stream(
                config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    let mut temp = vec![0.0f32; data.len()];
                    fill_output_f32(&mut temp, out_channels, step, &take, &shared);
                    for (dst, &s) in data.iter_mut().zip(temp.iter()) {
                        *dst = ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0) as u16;
                    }
                },
                err_fn,
                Some(Duration::from_secs(2)),
            )
            .map_err(|e| format!("Failed to build u16 output stream: {e}")),
        other => Err(format!(
            "Unsupported output device sample format: {other:?}"
        )),
    }
}

fn fill_output_f32(
    out: &mut [f32],
    out_channels: usize,
    step: f64,
    take: &MemoryTake,
    shared: &SharedPlaybackState,
) {
    if !shared.active.load(SeqCst) || shared.paused.load(SeqCst) {
        out.fill(0.0);
        return;
    }

    let total_frames = take.preview_stereo_f32.len() / 2;
    if total_frames == 0 {
        out.fill(0.0);
        shared.active.store(false, SeqCst);
        return;
    }

    let mut pos = f64::from_bits(shared.current_frame_bits.load(SeqCst));
    let mut vis_batch = Vec::with_capacity((out.len() / out_channels.max(1)) * 2);

    for frame_out in out.chunks_exact_mut(out_channels.max(1)) {
        let idx0 = pos.floor() as usize;
        if idx0 >= total_frames {
            frame_out.fill(0.0);
            shared.active.store(false, SeqCst);
            continue;
        }
        let idx1 = (idx0 + 1).min(total_frames - 1);
        let frac = (pos - (idx0 as f64)) as f32;

        let l0 = take.preview_stereo_f32[idx0 * 2];
        let r0 = take.preview_stereo_f32[idx0 * 2 + 1];
        let l1 = take.preview_stereo_f32[idx1 * 2];
        let r1 = take.preview_stereo_f32[idx1 * 2 + 1];

        let left = l0 + (l1 - l0) * frac;
        let right = r0 + (r1 - r0) * frac;

        frame_out[0] = left;
        if out_channels >= 2 {
            frame_out[1] = right;
            for extra in &mut frame_out[2..] {
                *extra = 0.0;
            }
        }
        vis_batch.push(left);
        vis_batch.push(right);
        pos += step;
    }

    shared.current_frame_bits.store(pos.to_bits(), SeqCst);
    if (pos.floor() as usize) >= total_frames {
        shared.active.store(false, SeqCst);
    }

    if !vis_batch.is_empty()
        && let Ok(mut guard) = shared.vis_samples.try_lock()
    {
        if guard.len() > 8192 {
            guard.clear();
        }
        guard.extend_from_slice(&vis_batch);
    }
}
