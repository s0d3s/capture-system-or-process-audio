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

