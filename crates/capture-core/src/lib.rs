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

