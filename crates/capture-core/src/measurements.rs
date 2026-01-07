use std::{
    sync::atomic::{
        AtomicBool, AtomicU32, AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
pub enum Stage {
    Acquired = 1,
    PublishBegin = 2,
    PublishEnd = 3,
    Dequeued = 4,
    ReadSatisfied = 5,
    ReadReturned = 6,
    DequeueBegin = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub stage: Stage,
    pub sequence: u64,
    pub elapsed_ns: u64,
}

struct Slot {
    meta: AtomicU32,
    elapsed_low: AtomicU32,
}

pub struct Trace {
    origin: Instant,
    slots: Box<[Slot]>,
    next: AtomicUsize,
    stride: AtomicUsize,
    overflowed: AtomicBool,
}

impl Trace {
    pub const COMPACT_CAPACITY: usize = 1280;

    pub fn new(capacity: usize) -> Option<Self> {
        Self::with_stride(capacity, 1)
    }

    pub fn with_stride(capacity: usize, stride: usize) -> Option<Self> {
        if capacity == 0 || capacity > 65536 || stride == 0 {
            return None;
        }
        Some(Self {
            origin: Instant::now(),
            slots: (0..capacity)
                .map(|_| Slot {
                    meta: AtomicU32::new(0),
                    elapsed_low: AtomicU32::new(0),
                })
                .collect(),
            next: AtomicUsize::new(0),
            stride: AtomicUsize::new(stride),
            overflowed: AtomicBool::new(false),
        })
    }

    pub fn new_compact() -> Self {
        Self::with_stride(Self::COMPACT_CAPACITY, 4).expect("valid compact trace capacity")
    }

    pub fn record(&self, stage: Stage, sequence: u64) {
        self.record_at(stage, sequence, Instant::now());
    }

    pub fn record_at(&self, stage: Stage, sequence: u64, instant: Instant) {
        let stride = self.stride.load(Relaxed);
        if stride > 1 && !(sequence as usize).is_multiple_of(stride) {
            return;
        }
        let Some(elapsed) = instant
            .checked_duration_since(self.origin)
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        else {
            self.overflowed.store(true, Relaxed);
            return;
        };
        if sequence >= (1 << 24) || (elapsed >> 32) >= 16 {
            self.overflowed.store(true, Relaxed);
            return;
        }
        if self.next.load(Relaxed) >= self.slots.len() {
            self.overflowed.store(true, Relaxed);
            return;
        }
        let index = self.next.fetch_add(1, Relaxed);
        let Some(slot) = self.slots.get(index) else {
            self.overflowed.store(true, Relaxed);
            return;
        };
        slot.elapsed_low.store(elapsed as u32, Relaxed);
        let packed = (1u32 << 31)
            | ((stage as u32 & 0x7) << 28)
            | (((elapsed >> 32) as u32 & 0xf) << 24)
            | (sequence as u32 & 0x00ff_ffff);
        slot.meta.store(packed, Release);
    }

    pub fn overflowed(&self) -> bool {
        self.overflowed.load(Relaxed)
    }

    pub fn storage_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.slots.len() * std::mem::size_of::<Slot>()
    }

    pub fn snapshot(&self) -> Vec<Event> {
        self.slots
            .iter()
            .filter_map(|slot| {
                let meta = slot.meta.load(Acquire);
                if meta & (1 << 31) == 0 {
                    return None;
                }
                let stage = match (meta >> 28) & 0x7 {
                    1 => Stage::Acquired,
                    2 => Stage::PublishBegin,
                    3 => Stage::PublishEnd,
                    4 => Stage::Dequeued,
                    5 => Stage::ReadSatisfied,
                    6 => Stage::ReadReturned,
                    7 => Stage::DequeueBegin,
                    _ => unreachable!("published measurement stage"),
                };
                let high = u64::from((meta >> 24) & 0xf);
                let low = u64::from(slot.elapsed_low.load(Relaxed));
                Some(Event {
                    stage,
                    sequence: u64::from(meta & 0x00ff_ffff),
                    elapsed_ns: (high << 32) | low,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn bounded_trace_controls() {
        assert!(Trace::new(0).is_none());
        assert!(Trace::new(65537).is_none());
        let compact = Trace::new_compact();
        assert!(compact.storage_bytes() + 6024 <= 16384);
        let trace = Arc::new(Trace::new(400).unwrap());
        let measured = allocation_counter::measure(|| {
            for index in 0..100 {
                trace.record_at(
                    Stage::Acquired,
                    index,
                    trace.origin + Duration::from_millis(1),
                );
            }
        });
        assert_eq!(measured.count_total, 0);
        std::thread::scope(|scope| {
            for stage in [Stage::PublishBegin, Stage::PublishEnd, Stage::Dequeued] {
                let trace = &trace;
                scope.spawn(move || {
                    for index in 0..100 {
                        trace.record(stage, index);
                    }
                });
            }
        });
        assert!(!trace.overflowed());
        let events = trace.snapshot();
        assert_eq!(events.len(), 400);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.stage == Stage::Acquired && event.elapsed_ns == 1_000_000)
                .count(),
            100
        );
        trace.record(Stage::ReadReturned, 100);
        assert!(trace.overflowed());
        assert_eq!(trace.snapshot(), events);
    }
}
