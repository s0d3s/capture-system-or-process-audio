#![forbid(unsafe_code)]

pub mod converter;
pub mod discovery;
pub mod lifecycle;
pub mod manager;
pub mod measurements;
pub mod mixer;
pub mod ordering;
pub mod planner;
pub mod reads;
pub mod routing;
pub mod schema;
pub mod storage;
pub mod stream_runtime;
pub mod traits;

use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapacityError {
    Zero,
    Overflow,
    ExceedsByteCeiling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageLimits {
    slots: NonZeroUsize,
    bytes_per_slot: NonZeroUsize,
    total_bytes: usize,
}

impl StorageLimits {
    pub fn new(
        slots: usize,
        frames_per_slot: usize,
        bytes_per_frame: usize,
        byte_ceiling: usize,
    ) -> Result<Self, CapacityError> {
        let slots = NonZeroUsize::new(slots).ok_or(CapacityError::Zero)?;
        if frames_per_slot == 0 || bytes_per_frame == 0 || byte_ceiling == 0 {
            return Err(CapacityError::Zero);
        }
        let bytes_per_slot = frames_per_slot
            .checked_mul(bytes_per_frame)
            .and_then(NonZeroUsize::new)
            .ok_or(CapacityError::Overflow)?;
        let total_bytes = slots
            .get()
            .checked_mul(bytes_per_slot.get())
            .ok_or(CapacityError::Overflow)?;
        if total_bytes > byte_ceiling {
            return Err(CapacityError::ExceedsByteCeiling);
        }
        Ok(Self {
            slots,
            bytes_per_slot,
            total_bytes,
        })
    }

    pub fn slots(self) -> usize {
        self.slots.get()
    }

    pub fn bytes_per_slot(self) -> usize {
        self.bytes_per_slot.get()
    }

    pub fn total_bytes(self) -> usize {
        self.total_bytes
    }
}
