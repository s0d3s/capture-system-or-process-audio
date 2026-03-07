use crate::CapacityError;
use crate::schema::{
    CaptureTarget, Format, MembershipDescriptor, MembershipEntry, MixedBlockMetadata, MixerConfig,
    OnInputFailure, ParticipationState, PlanError, Provenance, Reason, SampleType, Stamp,
};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MixerInputPacket {
    pub samples: Vec<f32>,
    pub frames: usize,
    pub timestamp: Option<Stamp>,
    pub arrival_ns: u64,
}

#[derive(Clone, Debug)]
struct InputSlot {
    target_id: Arc<str>,
    gain: f32,
    state: ParticipationState,
    queue: VecDeque<MixerInputPacket>,
    recycle_buffers: Vec<Vec<f32>>,
    dropped_frames: u64,
    clipped_samples: u64,
    nonfinite_samples: u64,
    drift_slips: u64,
    sustained_drift_since_ns: Option<u64>,
    drift_armed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputDiagnosticSnapshot {
    pub input_index: usize,
    pub target_id: Arc<str>,
    pub participation_state: ParticipationState,
    pub queued_packets: usize,
    pub dropped_frames: u64,
    pub clipped_samples: u64,
    pub nonfinite_samples: u64,
    pub drift_slips: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixedOutputBlock {
    pub frames: usize,
    pub segment: u64,
    pub metadata: MixedBlockMetadata,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixStepOutcome {
    Produced(usize),
    ParkedIdle,
    Eof,
    Failed(u32),
}

/// Safe-Rust software mixer (`CspaMixer`) executing off native acquisition threads.
pub struct CspaMixer {
    format: Arc<Format>,
    config: MixerConfig,
    max_targets: usize,
    max_frames_per_block: usize,
    inputs: Vec<InputSlot>,
    spare_slots: Vec<InputSlot>,
    next_slots_scratch: Vec<InputSlot>,
    slot_taken_scratch: Vec<bool>,
    mapping_scratch: Vec<Option<usize>>,
    membership_version: u64,
    segment: u64,
    parked_idle: bool,
    accum_scratch: Vec<f64>,
    drift_scratch: Vec<f32>,
}

impl CspaMixer {
    pub fn new(
        format: Arc<Format>,
        targets: &[CaptureTarget],
        config: MixerConfig,
        max_frames_per_block: usize,
    ) -> Result<Self, PlanError> {
        let default_cap = targets.len().max(16);
        Self::new_with_max_targets(format, targets, config, max_frames_per_block, default_cap)
    }

    pub fn new_with_max_targets(
        format: Arc<Format>,
        targets: &[CaptureTarget],
        config: MixerConfig,
        max_frames_per_block: usize,
        max_targets: usize,
    ) -> Result<Self, PlanError> {
        if targets.is_empty() || max_frames_per_block == 0 || max_targets == 0 {
            return Err(PlanError::InvalidTarget);
        }
        if targets.len() > max_targets {
            return Err(PlanError::Capacity(CapacityError::ExceedsByteCeiling));
        }
        if format.sample() != SampleType::F32 {
            return Err(PlanError::UnsupportedCapturePlan);
        }
        config.validate()?;
        let ch = usize::from(format.channels());
        let sample_cap = max_frames_per_block * ch;
        let make_recycle = || {
            (0..config.max_input_slots + 2)
                .map(|_| Vec::with_capacity(sample_cap))
                .collect::<Vec<_>>()
        };

        let mut inputs = Vec::with_capacity(max_targets);
        for target in targets {
            target.validate()?;
            inputs.push(InputSlot {
                target_id: target.id.clone(),
                gain: target.gain,
                state: ParticipationState::Starting,
                queue: VecDeque::with_capacity(config.max_input_slots),
                recycle_buffers: make_recycle(),
                dropped_frames: 0,
                clipped_samples: 0,
                nonfinite_samples: 0,
                drift_slips: 0,
                sustained_drift_since_ns: None,
                drift_armed: false,
            });
        }
        let empty_id: Arc<str> = Arc::from("");
        let spare_count = max_targets.saturating_sub(targets.len());
        let mut spare_slots = Vec::with_capacity(max_targets);
        for _ in 0..spare_count {
            spare_slots.push(InputSlot {
                target_id: empty_id.clone(),
                gain: 1.0,
                state: ParticipationState::Starting,
                queue: VecDeque::with_capacity(config.max_input_slots),
                recycle_buffers: make_recycle(),
                dropped_frames: 0,
                clipped_samples: 0,
                nonfinite_samples: 0,
                drift_slips: 0,
                sustained_drift_since_ns: None,
                drift_armed: false,
            });
        }
        Ok(Self {
            format,
            config,
            max_targets,
            max_frames_per_block,
            inputs,
            spare_slots,
            next_slots_scratch: Vec::with_capacity(max_targets),
            slot_taken_scratch: vec![false; max_targets],
            mapping_scratch: Vec::with_capacity(max_targets),
            membership_version: 1,
            segment: 1,
            parked_idle: false,
            accum_scratch: vec![0.0_f64; sample_cap],
            drift_scratch: vec![0.0_f32; sample_cap],
        })
    }

    pub fn format(&self) -> &Arc<Format> {
        &self.format
    }

    pub fn config(&self) -> &MixerConfig {
        &self.config
    }

    pub fn segment(&self) -> u64 {
        self.segment
    }

    pub fn membership_version(&self) -> u64 {
        self.membership_version
    }

    /// Reconfigure active mixer targets in place.
    /// Matching existing targets retain their queued packets, participation state, and drift history;
    /// newly added targets start in `ParticipationState::Starting`.
    /// Returns a slice mapping each new target index to `Some(old_slot_index)` if retained or `None` if newly added.
    pub fn reconfigure_targets(
        &mut self,
        new_targets: &[CaptureTarget],
    ) -> Result<&[Option<usize>], PlanError> {
        if new_targets.is_empty() {
            return Err(PlanError::InvalidTarget);
        }
        if new_targets.len() > self.max_targets {
            return Err(PlanError::Capacity(CapacityError::ExceedsByteCeiling));
        }
        for target in new_targets {
            target.validate()?;
        }

        let old_len = self.inputs.len();
        for flag in &mut self.slot_taken_scratch[..old_len] {
            *flag = false;
        }
        self.mapping_scratch.clear();
        self.next_slots_scratch.clear();

        for target in new_targets {
            let mut matched_old = None;
            for (old_idx, slot) in self.inputs.iter().enumerate() {
                if !self.slot_taken_scratch[old_idx]
                    && slot.target_id == target.id
                    && !matches!(
                        slot.state,
                        ParticipationState::Exited | ParticipationState::Failed(_)
                    )
                {
                    self.slot_taken_scratch[old_idx] = true;
                    matched_old = Some(old_idx);
                    break;
                }
            }
            self.mapping_scratch.push(matched_old);
        }

        for &mapped in &self.mapping_scratch {
            if let Some(old_idx) = mapped {
                let placeholder = self.spare_slots.pop().unwrap_or_else(|| InputSlot {
                    target_id: self.inputs[old_idx].target_id.clone(),
                    gain: 1.0,
                    state: ParticipationState::Starting,
                    queue: VecDeque::with_capacity(self.config.max_input_slots),
                    recycle_buffers: Vec::new(),
                    dropped_frames: 0,
                    clipped_samples: 0,
                    nonfinite_samples: 0,
                    drift_slips: 0,
                    sustained_drift_since_ns: None,
                    drift_armed: false,
                });
                let retained = std::mem::replace(&mut self.inputs[old_idx], placeholder);
                self.next_slots_scratch.push(retained);
            } else {
                let mut fresh = self.spare_slots.pop().unwrap_or_else(|| InputSlot {
                    target_id: new_targets[0].id.clone(),
                    gain: 1.0,
                    state: ParticipationState::Starting,
                    queue: VecDeque::with_capacity(self.config.max_input_slots),
                    recycle_buffers: Vec::new(),
                    dropped_frames: 0,
                    clipped_samples: 0,
                    nonfinite_samples: 0,
                    drift_slips: 0,
                    sustained_drift_since_ns: None,
                    drift_armed: false,
                });
                while let Some(mut old_pkt) = fresh.queue.pop_front() {
                    old_pkt.samples.clear();
                    fresh.recycle_buffers.push(old_pkt.samples);
                }
                fresh.state = ParticipationState::Starting;
                fresh.dropped_frames = 0;
                fresh.clipped_samples = 0;
                fresh.nonfinite_samples = 0;
                fresh.drift_slips = 0;
                fresh.sustained_drift_since_ns = None;
                fresh.drift_armed = false;
                self.next_slots_scratch.push(fresh);
            }
        }

        for (slot, target) in self.next_slots_scratch.iter_mut().zip(new_targets.iter()) {
            slot.target_id = target.id.clone();
            slot.gain = target.gain;
        }

        while let Some(mut old_slot) = self.inputs.pop() {
            while let Some(mut old_pkt) = old_slot.queue.pop_front() {
                old_pkt.samples.clear();
                old_slot.recycle_buffers.push(old_pkt.samples);
            }
            self.spare_slots.push(old_slot);
        }

        std::mem::swap(&mut self.inputs, &mut self.next_slots_scratch);
        self.membership_version = self.membership_version.saturating_add(1);
        self.segment = self.segment.saturating_add(1);
        Ok(&self.mapping_scratch)
    }

    pub fn membership_descriptor(&self) -> MembershipDescriptor {
        let entries: Vec<MembershipEntry> = self
            .inputs
            .iter()
            .enumerate()
            .map(|(idx, slot)| MembershipEntry {
                input_index: idx,
                target_id: slot.target_id.clone(),
                state: slot.state,
            })
            .collect();
        MembershipDescriptor {
            version: self.membership_version,
            entries: Arc::from(entries),
        }
    }

    pub fn input_diagnostics(&self) -> Vec<InputDiagnosticSnapshot> {
        let mut out = Vec::with_capacity(self.inputs.len());
        self.write_input_diagnostics(&mut out);
        out
    }

    pub fn write_input_diagnostics(&self, out: &mut Vec<InputDiagnosticSnapshot>) {
        if out.len() == self.inputs.len() {
            for (idx, (dst, slot)) in out.iter_mut().zip(self.inputs.iter()).enumerate() {
                dst.input_index = idx;
                if !Arc::ptr_eq(&dst.target_id, &slot.target_id) {
                    dst.target_id = slot.target_id.clone();
                }
                dst.participation_state = slot.state;
                dst.queued_packets = slot.queue.len();
                dst.dropped_frames = slot.dropped_frames;
                dst.clipped_samples = slot.clipped_samples;
                dst.nonfinite_samples = slot.nonfinite_samples;
                dst.drift_slips = slot.drift_slips;
            }
        } else {
            out.clear();
            for (idx, slot) in self.inputs.iter().enumerate() {
                out.push(InputDiagnosticSnapshot {
                    input_index: idx,
                    target_id: slot.target_id.clone(),
                    participation_state: slot.state,
                    queued_packets: slot.queue.len(),
                    dropped_frames: slot.dropped_frames,
                    clipped_samples: slot.clipped_samples,
                    nonfinite_samples: slot.nonfinite_samples,
                    drift_slips: slot.drift_slips,
                });
            }
        }
    }

    pub fn set_input_state(
        &mut self,
        index: usize,
        state: ParticipationState,
    ) -> Result<(), Reason> {
        let slot = self.inputs.get_mut(index).ok_or(Reason::InvalidFormat)?;
        if slot.state != state {
            slot.state = state;
            self.membership_version = self.membership_version.saturating_add(1);
            self.segment = self.segment.saturating_add(1);
        }
        Ok(())
    }

    pub fn queued_packets(&self, index: usize) -> usize {
        self.inputs.get(index).map_or(0, |s| s.queue.len())
    }

    /// Push samples using the slot's preallocated recycle pool (`0` heap allocations in steady state).
    pub fn push_input_samples(
        &mut self,
        index: usize,
        samples: &[f32],
        frames: usize,
        timestamp: Option<Stamp>,
        arrival_ns: u64,
    ) -> Result<(), Reason> {
        let ch = usize::from(self.format.channels());
        if frames == 0 || frames > self.max_frames_per_block || samples.len() != frames * ch {
            return Err(Reason::IncompleteFrame);
        }
        let max_slots = self.config.max_input_slots;
        let slot = self.inputs.get_mut(index).ok_or(Reason::InvalidFormat)?;
        if matches!(
            slot.state,
            ParticipationState::Exited | ParticipationState::Failed(_)
        ) {
            return Err(Reason::Unavailable);
        }
        if slot.queue.len() >= max_slots
            && let Some(mut evicted) = slot.queue.pop_front()
        {
            slot.dropped_frames = slot.dropped_frames.saturating_add(evicted.frames as u64);
            if evicted.samples.capacity() >= self.max_frames_per_block * ch {
                evicted.samples.clear();
                slot.recycle_buffers.push(evicted.samples);
            }
        }
        let mut buf = slot
            .recycle_buffers
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(self.max_frames_per_block * ch));
        buf.clear();
        buf.extend_from_slice(samples);
        slot.queue.push_back(MixerInputPacket {
            samples: buf,
            frames,
            timestamp,
            arrival_ns,
        });
        if slot.state != ParticipationState::Active {
            slot.state = ParticipationState::Active;
            self.membership_version = self.membership_version.saturating_add(1);
        }
        Ok(())
    }

    pub fn push_input_packet(
        &mut self,
        index: usize,
        packet: MixerInputPacket,
    ) -> Result<(), Reason> {
        let ch = usize::from(self.format.channels());
        if packet.frames == 0 || packet.samples.len() != packet.frames * ch {
            return Err(Reason::IncompleteFrame);
        }
        let max_slots = self.config.max_input_slots;
        let slot = self.inputs.get_mut(index).ok_or(Reason::InvalidFormat)?;
        if matches!(
            slot.state,
            ParticipationState::Exited | ParticipationState::Failed(_)
        ) {
            return Err(Reason::Unavailable);
        }
        if slot.queue.len() >= max_slots
            && let Some(mut evicted) = slot.queue.pop_front()
        {
            slot.dropped_frames = slot.dropped_frames.saturating_add(evicted.frames as u64);
            if evicted.samples.capacity() >= self.max_frames_per_block * ch
                && slot.recycle_buffers.len() < max_slots + 2
            {
                evicted.samples.clear();
                slot.recycle_buffers.push(evicted.samples);
            }
        }
        slot.queue.push_back(packet);
        if slot.state != ParticipationState::Active {
            slot.state = ParticipationState::Active;
            self.membership_version = self.membership_version.saturating_add(1);
        }
        Ok(())
    }

    /// Apply a 64-frame (or configured `drift_crossfade_frames`) linear crossfade slip/repeat correction
    /// between two buffers on input `index` when sustained skew exceeds `drift_threshold_ns`.
    pub fn apply_drift_correction(
        &mut self,
        index: usize,
        observed_skew_ns: u64,
        now_ns: u64,
        primary: &[f32],
        shifted: &[f32],
        out: &mut [f32],
    ) -> Result<bool, Reason> {
        let ch = usize::from(self.format.channels());
        if primary.len() != shifted.len()
            || primary.len() != out.len()
            || !primary.len().is_multiple_of(ch)
        {
            return Err(Reason::IncompleteFrame);
        }
        let threshold = self.config.drift_threshold_ns;
        let clear_threshold = threshold.saturating_sub(self.config.drift_hysteresis_ns);
        let hold_ns = self.config.drift_hold_ns;
        let ramp_frames = self.config.drift_crossfade_frames;
        let slot = self.inputs.get_mut(index).ok_or(Reason::InvalidFormat)?;

        if observed_skew_ns >= threshold {
            let since = *slot.sustained_drift_since_ns.get_or_insert(now_ns);
            if now_ns.saturating_sub(since) >= hold_ns {
                slot.drift_armed = true;
            }
        } else if observed_skew_ns <= clear_threshold {
            slot.sustained_drift_since_ns = None;
            slot.drift_armed = false;
        }

        if !slot.drift_armed {
            out.copy_from_slice(primary);
            return Ok(false);
        }

        let frames = primary.len() / ch;
        let actual_ramp = ramp_frames.min(frames);
        for frame in 0..frames {
            let weight = if actual_ramp == 0 || frame >= actual_ramp {
                1.0_f64
            } else {
                (frame as f64) / (actual_ramp as f64)
            };
            for c in 0..ch {
                let idx = frame * ch + c;
                let a = if primary[idx].is_finite() {
                    f64::from(primary[idx])
                } else {
                    0.0
                };
                let b = if shifted[idx].is_finite() {
                    f64::from(shifted[idx])
                } else {
                    0.0
                };
                out[idx] = ((1.0 - weight) * a + weight * b) as f32;
            }
        }
        slot.drift_slips = slot.drift_slips.saturating_add(1);
        slot.sustained_drift_since_ns = Some(now_ns);
        slot.drift_armed = false;
        Ok(true)
    }

    /// Mix one block of `frames` into `out_samples` (`frames * channels` length) without steady-state allocation.
    pub fn mix_block_into(
        &mut self,
        frames: usize,
        now_ns: u64,
        oldest_wait_ns: u64,
        out_samples: &mut [f32],
    ) -> Result<(MixStepOutcome, MixedBlockMetadata), Reason> {
        let ch = usize::from(self.format.channels());
        let total_samples = frames.checked_mul(ch).ok_or(Reason::InvalidFormat)?;
        if frames == 0 || out_samples.len() < total_samples {
            return Err(Reason::IncompleteFrame);
        }

        let mut any_failed_code: Option<u32> = None;
        let mut all_terminal = true;
        let mut all_failed = true;
        let mut failed_mask = 0u64;

        for (idx, slot) in self.inputs.iter().enumerate() {
            match slot.state {
                ParticipationState::Failed(code) => {
                    failed_mask |= 1u64 << idx;
                    if any_failed_code.is_none() {
                        any_failed_code = Some(code);
                    }
                }
                ParticipationState::Exited => {
                    all_failed = false;
                }
                _ => {
                    all_terminal = false;
                    all_failed = false;
                }
            }
        }

        if let Some(code) = any_failed_code
            && self.config.on_input_failure == OnInputFailure::Fail
        {
            return Ok((
                MixStepOutcome::Failed(code),
                MixedBlockMetadata {
                    membership_version: self.membership_version,
                    failed_inputs: failed_mask,
                    ..Default::default()
                },
            ));
        }

        let any_queued = self.inputs.iter().any(|s| !s.queue.is_empty());
        if !any_queued && all_terminal {
            if all_failed && let Some(code) = any_failed_code {
                return Ok((
                    MixStepOutcome::Failed(code),
                    MixedBlockMetadata {
                        membership_version: self.membership_version,
                        failed_inputs: failed_mask,
                        ..Default::default()
                    },
                ));
            }
            return Ok((MixStepOutcome::Eof, MixedBlockMetadata::default()));
        }

        if !any_queued {
            if !self.config.silence_on_idle {
                self.parked_idle = true;
                return Ok((MixStepOutcome::ParkedIdle, MixedBlockMetadata::default()));
            }
            out_samples[..total_samples].fill(0.0);
            let meta = MixedBlockMetadata {
                membership_version: self.membership_version,
                failed_inputs: failed_mask,
                synthesized_silence: true,
                ..Default::default()
            };
            return Ok((MixStepOutcome::Produced(frames), meta));
        }

        if self.parked_idle {
            self.parked_idle = false;
            self.segment = self.segment.saturating_add(1);
        }

        if self.accum_scratch.len() < total_samples {
            self.accum_scratch.resize(total_samples, 0.0);
        }
        self.accum_scratch[..total_samples].fill(0.0);

        let mut contributing_mask = 0u64;
        let mut late_inputs = 0u64;
        let mut nonfinite_inputs = 0u64;
        let mut nonfinite_samples = 0u64;
        let mut drift_slips = 0u64;

        let mut min_stamp_ns = u64::MAX;
        let mut max_stamp_ns = 0u64;
        let mut max_uncertainty_ns = 0u64;
        let mut common_clock_domain: Option<u64> = None;
        let mut all_stamps_calibrated = true;
        let mut active_contributors = 0usize;
        let rate_hz = u64::from(self.format.rate().max(1));
        let max_recycle = self.config.max_input_slots + 2;
        let min_active_q = self
            .inputs
            .iter()
            .filter(|s| !s.queue.is_empty())
            .map(|s| s.queue.len())
            .min()
            .unwrap_or(1)
            .max(1);

        for (idx, slot) in self.inputs.iter_mut().enumerate() {
            let Some(mut pkt) = slot.queue.pop_front() else {
                if matches!(
                    slot.state,
                    ParticipationState::Active | ParticipationState::Late
                ) && oldest_wait_ns >= self.config.alignment_window_ns
                {
                    late_inputs |= 1u64 << idx;
                    if slot.state != ParticipationState::Late {
                        slot.state = ParticipationState::Late;
                        self.membership_version = self.membership_version.saturating_add(1);
                    }
                }
                continue;
            };

            if pkt.frames != frames {
                return Err(Reason::IncompleteFrame);
            }
            contributing_mask |= 1u64 << idx;
            active_contributors += 1;
            if slot.state == ParticipationState::Late {
                slot.state = ParticipationState::Active;
                self.membership_version = self.membership_version.saturating_add(1);
            }

            // Automatic queue-excess slip correction when sustained lead >= `drift_threshold_ns`
            let excess_packets = (slot.queue.len() + 1).saturating_sub(min_active_q);
            let lead_ns =
                ((excess_packets * frames) as u64).saturating_mul(1_000_000_000) / rate_hz;
            let mut used_drift_scratch = false;
            if lead_ns >= self.config.drift_threshold_ns && !slot.queue.is_empty() {
                let since = *slot.sustained_drift_since_ns.get_or_insert(now_ns);
                if now_ns.saturating_sub(since) >= self.config.drift_hold_ns
                    && slot
                        .queue
                        .front()
                        .is_some_and(|next_pkt| next_pkt.frames == frames)
                    && let Some(mut pkt_next) = slot.queue.pop_front()
                {
                    let actual_ramp = self.config.drift_crossfade_frames.min(frames);
                    for frame in 0..frames {
                        let weight = if actual_ramp == 0 || frame >= actual_ramp {
                            1.0_f64
                        } else {
                            (frame as f64) / (actual_ramp as f64)
                        };
                        for c in 0..ch {
                            let s_i = frame * ch + c;
                            let a = if pkt.samples[s_i].is_finite() {
                                f64::from(pkt.samples[s_i])
                            } else {
                                0.0
                            };
                            let b = if pkt_next.samples[s_i].is_finite() {
                                f64::from(pkt_next.samples[s_i])
                            } else {
                                0.0
                            };
                            self.drift_scratch[s_i] = ((1.0 - weight) * a + weight * b) as f32;
                        }
                    }
                    pkt.timestamp = pkt_next.timestamp;
                    if pkt_next.samples.capacity() >= self.max_frames_per_block * ch
                        && slot.recycle_buffers.len() < max_recycle
                    {
                        pkt_next.samples.clear();
                        slot.recycle_buffers.push(pkt_next.samples);
                    }
                    slot.drift_slips = slot.drift_slips.saturating_add(1);
                    slot.sustained_drift_since_ns = Some(now_ns);
                    slot.drift_armed = false;
                    used_drift_scratch = true;
                }
            } else if lead_ns
                <= self
                    .config
                    .drift_threshold_ns
                    .saturating_sub(self.config.drift_hysteresis_ns)
            {
                slot.sustained_drift_since_ns = None;
                slot.drift_armed = false;
            }

            if let Some(stamp) = pkt.timestamp
                && stamp.provenance == Provenance::Native
                && stamp.ticks_per_second > 0
                && let Some(uncertainty) = stamp.uncertainty_ns
            {
                match common_clock_domain {
                    None => common_clock_domain = Some(stamp.clock_domain),
                    Some(dom) if dom != stamp.clock_domain => {
                        all_stamps_calibrated = false;
                    }
                    Some(_) => {}
                }
                if all_stamps_calibrated {
                    let stamp_ns = (u128::from(stamp.ticks) * 1_000_000_000
                        / u128::from(stamp.ticks_per_second))
                        as u64;
                    min_stamp_ns = min_stamp_ns.min(stamp_ns);
                    max_stamp_ns = max_stamp_ns.max(stamp_ns);
                    max_uncertainty_ns = max_uncertainty_ns.max(uncertainty);
                }
            } else {
                all_stamps_calibrated = false;
            }

            let gain = f64::from(slot.gain);
            let src_slice = if used_drift_scratch {
                &self.drift_scratch[..total_samples]
            } else {
                &pkt.samples[..total_samples]
            };
            for (s_idx, &raw) in src_slice.iter().enumerate() {
                let clean = if raw.is_finite() {
                    f64::from(raw)
                } else {
                    nonfinite_samples += 1;
                    slot.nonfinite_samples = slot.nonfinite_samples.saturating_add(1);
                    nonfinite_inputs |= 1u64 << idx;
                    0.0
                };
                self.accum_scratch[s_idx] += clean * gain;
            }
            if pkt.samples.capacity() >= self.max_frames_per_block * ch
                && slot.recycle_buffers.len() < max_recycle
            {
                pkt.samples.clear();
                slot.recycle_buffers.push(pkt.samples);
            }
            drift_slips = drift_slips.saturating_add(slot.drift_slips);
        }

        let mut clipped_samples = 0u64;
        for (idx, &val) in self.accum_scratch[..total_samples].iter().enumerate() {
            let as_f32 = val as f32;
            if as_f32.abs() > 1.0 {
                clipped_samples += 1;
            }
            out_samples[idx] = if self.config.clamp_f32 {
                as_f32.clamp(-1.0, 1.0)
            } else {
                as_f32
            };
        }

        let (skew_estimate_ns, skew_uncertainty_ns) =
            if active_contributors >= 2 && all_stamps_calibrated && min_stamp_ns <= max_stamp_ns {
                (Some(max_stamp_ns - min_stamp_ns), Some(max_uncertainty_ns))
            } else {
                (None, None)
            };

        let meta = MixedBlockMetadata {
            membership_version: self.membership_version,
            contributing_mask,
            late_inputs,
            failed_inputs: failed_mask,
            nonfinite_inputs,
            synthesized_silence: false,
            clipped_samples,
            nonfinite_samples,
            drift_slips,
            skew_estimate_ns,
            skew_uncertainty_ns,
        };

        Ok((MixStepOutcome::Produced(frames), meta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ByteOrder, Layout, OnRootExit, ProcessIdentity, ProcessScope};

    fn test_format() -> Arc<Format> {
        Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48000,
                2,
                Layout::Unknown,
            )
            .unwrap(),
        )
    }

    fn target(pid: u32, gain: f32) -> CaptureTarget {
        CaptureTarget::process(
            ProcessIdentity {
                pid,
                generation: u64::from(pid) * 10,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_gain(gain)
        .unwrap()
    }

    #[test]
    fn cspa_mixer_accumulates_sanitizes_duplicates_and_runs_zero_steady_allocations() {
        let fmt = test_format();
        let t1 = target(10, 1.0);
        let t2 = target(20, 0.5);
        // Duplicate t1 included intentionally to test caller-owned overlap summation
        let mut mixer =
            CspaMixer::new(fmt, &[t1.clone(), t1, t2], MixerConfig::default(), 4).unwrap();

        let p1 = MixerInputPacket {
            samples: vec![0.25, -0.25, 0.5, -0.5, f32::NAN, 0.25, 0.6, -0.6],
            frames: 4,
            timestamp: Some(Stamp {
                ticks: 1_000_000,
                ticks_per_second: 10_000_000,
                clock_domain: 1,
                provenance: Provenance::Native,
                uncertainty_ns: Some(200_000),
            }),
            arrival_ns: 100_000_000,
        };
        let p2 = MixerInputPacket {
            samples: vec![0.25, -0.25, 0.5, -0.5, 0.25, 0.25, 0.6, -0.6],
            frames: 4,
            timestamp: Some(Stamp {
                ticks: 1_020_000,
                ticks_per_second: 10_000_000,
                clock_domain: 1,
                provenance: Provenance::Native,
                uncertainty_ns: Some(300_000),
            }),
            arrival_ns: 102_000_000,
        };
        let p3 = MixerInputPacket {
            samples: vec![0.5, 0.5, -0.5, 0.5, f32::INFINITY, -0.5, 0.2, -0.2],
            frames: 4,
            timestamp: Some(Stamp {
                ticks: 1_030_000,
                ticks_per_second: 10_000_000,
                clock_domain: 1,
                provenance: Provenance::Native,
                uncertainty_ns: Some(250_000),
            }),
            arrival_ns: 103_000_000,
        };

        mixer.push_input_packet(0, p1).unwrap();
        mixer.push_input_packet(1, p2).unwrap();
        mixer.push_input_packet(2, p3).unwrap();

        let mut out = vec![0.0_f32; 8];
        let (outcome, meta) = mixer.mix_block_into(4, 105_000_000, 0, &mut out).unwrap();
        assert_eq!(outcome, MixStepOutcome::Produced(4));
        assert_eq!(meta.contributing_mask, 0b111);
        // Sample 0: 0.25 + 0.25 + 0.5 * 0.5 = 0.75
        assert!((out[0] - 0.75).abs() <= 1e-6);
        // Sample 4 had NaN on input 0 and +Inf on input 2 -> only input 1 (0.25) contributes
        assert!((out[4] - 0.25).abs() <= 1e-6);
        assert_eq!(meta.nonfinite_samples, 2);
        assert_eq!(meta.nonfinite_inputs, 0b101);
        // Sample 6: 0.6 + 0.6 + 0.2*0.5 = 1.3 (> 1.0, unclamped f32 default, counted as clipped)
        assert!((out[6] - 1.3).abs() <= 1e-6);
        assert_eq!(meta.clipped_samples, 2);
        // Calibrated skew: 103ms - 100ms = 3ms (3_000_000 ns), uncertainty = 300_000 ns
        assert_eq!(meta.skew_estimate_ns, Some(3_000_000));
        assert_eq!(meta.skew_uncertainty_ns, Some(300_000));

        let clean_samples = [0.1_f32; 8];
        for i in 0..3 {
            mixer
                .push_input_packet(
                    i,
                    MixerInputPacket {
                        samples: clean_samples.to_vec(),
                        frames: 4,
                        timestamp: None,
                        arrival_ns: 0,
                    },
                )
                .unwrap();
        }
        let measured = allocation_counter::measure(|| {
            let (res, m) = mixer.mix_block_into(4, 200_000_000, 0, &mut out).unwrap();
            assert_eq!(res, MixStepOutcome::Produced(4));
            assert_eq!(m.skew_estimate_ns, None);
        });
        assert_eq!(measured.count_total, 0);

        let t1_keep = target(10, 1.0);
        let t3_new = target(30, 0.5);
        mixer
            .push_input_packet(
                0,
                MixerInputPacket {
                    samples: vec![0.4_f32; 8],
                    frames: 4,
                    timestamp: None,
                    arrival_ns: 0,
                },
            )
            .unwrap();
        let new_targets = [t1_keep, t3_new];
        let reconfig_allocs = allocation_counter::measure(|| {
            let mapping = mixer.reconfigure_targets(&new_targets).unwrap();
            assert_eq!(mapping, &[Some(0), None]);
        });
        assert_eq!(reconfig_allocs.count_total, 0);
        let (swap_res, swap_meta) = mixer.mix_block_into(4, 210_000_000, 0, &mut out).unwrap();
        assert_eq!(swap_res, MixStepOutcome::Produced(4));
        assert_eq!(swap_meta.contributing_mask, 0b01);
        assert!((out[0] - 0.4).abs() <= 1e-6);
    }

    #[test]
    fn cspa_mixer_handles_drift_hysteresis_idle_silence_and_input_failure_modes() {
        let fmt = test_format();
        let t1 = target(10, 1.0);
        let t2 = target(20, 1.0);
        let mut config = MixerConfig {
            silence_on_idle: false,
            ..MixerConfig::default()
        };
        let mut mixer = CspaMixer::new(fmt.clone(), &[t1.clone(), t2.clone()], config, 64).unwrap();

        let mut out = vec![0.0_f32; 128];
        let (parked, _) = mixer.mix_block_into(64, 10_000_000, 0, &mut out).unwrap();
        assert_eq!(parked, MixStepOutcome::ParkedIdle);

        config.silence_on_idle = true;
        let mut idle_mixer =
            CspaMixer::new(fmt.clone(), &[t1.clone(), t2.clone()], config, 64).unwrap();
        let (synth, synth_meta) = idle_mixer
            .mix_block_into(64, 10_000_000, 0, &mut out)
            .unwrap();
        assert_eq!(synth, MixStepOutcome::Produced(64));
        assert!(synth_meta.synthesized_silence);

        let prim = vec![0.25_f32; 128];
        let shft = vec![0.75_f32; 128];
        // Skew above the threshold starts the hold timer without an immediate slip.
        let slipped0 = mixer
            .apply_drift_correction(0, 6_000_000, 100_000_000, &prim, &shft, &mut out)
            .unwrap();
        assert!(!slipped0);
        // Sustained at t=600ms (>= 500ms hold) triggers 64-frame crossfade slip
        let slipped1 = mixer
            .apply_drift_correction(0, 6_000_000, 600_000_000, &prim, &shft, &mut out)
            .unwrap();
        assert!(slipped1);
        assert!((out[0] - 0.25).abs() <= 1e-6);
        assert!((out[126] - (0.25 * (1.0 / 64.0) + 0.75 * (63.0 / 64.0))).abs() <= 1e-5);

        mixer
            .set_input_state(0, ParticipationState::Failed(42))
            .unwrap();
        mixer
            .push_input_packet(
                1,
                MixerInputPacket {
                    samples: vec![0.5_f32; 128],
                    frames: 64,
                    timestamp: None,
                    arrival_ns: 0,
                },
            )
            .unwrap();
        let (deg_outcome, deg_meta) = mixer.mix_block_into(64, 700_000_000, 0, &mut out).unwrap();
        assert_eq!(deg_outcome, MixStepOutcome::Produced(64));
        assert_eq!(deg_meta.failed_inputs, 0b01);
        assert_eq!(deg_meta.contributing_mask, 0b10);

        let fail_cfg = MixerConfig {
            on_input_failure: OnInputFailure::Fail,
            ..MixerConfig::default()
        };
        let mut fail_mixer = CspaMixer::new(fmt, &[t1, t2], fail_cfg, 64).unwrap();
        fail_mixer
            .set_input_state(0, ParticipationState::Failed(99))
            .unwrap();
        let (fail_outcome, _) = fail_mixer
            .mix_block_into(64, 700_000_000, 0, &mut out)
            .unwrap();
        assert_eq!(fail_outcome, MixStepOutcome::Failed(99));
    }

    #[test]
    fn cspa_mixer_gates_cross_clock_domain_skew_and_auto_slips_sustained_queue_lead() {
        let fmt = test_format();
        let t_sys = CaptureTarget::default_output(crate::schema::DefaultDeviceMode::Follow);
        let t_mic = CaptureTarget::microphone(crate::schema::DefaultDeviceMode::Follow);
        let mut mixer = CspaMixer::new(fmt, &[t_sys, t_mic], MixerConfig::default(), 480).unwrap();

        let blk = vec![0.25_f32; 960];
        // Distinct clock domains (domain 1 vs domain 2) must report skew_estimate_ns == None
        mixer
            .push_input_samples(
                0,
                &blk,
                480,
                Some(Stamp {
                    ticks: 48_000,
                    ticks_per_second: 48_000,
                    clock_domain: 1,
                    provenance: Provenance::Native,
                    uncertainty_ns: Some(200_000),
                }),
                0,
            )
            .unwrap();
        mixer
            .push_input_samples(
                1,
                &blk,
                480,
                Some(Stamp {
                    ticks: 960_000,
                    ticks_per_second: 48_000,
                    clock_domain: 2,
                    provenance: Provenance::Native,
                    uncertainty_ns: Some(200_000),
                }),
                0,
            )
            .unwrap();

        let mut out = vec![0.0_f32; 960];
        let (res, meta) = mixer.mix_block_into(480, 10_000_000, 0, &mut out).unwrap();
        assert_eq!(res, MixStepOutcome::Produced(480));
        assert_eq!(meta.skew_estimate_ns, None);
        assert_eq!(meta.skew_uncertainty_ns, None);

        // Sustained 1-packet excess lead (10ms >= 5ms threshold) on input 1 over >= 500ms hold triggers auto-slip
        mixer
            .push_input_samples(1, &blk, 480, None, 20_000_000)
            .unwrap(); // extra lead packet on mic
        for step in 0..60u64 {
            let now_ns = 20_000_000 + step * 10_000_000;
            let allocs = allocation_counter::measure(|| {
                mixer
                    .push_input_samples(0, &blk, 480, None, now_ns)
                    .unwrap();
                mixer
                    .push_input_samples(1, &blk, 480, None, now_ns)
                    .unwrap();
                let (r, _) = mixer.mix_block_into(480, now_ns, 0, &mut out).unwrap();
                assert_eq!(r, MixStepOutcome::Produced(480));
            });
            assert_eq!(allocs.count_total, 0);
        }
        assert_eq!(mixer.input_diagnostics()[1].drift_slips, 1);
        assert_eq!(mixer.queued_packets(1), 0);
    }
}
