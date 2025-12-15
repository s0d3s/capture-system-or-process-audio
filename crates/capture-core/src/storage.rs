use crate::{
    StorageLimits,
    manager::StorageReservation,
    schema::{Format, Header},
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageError {
    Exhausted,
    InvalidPacket,
    PacketTooLarge { rejected_frames: usize },
    GenerationBudget,
    GenerationExhausted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyStats {
    pub bytes: u64,
    pub destinations: u64,
    pub max_scan: usize,
}

impl CopyStats {
    pub fn copy(&mut self, destination: &mut [u8], source: &[u8]) {
        destination.copy_from_slice(source);
        self.bytes = self.bytes.saturating_add(source.len() as u64);
        self.destinations = self.destinations.saturating_add(1);
    }
}

#[derive(Debug)]
pub(crate) struct Block {
    pub(crate) bytes: Box<[u8]>,
    pub(crate) header: Header,
    pub(crate) format: Arc<Format>,
    pub(crate) _reservation: Option<StorageReservation>,
}

#[derive(Clone, Debug)]
pub struct OwnedBlock {
    pub(crate) owner: Arc<Block>,
}

impl OwnedBlock {
    pub fn bytes(&self) -> &[u8] {
        &self.owner.bytes[..self.owner.header.frames * self.owner.format.bytes_per_frame()]
    }
    pub fn header(&self) -> Header {
        self.owner.header
    }
    pub fn format(&self) -> &Format {
        &self.owner.format
    }
    pub fn descriptor(&self) -> Arc<Format> {
        self.owner.format.clone()
    }
    pub fn view(&self) -> Self {
        self.clone()
    }
}

pub struct Pool {
    pub(crate) blocks: Box<[Arc<Block>]>,
    max_frames: usize,
    limits: StorageLimits,
    generation: u64,
    format: Arc<Format>,
    descriptors: Box<[Option<Arc<Format>>]>,
    stats: CopyStats,
}

impl Pool {
    pub fn new(
        limits: StorageLimits,
        max_frames: usize,
        format: Arc<Format>,
    ) -> Result<Self, StorageError> {
        Self::new_impl(limits, max_frames, format, None)
    }

    pub fn new_reserved(
        limits: StorageLimits,
        max_frames: usize,
        format: Arc<Format>,
        reservation: StorageReservation,
    ) -> Result<Self, StorageError> {
        if reservation.pcm_bytes() < limits.total_bytes() {
            return Err(StorageError::GenerationBudget);
        }
        Self::new_impl(limits, max_frames, format, Some(reservation))
    }

    fn new_impl(
        limits: StorageLimits,
        max_frames: usize,
        format: Arc<Format>,
        reservation: Option<StorageReservation>,
    ) -> Result<Self, StorageError> {
        if max_frames == 0
            || format
                .byte_count(max_frames)
                .map_err(|_| StorageError::InvalidPacket)?
                > limits.bytes_per_slot()
        {
            return Err(StorageError::InvalidPacket);
        }
        let descriptor_slots = limits
            .slots()
            .checked_add(9)
            .ok_or(StorageError::GenerationBudget)?;
        let mut descriptors = vec![None; descriptor_slots].into_boxed_slice();
        descriptors[0] = Some(format.clone());
        let blocks = (0..limits.slots())
            .map(|_| {
                Arc::new(Block {
                    bytes: vec![0; limits.bytes_per_slot()].into_boxed_slice(),
                    header: Header {
                        sequence: 0,
                        segment: 1,
                        generation: 1,
                        frames: 0,
                        sample_position: None,
                        timestamp: None,
                    },
                    format: format.clone(),
                    _reservation: reservation.as_ref().map(StorageReservation::retain),
                })
            })
            .collect();
        Ok(Self {
            blocks,
            max_frames,
            limits,
            generation: 1,
            format,
            descriptors,
            stats: CopyStats::default(),
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn stats(&self) -> CopyStats {
        self.stats
    }
    pub fn limits(&self) -> StorageLimits {
        self.limits
    }
    pub fn format(&self) -> &Format {
        &self.format
    }
    pub fn descriptor(&self) -> Arc<Format> {
        self.format.clone()
    }
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }
    pub fn available(&self) -> usize {
        self.blocks
            .iter()
            .filter(|owner| Arc::strong_count(owner) == 1)
            .count()
    }

    pub fn offer(&mut self, bytes: &[u8], header: Header) -> Result<OwnedBlock, StorageError> {
        self.offer_impl(Some(bytes), header)
    }

    pub fn offer_silence(&mut self, header: Header) -> Result<OwnedBlock, StorageError> {
        self.offer_impl(None, header)
    }

    fn offer_impl(
        &mut self,
        bytes: Option<&[u8]>,
        header: Header,
    ) -> Result<OwnedBlock, StorageError> {
        if header.frames > self.max_frames {
            return Err(StorageError::PacketTooLarge {
                rejected_frames: header.frames,
            });
        }
        if header.frames == 0 || header.generation != self.generation {
            return Err(StorageError::InvalidPacket);
        }
        let byte_count = self
            .format
            .byte_count(header.frames)
            .map_err(|_| StorageError::InvalidPacket)?;
        if bytes.is_some_and(|bytes| bytes.len() != byte_count) {
            return Err(StorageError::InvalidPacket);
        }
        for (index, owner) in self.blocks.iter_mut().enumerate() {
            self.stats.max_scan = self.stats.max_scan.max(index + 1);
            if let Some(block) = Arc::get_mut(owner) {
                if block.header.generation != self.generation {
                    block.format = self.format.clone();
                }
                if let Some(bytes) = bytes {
                    self.stats.copy(&mut block.bytes[..byte_count], bytes);
                } else {
                    block.bytes[..byte_count].fill(0);
                    self.stats.bytes = self.stats.bytes.saturating_add(byte_count as u64);
                    self.stats.destinations = self.stats.destinations.saturating_add(1);
                }
                block.header = header;
                return Ok(OwnedBlock {
                    owner: owner.clone(),
                });
            }
        }
        Err(StorageError::Exhausted)
    }

    pub fn prepare_generation(
        &mut self,
        format: Arc<Format>,
        slots: usize,
    ) -> Result<u64, StorageError> {
        if slots == 0 || slots > self.available() {
            return Err(StorageError::GenerationBudget);
        }
        if format
            .byte_count(self.max_frames)
            .map_err(|_| StorageError::InvalidPacket)?
            > self.limits.bytes_per_slot()
        {
            return Err(StorageError::GenerationBudget);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(StorageError::GenerationExhausted)?;
        let descriptor = self
            .descriptors
            .iter()
            .position(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|existing| Arc::ptr_eq(existing, &format))
            })
            .or_else(|| {
                self.descriptors.iter().position(|entry| {
                    entry
                        .as_ref()
                        .is_none_or(|existing| Arc::strong_count(existing) == 1)
                })
            })
            .ok_or(StorageError::GenerationBudget)?;
        self.descriptors[descriptor] = Some(format.clone());
        let mut prepared = 0;
        for owner in &mut self.blocks {
            if let Some(block) = Arc::get_mut(owner) {
                block.format = format.clone();
                block.header.generation = generation;
                block.header.frames = 0;
                prepared += 1;
                if prepared == slots {
                    break;
                }
            }
        }
        self.generation = generation;
        self.format = format;
        Ok(generation)
    }
}
