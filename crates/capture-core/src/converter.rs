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
                for pair in self.decode_scratch.as_chunks::<2>().0 {
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
            for chunk in in_bytes.as_chunks::<4>().0 {
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
                    for pair in self.decode_scratch.as_chunks::<2>().0 {
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
                for pair in self.decode_scratch.as_chunks::<2>().0 {
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

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    pub fn buffered_frames(&self) -> usize {
        self.len_samples / self.channels
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len_samples = 0;
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> Result<(), Reason> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(Reason::IncompleteFrame);
        }
        let cap = self.ring.len();
        if self.len_samples + samples.len() > cap {
            return Err(Reason::Capacity(CapacityError::ExceedsByteCeiling));
        }
        let tail = (self.head + self.len_samples) % cap;
        let first = samples.len().min(cap - tail);
        self.ring[tail..tail + first].copy_from_slice(&samples[..first]);
        if first < samples.len() {
            let rem = samples.len() - first;
            self.ring[..rem].copy_from_slice(&samples[first..]);
        }
        self.len_samples += samples.len();
        Ok(())
    }

    pub fn pop_block_into(&mut self, out_samples: &mut [f32]) -> Result<bool, Reason> {
        let needed = self.block_frames * self.channels;
        if out_samples.len() < needed {
            return Err(Reason::IncompleteFrame);
        }
        if self.len_samples < needed {
            return Ok(false);
        }
        let cap = self.ring.len();
        let first = needed.min(cap - self.head);
        out_samples[..first].copy_from_slice(&self.ring[self.head..self.head + first]);
        if first < needed {
            let rem = needed - first;
            out_samples[first..needed].copy_from_slice(&self.ring[..rem]);
        }
        self.head = (self.head + needed) % cap;
        self.len_samples -= needed;
        Ok(true)
    }

    pub fn peek_into(&self, frame_offset: usize, out_samples: &mut [f32]) -> Result<bool, Reason> {
        if !out_samples.len().is_multiple_of(self.channels) {
            return Err(Reason::IncompleteFrame);
        }
        let offset_samples = frame_offset
            .checked_mul(self.channels)
            .ok_or(Reason::Capacity(CapacityError::Overflow))?;
        let needed = out_samples.len();
        if offset_samples.saturating_add(needed) > self.len_samples {
            return Ok(false);
        }
        let cap = self.ring.len();
        let start = (self.head + offset_samples) % cap;
        let first = needed.min(cap - start);
        out_samples[..first].copy_from_slice(&self.ring[start..start + first]);
        if first < needed {
            let rem = needed - first;
            out_samples[first..needed].copy_from_slice(&self.ring[..rem]);
        }
        Ok(true)
    }

    pub fn discard_frames(&mut self, frames: usize) -> usize {
        let drop_samples = frames.saturating_mul(self.channels).min(self.len_samples);
        let cap = self.ring.len();
        self.head = (self.head + drop_samples) % cap;
        self.len_samples -= drop_samples;
        drop_samples / self.channels
    }
}

fn decode_samples_sanitized(
    in_bytes: &[u8],
    sample_type: SampleType,
    order: ByteOrder,
    out_f32: &mut Vec<f32>,
    nonfinite_count: &mut u64,
) -> Result<(), Reason> {
    match sample_type {
        SampleType::F32 => {
            if !in_bytes.len().is_multiple_of(4) {
                return Err(Reason::IncompleteFrame);
            }
            for chunk in in_bytes.as_chunks::<4>().0 {
                let b: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
                let val = match order {
                    ByteOrder::Little => f32::from_le_bytes(b),
                    ByteOrder::Big => f32::from_be_bytes(b),
                };
                if val.is_finite() {
                    out_f32.push(val);
                } else {
                    *nonfinite_count = nonfinite_count.saturating_add(1);
                    out_f32.push(0.0);
                }
            }
        }
        SampleType::I16 => {
            if !in_bytes.len().is_multiple_of(2) {
                return Err(Reason::IncompleteFrame);
            }
            for chunk in in_bytes.as_chunks::<2>().0 {
                let b: [u8; 2] = [chunk[0], chunk[1]];
                let val = match order {
                    ByteOrder::Little => i16::from_le_bytes(b),
                    ByteOrder::Big => i16::from_be_bytes(b),
                };
                out_f32.push((f32::from(val) / 32768.0).clamp(-1.0, 1.0));
            }
        }
        SampleType::I24 => {
            if !in_bytes.len().is_multiple_of(3) {
                return Err(Reason::IncompleteFrame);
            }
            for chunk in in_bytes.as_chunks::<3>().0 {
                let signed = match order {
                    ByteOrder::Little => {
                        let raw = u32::from(chunk[0])
                            | (u32::from(chunk[1]) << 8)
                            | (u32::from(chunk[2]) << 16);
                        ((raw << 8) as i32) >> 8
                    }
                    ByteOrder::Big => {
                        let raw = (u32::from(chunk[0]) << 16)
                            | (u32::from(chunk[1]) << 8)
                            | u32::from(chunk[2]);
                        ((raw << 8) as i32) >> 8
                    }
                };
                out_f32.push(((signed as f64) / 8_388_608.0).clamp(-1.0, 1.0) as f32);
            }
        }
        SampleType::I32 => {
            if !in_bytes.len().is_multiple_of(4) {
                return Err(Reason::IncompleteFrame);
            }
            for chunk in in_bytes.as_chunks::<4>().0 {
                let b: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
                let val = match order {
                    ByteOrder::Little => i32::from_le_bytes(b),
                    ByteOrder::Big => i32::from_be_bytes(b),
                };
                out_f32.push(((val as f64) / 2_147_483_648.0).clamp(-1.0, 1.0) as f32);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_samples_quantized(
    samples: &[f32],
    sample_type: SampleType,
    order: ByteOrder,
    dither: DitherMode,
    seed: u64,
    sample_counter: &mut u64,
    clipped_count: &mut u64,
    out_bytes: &mut Vec<u8>,
) {
    for &raw in samples {
        let idx = *sample_counter;
        *sample_counter = sample_counter.wrapping_add(1);
        let clean = if raw.is_finite() { f64::from(raw) } else { 0.0 };
        if clean.abs() > 1.0 {
            *clipped_count = clipped_count.saturating_add(1);
        }
        let dither_lsb = match dither {
            DitherMode::Tpdf => tpdf_dither_lsb(seed, idx),
            DitherMode::None => 0.0,
        };
        match sample_type {
            SampleType::F32 => {
                let val = clean as f32;
                let bytes = match order {
                    ByteOrder::Little => val.to_le_bytes(),
                    ByteOrder::Big => val.to_be_bytes(),
                };
                out_bytes.extend_from_slice(&bytes);
            }
            SampleType::I16 => {
                let q = (clean * 32767.0 + dither_lsb)
                    .round()
                    .clamp(-32768.0, 32767.0) as i16;
                let bytes = match order {
                    ByteOrder::Little => q.to_le_bytes(),
                    ByteOrder::Big => q.to_be_bytes(),
                };
                out_bytes.extend_from_slice(&bytes);
            }
            SampleType::I24 => {
                let q = (clean * 8_388_607.0 + dither_lsb)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0) as i32;
                let le = q.to_le_bytes();
                match order {
                    ByteOrder::Little => out_bytes.extend_from_slice(&le[..3]),
                    ByteOrder::Big => {
                        out_bytes.push(le[2]);
                        out_bytes.push(le[1]);
                        out_bytes.push(le[0]);
                    }
                }
            }
            SampleType::I32 => {
                let q = (clean * 2_147_483_647.0 + dither_lsb)
                    .round()
                    .clamp(-2_147_483_648.0, 2_147_483_647.0) as i32;
                let bytes = match order {
                    ByteOrder::Little => q.to_le_bytes(),
                    ByteOrder::Big => q.to_be_bytes(),
                };
                out_bytes.extend_from_slice(&bytes);
            }
        }
    }
}

fn resample_sinc_into(
    input: &[f32],
    channels: usize,
    in_rate: u32,
    out_rate: u32,
    half_wing_taps: usize,
    out: &mut Vec<f32>,
) -> usize {
    let in_frames = input.len() / channels;
    let out_frames = ((in_frames as u64) * u64::from(out_rate) / u64::from(in_rate)) as usize;
    out.clear();
    let ratio = f64::from(out_rate) / f64::from(in_rate);
    let cutoff = ratio.min(1.0) * 0.45;

    for out_f in 0..out_frames {
        let center = (out_f as f64) / ratio;
        let base = center.floor() as isize;
        for ch in 0..channels {
            let mut acc = 0.0_f64;
            let mut weight_sum = 0.0_f64;
            for tap in -(half_wing_taps as isize)..=(half_wing_taps as isize) {
                let src_f = base + tap;
                let x = (src_f as f64) - center;
                let window_pos = x / (half_wing_taps as f64);
                if window_pos.abs() >= 1.0 {
                    continue;
                }
                let w = 0.42
                    + 0.5 * (std::f64::consts::PI * window_pos).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * window_pos).cos();
                let sinc_arg = 2.0 * cutoff * x;
                let sinc = if sinc_arg.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * sinc_arg).sin() / (std::f64::consts::PI * sinc_arg)
                };
                let coeff = 2.0 * cutoff * sinc * w;
                weight_sum += coeff;
                if src_f >= 0 && (src_f as usize) < in_frames {
                    let s = input[(src_f as usize) * channels + ch];
                    if s.is_finite() {
                        acc += f64::from(s) * coeff;
                    }
                }
            }
            if weight_sum.abs() > 1e-12 {
                acc /= weight_sum;
            }
            out.push(acc as f32);
        }
    }
    out_frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn output_converter_quantizes_dithers_clamps_and_maps_layouts_with_zero_steady_allocations() {
        let in_fmt = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48000,
            1,
            Layout::Known(Arc::from([Channel::Mono])),
        )
        .unwrap();
        let out_fmt = Format::new(
            SampleType::I16,
            ByteOrder::Little,
            16000,
            2,
            Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
        )
        .unwrap();

        let mut conv = OutputConverter::new(
            in_fmt.clone(),
            OutputConverterConfig::new(out_fmt.clone(), DitherMode::Tpdf).with_seed(0xC59A_2026),
            480,
        )
        .unwrap();
        assert_eq!(conv.group_delay_input_frames(), 16);

        let mut in_bytes = Vec::with_capacity(480 * 4);
        for i in 0..480 {
            let s = if i == 0 {
                f32::NAN
            } else if i == 1 {
                1.5
            } else if i == 2 {
                -1.5
            } else {
                ((2.0 * std::f32::consts::PI * 1000.0 * (i as f32) / 48000.0).sin()) * 0.5
            };
            in_bytes.extend_from_slice(&s.to_le_bytes());
        }

        let mut out_bytes = Vec::with_capacity(160 * 4);
        let frames = conv.convert_into(&in_bytes, &mut out_bytes).unwrap();
        assert_eq!(frames, 160);
        assert_eq!(out_bytes.len(), 160 * 4);
        assert_eq!(conv.stats().nonfinite_samples, 1);

        let measured = allocation_counter::measure(|| {
            for _ in 0..1000 {
                let f = conv.convert_into(&in_bytes, &mut out_bytes).unwrap();
                assert_eq!(f, 160);
            }
        });
        assert_eq!(measured.count_total, 0);
    }

    #[test]
    fn output_converter_rejects_ambiguous_layouts_and_handles_on_format_change() {
        let unknown_stereo = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48000,
            2,
            Layout::Unknown,
        )
        .unwrap();
        let unknown_mono = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48000,
            1,
            Layout::Unknown,
        )
        .unwrap();
        assert_eq!(
            validate_channel_mapping(&unknown_stereo, &unknown_mono),
            Err(Reason::UnsupportedChannelMapping)
        );

        let stereo_96k = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            96000,
            2,
            Layout::Unknown,
        )
        .unwrap();
        assert_eq!(
            evaluate_format_change(&unknown_stereo, &stereo_96k, OnFormatChange::Adapt, None),
            Ok(true)
        );
        assert_eq!(
            evaluate_format_change(&unknown_stereo, &stereo_96k, OnFormatChange::Fail, None),
            Err(Reason::FormatChanged)
        );
    }

    #[test]
    fn input_normalizer_and_reblocker_preserve_seams_order_independence_and_zero_allocations() {
        let mono_16k_i16 = Format::new(
            SampleType::I16,
            ByteOrder::Little,
            16_000,
            1,
            Layout::Known(Arc::from([Channel::Mono])),
        )
        .unwrap();
        let stereo_48k_f32 = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48_000,
            2,
            Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
        )
        .unwrap();

        let (wf_ab, res_ab) = resolve_mixer_working_format(
            &[mono_16k_i16.clone(), stereo_48k_f32.clone()],
            None,
            InputConversionPolicy::AutoWhenMixing,
        )
        .unwrap();
        let (wf_ba, res_ba) = resolve_mixer_working_format(
            &[stereo_48k_f32.clone(), mono_16k_i16.clone()],
            None,
            InputConversionPolicy::AutoWhenMixing,
        )
        .unwrap();
        assert_eq!(wf_ab, stereo_48k_f32);
        assert_eq!(wf_ab, wf_ba);
        assert!(res_ab && res_ba);
        assert_eq!(
            resolve_mixer_working_format(
                &[mono_16k_i16.clone(), stereo_48k_f32.clone()],
                None,
                InputConversionPolicy::StrictNoResample,
            ),
            Err(PlanError::UnsupportedCapturePlan)
        );

        let bypass =
            InputNormalizer::new(stereo_48k_f32.clone(), stereo_48k_f32.clone(), 1024).unwrap();
        assert!(bypass.is_identity_bypass());
        assert!(!bypass.is_resampled());
        assert_eq!(bypass.group_delay_input_frames(), 0);

        // Prime-chunk vs continuous single-shot seam equivalence on 16k Mono I16 -> 48k Stereo F32
        let total_in_frames = 8_000usize;
        let mut raw_16k = Vec::with_capacity(total_in_frames * 2);
        for f in 0..total_in_frames {
            let t = (f as f64) / 16_000.0;
            let s =
                (0.75 * (2.0 * std::f64::consts::PI * 997.0 * t).sin() * 32767.0).round() as i16;
            raw_16k.extend_from_slice(&s.to_le_bytes());
        }

        let mut single_norm = InputNormalizer::new(
            mono_16k_i16.clone(),
            stereo_48k_f32.clone(),
            total_in_frames,
        )
        .unwrap();
        assert_eq!(single_norm.group_delay_input_frames(), 32);
        let mut single_out = Vec::with_capacity(total_in_frames * 6);
        let single_frames = single_norm
            .normalize_into(&raw_16k, &mut single_out)
            .unwrap();
        assert!(single_frames > 23_800);

        let mut chunk_norm =
            InputNormalizer::new(mono_16k_i16.clone(), stereo_48k_f32.clone(), 1024).unwrap();
        let mut chunk_tmp = Vec::with_capacity(1024 * 8);
        let mut chunk_all = Vec::with_capacity(single_out.len());
        let mut cursor = 0usize;
        for &step in [67usize, 137, 320, 441, 480, 1024].iter().cycle() {
            if cursor >= total_in_frames {
                break;
            }
            let take = step.min(total_in_frames - cursor);
            chunk_norm
                .normalize_into(&raw_16k[cursor * 2..(cursor + take) * 2], &mut chunk_tmp)
                .unwrap();
            chunk_all.extend_from_slice(&chunk_tmp);
            cursor += take;
        }
        assert_eq!(chunk_all.len(), single_out.len());
        let max_seam_diff = single_out
            .iter()
            .zip(chunk_all.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_seam_diff <= 1e-6,
            "unexpected seam diff: {max_seam_diff}"
        );

        let mut reblocker = FrameReblocker::new(2, 480, 480 * 8).unwrap();
        let packet_160 = &raw_16k[..160 * 2]; // 10 ms at 16 kHz -> 480 frames at 48 kHz
        let mut block_out = vec![0.0_f32; 480 * 2];
        chunk_norm
            .normalize_into(packet_160, &mut chunk_tmp)
            .unwrap();
        reblocker.push_samples(&chunk_tmp).unwrap();
        let _ = reblocker.pop_block_into(&mut block_out).unwrap();

        let allocs = allocation_counter::measure(|| {
            for _ in 0..1_000 {
                chunk_norm
                    .normalize_into(packet_160, &mut chunk_tmp)
                    .unwrap();
                reblocker.push_samples(&chunk_tmp).unwrap();
                while reblocker.pop_block_into(&mut block_out).unwrap() {}
            }
        });
        assert_eq!(allocs.count_total, 0);
    }
}
