use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::Arc;

pub const FFT_SIZE: usize = 2048;
pub const NUM_SPECTRUM_BANDS: usize = 64;
pub const WATERFALL_HISTORY: usize = 96;
pub const SCOPE_WINDOW_FRAMES: usize = 512;
pub const VOICE_PILL_HISTORY: usize = 240;
pub const DB_FLOOR: f32 = -72.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VisMode {
    #[default]
    FftSpectrum,
    Waterfall,
    Oscilloscope,
}

impl VisMode {
    pub fn next(self) -> Self {
        match self {
            Self::FftSpectrum => Self::Waterfall,
            Self::Waterfall => Self::Oscilloscope,
            Self::Oscilloscope => Self::FftSpectrum,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FftSpectrum => "FFT Spectrum + Peak Hold",
            Self::Waterfall => "Spectrogram Waterfall",
            Self::Oscilloscope => "Braille Oscilloscope",
        }
    }
}

pub struct SpectroscopeAnalyzer {
    sample_rate: u32,
    fft: Arc<dyn Fft<f32>>,
    hann_window: Vec<f32>,
    fft_scratch: Vec<Complex<f32>>,
    mono_ring: VecDeque<f32>,
    stereo_ring: VecDeque<(f32, f32)>,
    band_edges_hz: Vec<(f32, f32)>,
    pub smoothed_bands_db: [f32; NUM_SPECTRUM_BANDS],
    pub peak_hold_db: [f32; NUM_SPECTRUM_BANDS],
    peak_hold_age: [u16; NUM_SPECTRUM_BANDS],
    pub waterfall_rows: VecDeque<[f32; NUM_SPECTRUM_BANDS]>,
    pub scope_triggered: Vec<(f32, f32)>,
    pub voice_pill_history: VecDeque<f32>,
    pub peak_dbfs: [f32; 2],
    pub rms_dbfs: [f32; 2],
}

impl Default for SpectroscopeAnalyzer {
    fn default() -> Self {
        Self::new(48_000)
    }
}

impl SpectroscopeAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * (i as f32) / ((FFT_SIZE - 1) as f32)).cos()))
            .collect();
        let band_edges_hz = compute_log_bands(NUM_SPECTRUM_BANDS, 20.0, 20_000.0);

        let mut waterfall_rows = VecDeque::with_capacity(WATERFALL_HISTORY);
        for _ in 0..WATERFALL_HISTORY {
            waterfall_rows.push_back([DB_FLOOR; NUM_SPECTRUM_BANDS]);
        }
        let mut voice_pill_history = VecDeque::with_capacity(VOICE_PILL_HISTORY);
        for _ in 0..VOICE_PILL_HISTORY {
            voice_pill_history.push_back(0.0);
        }

        Self {
            sample_rate: sample_rate.max(8_000),
            fft,
            hann_window,
            fft_scratch: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            mono_ring: VecDeque::with_capacity(FFT_SIZE * 2),
            stereo_ring: VecDeque::with_capacity(FFT_SIZE * 2),
            band_edges_hz,
            smoothed_bands_db: [DB_FLOOR; NUM_SPECTRUM_BANDS],
            peak_hold_db: [DB_FLOOR; NUM_SPECTRUM_BANDS],
            peak_hold_age: [0; NUM_SPECTRUM_BANDS],
            waterfall_rows,
            scope_triggered: vec![(0.0, 0.0); SCOPE_WINDOW_FRAMES],
            voice_pill_history,
            peak_dbfs: [DB_FLOOR, DB_FLOOR],
            rms_dbfs: [DB_FLOOR, DB_FLOOR],
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        let clamped = sample_rate.max(8_000);
        if self.sample_rate != clamped {
            self.sample_rate = clamped;
        }
    }

    pub fn push_stereo_samples(&mut self, interleaved_stereo: &[f32]) {
        if interleaved_stereo.len() < 2 {
            return;
        }
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        let mut sum_sq_l = 0.0f64;
        let mut sum_sq_r = 0.0f64;
        let frames = interleaved_stereo.len() / 2;

        for pair in interleaved_stereo.chunks_exact(2) {
            let l = if pair[0].is_finite() { pair[0] } else { 0.0 };
            let r = if pair[1].is_finite() { pair[1] } else { 0.0 };
            peak_l = peak_l.max(l.abs());
            peak_r = peak_r.max(r.abs());
            sum_sq_l += f64::from(l) * f64::from(l);
            sum_sq_r += f64::from(r) * f64::from(r);

            if self.mono_ring.len() >= FFT_SIZE * 2 {
                self.mono_ring.pop_front();
            }
            self.mono_ring.push_back(0.5 * (l + r));

            if self.stereo_ring.len() >= FFT_SIZE * 2 {
                self.stereo_ring.pop_front();
            }
            self.stereo_ring.push_back((l, r));
        }

        let inst_peak_l = amplitude_to_db(peak_l);
        let inst_peak_r = amplitude_to_db(peak_r);
        let inst_rms_l = amplitude_to_db((sum_sq_l / frames as f64).sqrt() as f32);
        let inst_rms_r = amplitude_to_db((sum_sq_r / frames as f64).sqrt() as f32);

        self.peak_dbfs[0] = (self.peak_dbfs[0] * 0.72 + inst_peak_l * 0.28).max(inst_peak_l);
        self.peak_dbfs[1] = (self.peak_dbfs[1] * 0.72 + inst_peak_r * 0.28).max(inst_peak_r);
        self.rms_dbfs[0] = self.rms_dbfs[0] * 0.75 + inst_rms_l * 0.25;
        self.rms_dbfs[1] = self.rms_dbfs[1] * 0.75 + inst_rms_r * 0.25;

        let pill_db = inst_peak_l.max(inst_peak_r) * 0.65 + inst_rms_l.max(inst_rms_r) * 0.35;
        if self.voice_pill_history.len() >= VOICE_PILL_HISTORY {
            self.voice_pill_history.pop_front();
        }
        self.voice_pill_history.push_back(db_to_unit(pill_db));

        if self.mono_ring.len() >= FFT_SIZE {
            self.recompute_fft_and_scope();
        }
    }

    pub fn decay_idle(&mut self) {
        for i in 0..NUM_SPECTRUM_BANDS {
            self.smoothed_bands_db[i] = (self.smoothed_bands_db[i] - 1.8).max(DB_FLOOR);
            self.peak_hold_age[i] = self.peak_hold_age[i].saturating_add(1);
            if self.peak_hold_age[i] > 15 {
                self.peak_hold_db[i] = (self.peak_hold_db[i] - 0.9).max(self.smoothed_bands_db[i]);
            }
        }
        for ch in 0..2 {
            self.peak_dbfs[ch] = (self.peak_dbfs[ch] - 1.5).max(DB_FLOOR);
            self.rms_dbfs[ch] = (self.rms_dbfs[ch] - 1.5).max(DB_FLOOR);
        }
    }

    fn recompute_fft_and_scope(&mut self) {
        let start = self.mono_ring.len() - FFT_SIZE;
        for (idx, (&sample, &win)) in self
            .mono_ring
            .iter()
            .skip(start)
            .zip(self.hann_window.iter())
            .enumerate()
        {
            self.fft_scratch[idx] = Complex::new(sample * win, 0.0);
        }
        self.fft.process(&mut self.fft_scratch);

        let nyquist_bins = FFT_SIZE / 2;
        let bin_hz = (self.sample_rate as f32) / (FFT_SIZE as f32);
        let norm = 2.0 / (FFT_SIZE as f32 * 0.5);

        let mut current_row = [DB_FLOOR; NUM_SPECTRUM_BANDS];
        for (band_idx, &(f_lo, f_hi)) in self.band_edges_hz.iter().enumerate() {
            let b0 = ((f_lo / bin_hz).floor() as usize).clamp(1, nyquist_bins - 1);
            let b1 = ((f_hi / bin_hz).ceil() as usize).clamp(b0, nyquist_bins - 1);
            let mut max_mag = 0.0f32;
            for bin in b0..=b1 {
                let c = self.fft_scratch[bin];
                let mag = (c.re * c.re + c.im * c.im).sqrt() * norm;
                max_mag = max_mag.max(mag);
            }
            let db = amplitude_to_db(max_mag);
            current_row[band_idx] = db;

            if db >= self.smoothed_bands_db[band_idx] {
                if self.smoothed_bands_db[band_idx] <= DB_FLOOR + 1.0 {
                    self.smoothed_bands_db[band_idx] = db;
                } else {
                    self.smoothed_bands_db[band_idx] =
                        self.smoothed_bands_db[band_idx] * 0.20 + db * 0.80;
                }
            } else {
                self.smoothed_bands_db[band_idx] = (self.smoothed_bands_db[band_idx] - 1.6).max(db);
            }

            if self.smoothed_bands_db[band_idx] >= self.peak_hold_db[band_idx] {
                self.peak_hold_db[band_idx] = self.smoothed_bands_db[band_idx];
                self.peak_hold_age[band_idx] = 0;
            } else {
                self.peak_hold_age[band_idx] = self.peak_hold_age[band_idx].saturating_add(1);
                if self.peak_hold_age[band_idx] > 20 {
                    self.peak_hold_db[band_idx] =
                        (self.peak_hold_db[band_idx] - 0.65).max(self.smoothed_bands_db[band_idx]);
                }
            }
        }

        if self.waterfall_rows.len() >= WATERFALL_HISTORY {
            self.waterfall_rows.pop_back();
        }
        self.waterfall_rows.push_front(current_row);

        if self.stereo_ring.len() >= SCOPE_WINDOW_FRAMES * 2 {
            let search_start = self.stereo_ring.len() - SCOPE_WINDOW_FRAMES * 2;
            let mut trigger_offset = self.stereo_ring.len() - SCOPE_WINDOW_FRAMES;
            for idx in search_start..(self.stereo_ring.len() - SCOPE_WINDOW_FRAMES) {
                let prev_l = self.stereo_ring[idx].0;
                let curr_l = self.stereo_ring[idx + 1].0;
                if prev_l <= 0.0 && curr_l > 0.0 {
                    trigger_offset = idx;
                    break;
                }
            }
            self.scope_triggered.clear();
            self.scope_triggered.extend(
                self.stereo_ring
                    .iter()
                    .skip(trigger_offset)
                    .take(SCOPE_WINDOW_FRAMES)
                    .copied(),
            );
        }
    }

    pub fn band_center_hz(&self, band_idx: usize) -> f32 {
        let (lo, hi) = self.band_edges_hz[band_idx.min(NUM_SPECTRUM_BANDS - 1)];
        (lo * hi).sqrt()
    }
}

pub fn summarize_take_voice_bars(preview_stereo_f32: &[f32], num_bars: usize) -> Vec<f32> {
    if num_bars == 0 {
        return Vec::new();
    }
    let total_frames = preview_stereo_f32.len() / 2;
    if total_frames == 0 {
        return vec![0.0; num_bars];
    }
    let mut bars = Vec::with_capacity(num_bars);
    for idx in 0..num_bars {
        let f0 = idx * total_frames / num_bars;
        let f1 = ((idx + 1) * total_frames / num_bars)
            .max(f0 + 1)
            .min(total_frames);
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let count = (f1 - f0).max(1);
        for frame in f0..f1 {
            let l = preview_stereo_f32[frame * 2].abs();
            let r = preview_stereo_f32[frame * 2 + 1].abs();
            let m = l.max(r);
            peak = peak.max(m);
            sum_sq += f64::from(m) * f64::from(m);
        }
        let rms = (sum_sq / count as f64).sqrt() as f32;
        let db = amplitude_to_db(peak * 0.65 + rms * 0.35);
        bars.push(db_to_unit(db));
    }
    bars
}

pub fn amplitude_to_db(amp: f32) -> f32 {
    if amp <= 1e-6 || !amp.is_finite() {
        DB_FLOOR
    } else {
        (20.0 * amp.log10()).clamp(DB_FLOOR, 6.0)
    }
}

pub fn db_to_unit(db: f32) -> f32 {
    ((db - DB_FLOOR) / (0.0 - DB_FLOOR)).clamp(0.0, 1.0)
}

pub fn heatmap_rgb(unit: f32) -> (u8, u8, u8) {
    let t = unit.clamp(0.0, 1.0);
    if t < 0.25 {
        let k = t / 0.25;
        (
            (12.0 + k * 20.0) as u8,
            (16.0 + k * 70.0) as u8,
            (40.0 + k * 140.0) as u8,
        )
    } else if t < 0.55 {
        let k = (t - 0.25) / 0.30;
        (
            (32.0 + k * 20.0) as u8,
            (86.0 + k * 140.0) as u8,
            (180.0 + k * 40.0) as u8,
        )
    } else if t < 0.80 {
        let k = (t - 0.55) / 0.25;
        (
            (52.0 + k * 195.0) as u8,
            (226.0 - k * 20.0) as u8,
            (220.0 - k * 170.0) as u8,
        )
    } else {
        let k = (t - 0.80) / 0.20;
        (247, (206.0 - k * 140.0) as u8, (50.0 + k * 40.0) as u8)
    }
}

fn compute_log_bands(num_bands: usize, f_min: f32, f_max: f32) -> Vec<(f32, f32)> {
    let ratio = (f_max / f_min).powf(1.0 / (num_bands as f32));
    let mut bands = Vec::with_capacity(num_bands);
    let mut lo = f_min;
    for _ in 0..num_bands {
        let hi = lo * ratio;
        bands.push((lo, hi));
        lo = hi;
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_analyzer_locates_1khz_sine_peak_band() {
        let mut analyzer = SpectroscopeAnalyzer::new(48_000);
        let mut stereo = Vec::with_capacity(FFT_SIZE * 2);
        let freq = 1000.0f32;
        for i in 0..FFT_SIZE {
            let s = (2.0 * PI * freq * (i as f32) / 48_000.0).sin() * 0.8;
            stereo.push(s);
            stereo.push(s);
        }
        analyzer.push_stereo_samples(&stereo);

        let (peak_band, &peak_db) = analyzer
            .smoothed_bands_db
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .expect("max band");
        let center_hz = analyzer.band_center_hz(peak_band);
        assert!(
            peak_db > -15.0,
            "expected strong peak near 0 dBFS, got {peak_db} dB"
        );
        assert!(
            (center_hz - 1000.0).abs() < 200.0,
            "expected peak band near 1000 Hz, got {center_hz} Hz (band {peak_band})"
        );
        assert!(analyzer.peak_dbfs[0] > -4.0);
        assert_eq!(analyzer.scope_triggered.len(), SCOPE_WINDOW_FRAMES);
    }
}
