use crate::CapacityError;
use crate::schema::{
    ByteOrder, Channel, DitherMode, Format, InputConversionPolicy, Layout, OnFormatChange,
    OutputConverterConfig, PlanError, Reason, SampleType,
};
use std::sync::Arc;

pub const MAX_RESAMPLER_HALF_WING_TAPS: usize = 16;
pub const INPUT_NORMALIZER_HALF_WING_TAPS: usize = 32;
pub const INPUT_NORMALIZER_TAPS: usize = 64; // k in -31..=32
pub const INPUT_NORMALIZER_POLYPHASE_STEPS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelMappingKind {
    Identity,
    MonoToStereo,
    StereoToMono,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConversionStats {
    pub input_frames: u64,
    pub output_frames: u64,
    pub clipped_samples: u64,
    pub nonfinite_samples: u64,
    pub reset_boundaries: u64,
    pub destination_writes: u64,
}

/// Deterministic stateless SplitMix64 TPDF dither in `[-1.0, +1.0]` LSB.
pub fn tpdf_dither_lsb(seed: u64, sample_index: u64) -> f64 {
    fn splitmix64(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }
    let u1 = splitmix64(seed ^ sample_index.wrapping_mul(2));
    let u2 = splitmix64(seed ^ sample_index.wrapping_mul(2).wrapping_add(1));
    let r1 = ((u1 >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
    let r2 = ((u2 >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
    r1 - r2
}

pub fn validate_channel_mapping(
    input_format: &Format,
    output_format: &Format,
) -> Result<ChannelMappingKind, Reason> {
    if input_format.channels() == output_format.channels() {
        match (input_format.layout(), output_format.layout()) {
            (Layout::Known(in_labels), Layout::Known(out_labels)) if in_labels != out_labels => {
                return Err(Reason::UnsupportedChannelMapping);
            }
            _ => return Ok(ChannelMappingKind::Identity),
        }
    }

    match (input_format.layout(), output_format.layout()) {
        (Layout::Known(in_labels), Layout::Known(out_labels)) => {
            if in_labels.as_ref() == [Channel::Mono]
                && out_labels.as_ref() == [Channel::FrontLeft, Channel::FrontRight]
            {
                Ok(ChannelMappingKind::MonoToStereo)
            } else if in_labels.as_ref() == [Channel::FrontLeft, Channel::FrontRight]
                && out_labels.as_ref() == [Channel::Mono]
            {
                Ok(ChannelMappingKind::StereoToMono)
            } else {
                Err(Reason::UnsupportedChannelMapping)
            }
        }
        _ => Err(Reason::UnsupportedChannelMapping),
    }
}

/// Preallocated output converter running off acquisition threads.
pub struct OutputConverter {
    input_format: Format,
    config: OutputConverterConfig,
    mapping: ChannelMappingKind,
    sample_counter: u64,
    stats: ConversionStats,
    decode_scratch: Vec<f32>,
    mapped_scratch: Vec<f32>,
    resampled_scratch: Vec<f32>,
}

impl OutputConverter {
    pub fn new(
        input_format: Format,
        config: OutputConverterConfig,
        max_frames_per_block: usize,
    ) -> Result<Self, Reason> {
        if max_frames_per_block == 0 {
            return Err(Reason::InvalidFormat);
        }
        let mapping = validate_channel_mapping(&input_format, &config.target_format)?;
        let in_ch = usize::from(input_format.channels());
        let out_ch = usize::from(config.target_format.channels());
        let max_out_frames = (max_frames_per_block as u64)
            .saturating_mul(u64::from(config.target_format.rate()))
            .div_ceil(u64::from(input_format.rate())) as usize
            + 64;

        Ok(Self {
            input_format,
            config,
            mapping,
            sample_counter: 0,
            stats: ConversionStats::default(),
            decode_scratch: Vec::with_capacity(max_frames_per_block * in_ch),
            mapped_scratch: Vec::with_capacity(max_frames_per_block * out_ch),
            resampled_scratch: Vec::with_capacity(max_out_frames * out_ch),
        })
    }

    pub fn input_format(&self) -> &Format {
        &self.input_format
    }

    pub fn output_format(&self) -> &Format {
        &self.config.target_format
    }

    pub fn group_delay_input_frames(&self) -> usize {
        if self.input_format.rate() == self.config.target_format.rate() {
            0
        } else {
            MAX_RESAMPLER_HALF_WING_TAPS
        }
    }

    pub fn stats(&self) -> ConversionStats {
        self.stats
    }

    /// Reset converter history at a continuity/format boundary or reconfigure for a new native input format.
    pub fn reset_for_input_format(&mut self, new_input_format: Format) -> Result<(), Reason> {
        let mapping = validate_channel_mapping(&new_input_format, &self.config.target_format)?;
        self.input_format = new_input_format;
        self.mapping = mapping;
        self.stats.reset_boundaries = self.stats.reset_boundaries.saturating_add(1);
        Ok(())
    }

    /// Convert packed input PCM bytes into `out_bytes` without heap allocation when within preallocated capacity.
    /// Returns the number of output frames written into `out_bytes`.
    pub fn convert_into(
        &mut self,
        in_bytes: &[u8],
        out_bytes: &mut Vec<u8>,
    ) -> Result<usize, Reason> {
        let in_frames = self.input_format.frames_in(in_bytes.len())?;
        let out_ch = usize::from(self.config.target_format.channels());

        self.decode_scratch.clear();
        decode_samples_sanitized(
            in_bytes,
            self.input_format.sample(),
            self.input_format.order(),
            &mut self.decode_scratch,
            &mut self.stats.nonfinite_samples,
        )?;

        self.mapped_scratch.clear();
        match self.mapping {
            ChannelMappingKind::Identity => {
                self.mapped_scratch.extend_from_slice(&self.decode_scratch);
            }
            ChannelMappingKind::MonoToStereo => {
                for &sample in &self.decode_scratch {
                    self.mapped_scratch.push(sample);
                    self.mapped_scratch.push(sample);
                }
            }
            ChannelMappingKind::StereoToMono => {
                for pair in self.decode_scratch.chunks_exact(2) {
                    self.mapped_scratch.push(0.5 * (pair[0] + pair[1]));
                }
            }
        }
        debug_assert_eq!(self.mapped_scratch.len(), in_frames * out_ch);

        let in_rate = self.input_format.rate();
        let out_rate = self.config.target_format.rate();
        let out_frames = if in_rate == out_rate {
            self.resampled_scratch.clear();
            self.resampled_scratch
                .extend_from_slice(&self.mapped_scratch);
            in_frames
        } else {
            resample_sinc_into(
                &self.mapped_scratch,
                out_ch,
                in_rate,
                out_rate,
                MAX_RESAMPLER_HALF_WING_TAPS,
                &mut self.resampled_scratch,
            )
        };

        out_bytes.clear();
        encode_samples_quantized(
            &self.resampled_scratch,
            self.config.target_format.sample(),
            self.config.target_format.order(),
            self.config.dither,
            self.config.seed,
            &mut self.sample_counter,
            &mut self.stats.clipped_samples,
            out_bytes,
        );

        self.stats.input_frames = self.stats.input_frames.saturating_add(in_frames as u64);
        self.stats.output_frames = self.stats.output_frames.saturating_add(out_frames as u64);
        self.stats.destination_writes = self.stats.destination_writes.saturating_add(1);
        Ok(out_frames)
    }
}

/// Evaluate mid-stream native format transition policy (`on_format_change="adapt" | "fail"`).
pub fn evaluate_format_change(
    previous: &Format,
    next: &Format,
    policy: OnFormatChange,
    converter: Option<&mut OutputConverter>,
) -> Result<bool, Reason> {
    if previous == next {
        return Ok(false);
    }
    if let Some(conv) = converter {
        conv.reset_for_input_format(next.clone())?;
        return Ok(true);
    }
    match policy {
        OnFormatChange::Adapt => Ok(true),
        OnFormatChange::Fail => Err(Reason::FormatChanged),
    }
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}

#[inline]
fn blackman_harris_4term(v: f64) -> f64 {
    if v.abs() >= 1.0 {
        return 0.0;
    }
    let pi = std::f64::consts::PI;
    0.35875
        + 0.48829 * (pi * v).cos()
        + 0.14128 * (2.0 * pi * v).cos()
        + 0.01168 * (3.0 * pi * v).cos()
}

/// Resolve order-independent `CspaMixer` `F32` working format across native input streams.
/// Returns `(working_format, any_input_resampled)`.
pub fn resolve_mixer_working_format(
    input_formats: &[Format],
    output_converter: Option<&OutputConverterConfig>,
    policy: InputConversionPolicy,
) -> Result<(Format, bool), PlanError> {
    if input_formats.is_empty() {
        return Err(PlanError::InvalidTarget);
    }
    let first = &input_formats[0];
    let all_same_rate = input_formats.iter().all(|f| f.rate() == first.rate());
    let all_same_channels = input_formats
        .iter()
        .all(|f| f.channels() == first.channels() && f.layout() == first.layout());

    if !all_same_rate && policy == InputConversionPolicy::StrictNoResample {
        return Err(PlanError::UnsupportedCapturePlan);
    }

    if all_same_rate && all_same_channels {
        let fmt = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            first.rate(),
            first.channels(),
            first.layout().clone(),
        )
        .map_err(|_| PlanError::UnsupportedCapturePlan)?;
        return Ok((fmt, false));
    }

    let mut has_mono = false;
    let mut has_stereo = false;
    for fmt in input_formats {
        match fmt.layout() {
            Layout::Known(labels) if labels.as_ref() == [Channel::Mono] => {
                has_mono = true;
            }
            Layout::Known(labels)
                if labels.as_ref() == [Channel::FrontLeft, Channel::FrontRight] =>
            {
                has_stereo = true;
            }
            _ => return Err(PlanError::UnsupportedCapturePlan),
        }
    }

    let (out_channels, out_layout) = if has_stereo {
        (
            2u16,
            Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
        )
    } else if has_mono {
        (1u16, Layout::Known(Arc::from([Channel::Mono])))
    } else {
        return Err(PlanError::UnsupportedCapturePlan);
    };

    let max_in_rate = input_formats
        .iter()
        .map(Format::rate)
        .max()
        .unwrap_or(48_000);
    let cap_rate = output_converter
        .map_or(48_000, |c| c.target_format.rate())
        .max(48_000);
    let working_rate = if max_in_rate > 48_000 {
        max_in_rate.max(cap_rate)
    } else {
        max_in_rate.min(cap_rate)
    };
    let requires_resample = input_formats.iter().any(|f| f.rate() != working_rate);
    let working = Format::new(
        SampleType::F32,
        ByteOrder::Little,
        working_rate,
        out_channels,
        out_layout,
    )
    .map_err(|_| PlanError::UnsupportedCapturePlan)?;
    Ok((working, requires_resample))
}

fn build_polyphase_sinc_table(in_rate: u32, out_rate: u32) -> Vec<[f32; INPUT_NORMALIZER_TAPS]> {
    if in_rate == out_rate {
        return Vec::new();
    }
    let ratio = f64::from(out_rate) / f64::from(in_rate);
    let cutoff = 0.425 * ratio.min(1.0);
    let radius = INPUT_NORMALIZER_HALF_WING_TAPS as f64;
    let mut table = Vec::with_capacity(INPUT_NORMALIZER_POLYPHASE_STEPS + 1);

    for phase_idx in 0..=INPUT_NORMALIZER_POLYPHASE_STEPS {
        let mu = (phase_idx as f64) / (INPUT_NORMALIZER_POLYPHASE_STEPS as f64);
        let mut raw_weights = [0.0_f64; INPUT_NORMALIZER_TAPS];
        let mut sum = 0.0_f64;
        for (t_idx, tap) in (-((INPUT_NORMALIZER_HALF_WING_TAPS as isize) - 1)
            ..=(INPUT_NORMALIZER_HALF_WING_TAPS as isize))
            .enumerate()
        {
            let x = (tap as f64) - mu;
            let w = blackman_harris_4term(x / radius);
            let arg = 2.0 * cutoff * x;
            let sinc = if arg.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg)
            };
            let c = 2.0 * cutoff * sinc * w;
            raw_weights[t_idx] = c;
            sum += c;
        }
        let inv = if sum.abs() > 1e-15 { 1.0 / sum } else { 0.0 };
        let mut row = [0.0_f32; INPUT_NORMALIZER_TAPS];
        for (dst, &w) in row.iter_mut().zip(raw_weights.iter()) {
            *dst = (w * inv) as f32;
        }
        table.push(row);
    }
    table
}

/// Per-input decoding, channel mapping, and stateful resampling before `CspaMixer`.
/// Uses no heap allocations in steady state.
pub struct InputNormalizer {
    input_format: Format,
    working_format: Format,
    mapping: ChannelMappingKind,
    identity_bypass: bool,
    step_p: u64,
    step_q: u64,
    phase_num: u64,
    cursor_in: usize,
    polyphase_table: Vec<[f32; INPUT_NORMALIZER_TAPS]>,
    decode_scratch: Vec<f32>,
    mapped_scratch: Vec<f32>,
    history: Vec<f32>,
    stats: ConversionStats,
}

impl InputNormalizer {
    pub fn new(
        input_format: Format,
        working_format: Format,
        max_input_frames_per_call: usize,
    ) -> Result<Self, Reason> {
        if max_input_frames_per_call == 0 || working_format.sample() != SampleType::F32 {
            return Err(Reason::InvalidFormat);
        }
        let mapping = validate_channel_mapping(&input_format, &working_format)?;
        let identity_bypass = input_format.sample() == SampleType::F32
            && input_format.order() == ByteOrder::Little
            && input_format.rate() == working_format.rate()
            && mapping == ChannelMappingKind::Identity;

        let in_ch = usize::from(input_format.channels());
        let out_ch = usize::from(working_format.channels());
        let g = gcd_u64(
            u64::from(input_format.rate()),
            u64::from(working_format.rate()),
        );
        let step_p = u64::from(input_format.rate()) / g;
        let step_q = u64::from(working_format.rate()) / g;
        let lead_zeros = INPUT_NORMALIZER_HALF_WING_TAPS - 1;
        let is_resampled = input_format.rate() != working_format.rate();
        let polyphase_table =
            build_polyphase_sinc_table(input_format.rate(), working_format.rate());

        let (decode_cap, mapped_cap, history_cap) = if identity_bypass {
            (0, 0, 0)
        } else if !is_resampled {
            (
                max_input_frames_per_call * in_ch,
                max_input_frames_per_call * out_ch,
                0,
            )
        } else {
            (
                max_input_frames_per_call * in_ch,
                max_input_frames_per_call * out_ch,
                (INPUT_NORMALIZER_TAPS + max_input_frames_per_call + 64) * out_ch,
            )
        };

        let mut history = Vec::with_capacity(history_cap);
        if is_resampled {
            history.resize(lead_zeros * out_ch, 0.0);
        }

        Ok(Self {
            input_format,
            working_format,
            mapping,
            identity_bypass,
            step_p,
            step_q,
            phase_num: 0,
            cursor_in: lead_zeros,
            polyphase_table,
            decode_scratch: Vec::with_capacity(decode_cap),
            mapped_scratch: Vec::with_capacity(mapped_cap),
            history,
            stats: ConversionStats::default(),
        })
    }

    pub fn input_format(&self) -> &Format {
        &self.input_format
    }

    pub fn working_format(&self) -> &Format {
        &self.working_format
    }

    pub fn is_identity_bypass(&self) -> bool {
        self.identity_bypass
    }

    pub fn is_resampled(&self) -> bool {
        self.input_format.rate() != self.working_format.rate()
    }

    pub fn group_delay_input_frames(&self) -> usize {
        if self.is_resampled() {
            INPUT_NORMALIZER_HALF_WING_TAPS
        } else {
            0
        }
    }

    pub fn max_output_frames(&self, max_input_frames: usize) -> usize {
        if !self.is_resampled() {
            max_input_frames
        } else {
            ((max_input_frames as u64)
                .saturating_mul(u64::from(self.working_format.rate()))
                .div_ceil(u64::from(self.input_format.rate())) as usize)
                + 8
        }
    }

    pub fn stats(&self) -> ConversionStats {
        self.stats
    }

    pub fn reset(&mut self) {
        let out_ch = usize::from(self.working_format.channels());
        let lead_zeros = INPUT_NORMALIZER_HALF_WING_TAPS - 1;
        self.phase_num = 0;
        self.cursor_in = lead_zeros;
        self.history.clear();
        if self.is_resampled() {
            self.history.resize(lead_zeros * out_ch, 0.0);
        }
        self.stats.reset_boundaries = self.stats.reset_boundaries.saturating_add(1);
    }

    pub fn reset_for_input_format(
        &mut self,
        new_input_format: Format,
        max_input_frames_per_call: usize,
    ) -> Result<(), Reason> {
        *self = Self::new(
            new_input_format,
            self.working_format.clone(),
            max_input_frames_per_call,
        )?;
        self.stats.reset_boundaries = 1;
        Ok(())
    }

    /// Normalize packed input PCM bytes into `out_f32` (`working_format` interleaved `f32`)
    /// with `0` heap allocations when within preallocated `max_input_frames_per_call` capacity.
    pub fn normalize_into(
        &mut self,
        in_bytes: &[u8],
        out_f32: &mut Vec<f32>,
    ) -> Result<usize, Reason> {
        let in_frames = self.input_format.frames_in(in_bytes.len())?;
        out_f32.clear();
        if in_frames == 0 {
            return Ok(0);
        }

        if self.identity_bypass {
            for chunk in in_bytes.chunks_exact(4) {
                let val = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if val.is_finite() {
                    out_f32.push(val);
                } else {
                    self.stats.nonfinite_samples = self.stats.nonfinite_samples.saturating_add(1);
                    out_f32.push(0.0);
                }
            }
            self.stats.input_frames = self.stats.input_frames.saturating_add(in_frames as u64);
            self.stats.output_frames = self.stats.output_frames.saturating_add(in_frames as u64);
            self.stats.destination_writes = self.stats.destination_writes.saturating_add(1);
            return Ok(in_frames);
        }

        let out_ch = usize::from(self.working_format.channels());
        if in_frames * usize::from(self.input_format.channels()) > self.decode_scratch.capacity() {
            return Err(Reason::Capacity(CapacityError::ExceedsByteCeiling));
        }

        self.decode_scratch.clear();
        decode_samples_sanitized(
            in_bytes,
            self.input_format.sample(),
            self.input_format.order(),
            &mut self.decode_scratch,
            &mut self.stats.nonfinite_samples,
        )?;

        if !self.is_resampled() {
            match self.mapping {
                ChannelMappingKind::Identity => {
                    out_f32.extend_from_slice(&self.decode_scratch);
                }
                ChannelMappingKind::MonoToStereo => {
                    for &s in &self.decode_scratch {
                        out_f32.push(s);
                        out_f32.push(s);
                    }
                }
                ChannelMappingKind::StereoToMono => {
                    for pair in self.decode_scratch.chunks_exact(2) {
                        out_f32.push(0.5 * (pair[0] + pair[1]));
                    }
                }
            }
            self.stats.input_frames = self.stats.input_frames.saturating_add(in_frames as u64);
            self.stats.output_frames = self.stats.output_frames.saturating_add(in_frames as u64);
            self.stats.destination_writes = self.stats.destination_writes.saturating_add(1);
            return Ok(in_frames);
        }

        self.mapped_scratch.clear();
        match self.mapping {
            ChannelMappingKind::Identity => {
                self.mapped_scratch.extend_from_slice(&self.decode_scratch);
            }
            ChannelMappingKind::MonoToStereo => {
                for &s in &self.decode_scratch {
                    self.mapped_scratch.push(s);
                    self.mapped_scratch.push(s);
                }
            }
            ChannelMappingKind::StereoToMono => {
                for pair in self.decode_scratch.chunks_exact(2) {
                    self.mapped_scratch.push(0.5 * (pair[0] + pair[1]));
                }
            }
        }

        if self.history.len() + self.mapped_scratch.len() > self.history.capacity() {
            return Err(Reason::Capacity(CapacityError::ExceedsByteCeiling));
        }
        self.history.extend_from_slice(&self.mapped_scratch);
        let frames_in_buf = self.history.len() / out_ch;
        let scale_num = self.step_q;
        let table_steps = INPUT_NORMALIZER_POLYPHASE_STEPS as u128;
        let mut produced_frames = 0usize;

        while self.cursor_in + INPUT_NORMALIZER_HALF_WING_TAPS < frames_in_buf {
            let scaled = u128::from(self.phase_num) * table_steps;
            let idx0 = (scaled / u128::from(scale_num)) as usize;
            let rem = (scaled % u128::from(scale_num)) as f64;
            let frac = (rem / (scale_num as f64)) as f32;
            let row0 = &self.polyphase_table[idx0];
            let row1 = &self.polyphase_table[idx0 + 1];
            let base_frame = self.cursor_in - (INPUT_NORMALIZER_HALF_WING_TAPS - 1);

            for ch in 0..out_ch {
                let mut acc = 0.0_f64;
                for t_idx in 0..INPUT_NORMALIZER_TAPS {
                    let w = row0[t_idx] + frac * (row1[t_idx] - row0[t_idx]);
                    let s = self.history[(base_frame + t_idx) * out_ch + ch];
                    acc += f64::from(s) * f64::from(w);
                }
                out_f32.push(acc as f32);
            }
            produced_frames += 1;

            let next_num = self.phase_num + self.step_p;
            self.cursor_in += (next_num / self.step_q) as usize;
            self.phase_num = next_num % self.step_q;
        }

        let keep_lead = INPUT_NORMALIZER_HALF_WING_TAPS - 1;
        let consumed = self.cursor_in.saturating_sub(keep_lead);
        if consumed > 0 {
            let total_samples = self.history.len();
            let drop_samples = consumed * out_ch;
            self.history.copy_within(drop_samples..total_samples, 0);
            self.history.truncate(total_samples - drop_samples);
            self.cursor_in -= consumed;
        }

        self.stats.input_frames = self.stats.input_frames.saturating_add(in_frames as u64);
        self.stats.output_frames = self
            .stats
            .output_frames
            .saturating_add(produced_frames as u64);
        self.stats.destination_writes = self.stats.destination_writes.saturating_add(1);
        Ok(produced_frames)
    }
}

/// Fixed-capacity ring-buffered frame reblocker slicing arbitrary normalized chunks into exact
/// `block_frames` blocks for `CspaMixer` with `0` heap allocations in steady state.
pub struct FrameReblocker {
    channels: usize,
    block_frames: usize,
    ring: Vec<f32>,
    head: usize,
    len_samples: usize,
}

impl FrameReblocker {
    pub fn new(
        channels: usize,
        block_frames: usize,
        max_buffered_frames: usize,
    ) -> Result<Self, Reason> {
        if channels == 0 || block_frames == 0 || max_buffered_frames < block_frames {
            return Err(Reason::InvalidFormat);
        }
        let cap_samples = max_buffered_frames
            .checked_mul(channels)
            .ok_or(Reason::Capacity(CapacityError::Overflow))?;
        Ok(Self {
            channels,
            block_frames,
            ring: vec![0.0_f32; cap_samples],
            head: 0,
            len_samples: 0,
        })
    }

