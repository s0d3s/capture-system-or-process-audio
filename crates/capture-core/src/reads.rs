use crate::{
    ordering::{Control, Failure, Item, Reader, Snapshot},
    schema::{Format, Header},
    storage::{CopyStats, OwnedBlock},
};
use std::sync::{
    Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, Ordering::SeqCst},
};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(SeqCst)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Variable,
    Count,
    Into,
    Readiness,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadError {
    AlreadyPending,
    InvalidRequest,
    InvalidDestination,
    Poisoned,
    WrongMode,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartialReason {
    Complete,
    WouldBlock,
    Boundary,
    Cancelled,
    Timeout,
    Eof,
    Failure(Failure),
    Ready,
}

#[derive(Clone, Debug)]
pub struct Slice {
    pub block: OwnedBlock,
    pub first: usize,
    pub frames: usize,
}
impl Slice {
    pub fn bytes(&self) -> &[u8] {
        let stride = self.block.format().bytes_per_frame();
        &self.block.bytes()[self.first * stride..(self.first + self.frames) * stride]
    }
    pub fn header(&self) -> Header {
        let mut header = self.block.header();
        header.frames = self.frames;
        header.sample_position = header
            .sample_position
            .and_then(|position| position.checked_add(self.first as u64));
        if self.first != 0 {
            header.timestamp = None;
        }
        header
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketAnchor {
    pub header: Header,
    pub packet_offset: usize,
    pub output_offset: usize,
    pub frames: usize,
}

#[derive(Debug)]
pub struct ReadResult {
    pub frames: usize,
    pub chunks: Vec<Slice>,
    pub first_packet: Option<PacketAnchor>,
    pub last_packet: Option<PacketAnchor>,
    pub reason: PartialReason,
    pub control: Option<Control>,
    pub copies: CopyStats,
}

struct State {
    reader: Reader,
    remainder: Option<Slice>,
    pending_control: Option<Control>,
    terminal: Option<PartialReason>,
}
pub struct ReadStream {
    state: Mutex<State>,
    control: crate::ordering::ControlHandle,
}

pub struct Blocks<'a> {
    stream: &'a ReadStream,
}
impl Iterator for Blocks<'_> {
    type Item = Result<ReadResult, ReadError>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut operation = match self.stream.begin(
            Mode::Variable,
            usize::MAX / 32,
            false,
            None,
            Cancellation::default(),
        ) {
            Ok(operation) => operation,
            Err(error) => return Some(Err(error)),
        };
        match operation.poll(0) {
            Some(result) if result.reason != PartialReason::Eof => Some(Ok(result)),
            _ => None,
        }
    }
}

pub struct Operation<'a> {
    state: Option<MutexGuard<'a, State>>,
    mode: Mode,
    requested: usize,
    blocking: bool,
    deadline: Option<u64>,
    cancel: Cancellation,
    destination: Option<&'a mut [u8]>,
    format: Option<Arc<Format>>,
    generation: Option<u64>,
    segment: Option<u64>,
    frames: usize,
    chunks: Vec<Slice>,
    first_packet: Option<PacketAnchor>,
    last_packet: Option<PacketAnchor>,
    copies: CopyStats,
    assembly: Option<crate::storage::Block>,
}

impl ReadStream {
    pub fn status(&self) -> Snapshot {
        self.control.snapshot()
    }
    pub fn control_handle(&self) -> crate::ordering::ControlHandle {
        self.control.clone()
    }
    pub fn blocks(&self) -> Blocks<'_> {
        Blocks { stream: self }
    }
    pub fn new(reader: Reader) -> Self {
        Self {
            control: reader.control_handle(),
            state: Mutex::new(State {
                reader,
                remainder: None,
                pending_control: None,
                terminal: None,
            }),
        }
    }
    fn reserve(&self) -> Result<MutexGuard<'_, State>, ReadError> {
        self.state.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })
    }
    pub fn is_pending(&self) -> bool {
        matches!(self.state.try_lock(), Err(TryLockError::WouldBlock))
    }
    pub fn begin(
        &self,
        mode: Mode,
        requested: usize,
        blocking: bool,
        deadline: Option<u64>,
        cancel: Cancellation,
    ) -> Result<Operation<'_>, ReadError> {
        if requested == 0 || mode == Mode::Into {
            return Err(ReadError::InvalidRequest);
        }
        self.operation(mode, requested, blocking, deadline, cancel, None)
    }
    pub fn begin_into<'a>(
        &'a self,
        destination: &'a mut [u8],
        format: Arc<Format>,
        requested: usize,
        blocking: bool,
        deadline: Option<u64>,
        cancel: Cancellation,
    ) -> Result<Operation<'a>, ReadError> {
        if requested == 0
            || !destination.len().is_multiple_of(format.bytes_per_frame())
            || format
                .byte_count(requested)
                .map_err(|_| ReadError::InvalidDestination)?
                > destination.len()
        {
            return Err(ReadError::InvalidDestination);
        }
        self.operation(
            Mode::Into,
            requested,
            blocking,
            deadline,
            cancel,
            Some((destination, format)),
        )
    }
    fn operation<'a>(
        &'a self,
        mode: Mode,
        requested: usize,
        blocking: bool,
        deadline: Option<u64>,
        cancel: Cancellation,
        output: Option<(&'a mut [u8], Arc<Format>)>,
    ) -> Result<Operation<'a>, ReadError> {
        let (destination, format) = match output {
            Some((destination, format)) => (Some(destination), Some(format)),
            None => (None, None),
        };
        let state = self.reserve()?;
        if mode == Mode::Count
            && requested
                .checked_mul(state.reader.max_frame_bytes())
                .is_none_or(|bytes| bytes > isize::MAX as usize)
        {
            return Err(ReadError::InvalidRequest);
        }
        Ok(Operation {
            state: Some(state),
            mode,
            requested,
            blocking,
            deadline,
            cancel,
            destination,
            format,
            generation: None,
            segment: None,
            frames: 0,
            chunks: Vec::new(),
            first_packet: None,
            last_packet: None,
            copies: CopyStats::default(),
            assembly: None,
        })
    }
}

impl Operation<'_> {
    pub fn status(&self) -> Option<Snapshot> {
        self.state.as_ref().map(|state| state.reader.snapshot())
    }
    fn finish(&mut self, reason: PartialReason, control: Option<Control>) -> ReadResult {
        if let Some(anchor) = self.last_packet
            && let Some(trace) = self.state.as_ref().and_then(|state| state.reader.trace())
        {
            trace.record(
                crate::measurements::Stage::ReadSatisfied,
                anchor.header.sequence,
            );
        }
        if let Some(mut assembly) = self.assembly.take() {
            assembly.header.frames = self.frames;
            self.chunks.push(Slice {
                block: OwnedBlock {
                    owner: Arc::new(assembly),
                },
                first: 0,
                frames: self.frames,
            });
        }
        self.state.take();
        ReadResult {
            frames: self.frames,
            chunks: std::mem::take(&mut self.chunks),
            first_packet: self.first_packet,
            last_packet: self.last_packet,
            reason,
            control,
            copies: self.copies,
        }
    }
    pub fn poll(&mut self, now_ns: u64) -> Option<ReadResult> {
        self.state.as_ref()?;
        if self.cancel.is_cancelled() {
            return Some(self.finish(PartialReason::Cancelled, None));
        }
        if self.deadline.is_some_and(|deadline| now_ns >= deadline) {
            return Some(self.finish(PartialReason::Timeout, None));
        }
        for _ in 0..32 {
            let state = self.state.as_mut()?;
            if state.reader.control_handle().aborted() {
                if let Some(remainder) = state.remainder.take() {
                    state.reader.finish_delivery(remainder.frames, true);
                }
                state.pending_control = None;
            }
            if let Some(reason) = state.terminal {
                return Some(self.finish(reason, None));
            }
            if let Some(control) = state.pending_control.take() {
                return Some(self.finish(PartialReason::Boundary, Some(control)));
            }
            if self.mode == Mode::Readiness {
                if state.remainder.is_some() || state.reader.ready() {
                    return Some(self.finish(PartialReason::Ready, None));
                }
                return if self.blocking {
                    None
                } else {
                    Some(self.finish(PartialReason::WouldBlock, None))
                };
            }
            if self.cancel.is_cancelled() {
                return Some(self.finish(PartialReason::Cancelled, None));
            }
            if state.remainder.is_none() {
                match state.reader.next_item() {
                    Some(Item::Audio(block)) => {
                        let frames = block.header().frames;
                        state.reader.defer_delivery(frames);
                        state.remainder = Some(Slice {
                            block,
                            first: 0,
                            frames,
                        });
                    }
                    Some(Item::Control(control)) => {
                        return Some(self.finish(PartialReason::Boundary, Some(control)));
                    }
                    Some(Item::Failed(failure)) => {
                        state.terminal = Some(PartialReason::Failure(failure));
                        return Some(self.finish(PartialReason::Failure(failure), None));
                    }
                    None => {
                        if state.reader.ended() {
                            state.terminal = Some(PartialReason::Eof);
                            return Some(self.finish(PartialReason::Eof, None));
                        }
                        return if self.blocking {
                            None
                        } else {
                            Some(self.finish(PartialReason::WouldBlock, None))
                        };
                    }
                }
            }
            let slice = state.remainder.as_mut().expect("read remainder");
            let header = slice.block.header();
            if self
                .format
                .as_ref()
                .is_some_and(|format| format.as_ref() != slice.block.format())
                || self
                    .generation
                    .is_some_and(|generation| generation != header.generation)
                || self
                    .segment
                    .is_some_and(|segment| segment != header.segment)
            {
                let control = Control {
                    sequence: header.sequence,
                    generation: header.generation,
                    segment: header.segment,
                    format: slice.block.descriptor(),
                    kind: crate::ordering::ControlKind::Recovered,
                    losses: crate::ordering::Losses::default(),
                    records: 0,
                };
                return Some(self.finish(PartialReason::Boundary, Some(control)));
            }
            self.format.get_or_insert_with(|| slice.block.descriptor());
            self.generation = Some(header.generation);
            self.segment = Some(header.segment);
            let frames = slice.frames.min(self.requested - self.frames);
            let anchor = PacketAnchor {
                header,
                packet_offset: slice.first,
                output_offset: self.frames,
                frames,
            };
            self.first_packet.get_or_insert(anchor);
            self.last_packet = Some(anchor);
            let taken = Slice {
                block: slice.block.clone(),
                first: slice.first,
                frames,
            };
            if let Some(destination) = self.destination.as_mut() {
                let stride = taken.block.format().bytes_per_frame();
                self.copies.copy(
                    &mut destination[self.frames * stride..(self.frames + frames) * stride],
                    taken.bytes(),
                );
            } else if self.mode == Mode::Count {
                let stride = taken.block.format().bytes_per_frame();
                let assembly = self.assembly.get_or_insert_with(|| crate::storage::Block {
                    bytes: vec![0; self.requested * stride].into_boxed_slice(),
                    header: taken.header(),
                    format: taken.block.descriptor(),
                    _reservation: None,
                });
                self.copies.copy(
                    &mut assembly.bytes[self.frames * stride..(self.frames + frames) * stride],
                    taken.bytes(),
                );
            } else {
                self.chunks.push(taken);
            }
            slice.first += frames;
            slice.frames -= frames;
            self.frames += frames;
            if slice.frames == 0 {
                state.remainder = None;
            }
            state.reader.finish_delivery(frames, false);
            if self.frames == self.requested || self.mode == Mode::Variable {
                return Some(self.finish(PartialReason::Complete, None));
            }
        }
        None
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(remainder) = self.remainder.take() {
            self.reader.finish_delivery(remainder.frames, true);
        }
    }
}

pub fn partial_code(reason: PartialReason) -> (u32, u32) {
    match reason {
        PartialReason::Complete => (1, 0),
        PartialReason::WouldBlock => (2, 0),
        PartialReason::Boundary => (3, 0),
        PartialReason::Cancelled => (4, 0),
        PartialReason::Timeout => (5, 0),
        PartialReason::Eof => (6, 0),
        PartialReason::Failure(failure) => (7, crate::ordering::failure_code(failure)),
        PartialReason::Ready => (8, 0),
    }
}
pub fn partial_from_code(code: u32, failure: u32) -> Result<PartialReason, ReadError> {
    match (code, failure) {
        (1, 0) => Ok(PartialReason::Complete),
        (2, 0) => Ok(PartialReason::WouldBlock),
        (3, 0) => Ok(PartialReason::Boundary),
        (4, 0) => Ok(PartialReason::Cancelled),
        (5, 0) => Ok(PartialReason::Timeout),
        (6, 0) => Ok(PartialReason::Eof),
        (7, failure) => crate::ordering::failure_from_code(failure)
            .map(PartialReason::Failure)
            .ok_or(ReadError::InvalidRequest),
        (8, 0) => Ok(PartialReason::Ready),
        _ => Err(ReadError::InvalidRequest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        StorageLimits,
        ordering::{Overflow, channel},
        schema::{ByteOrder, Layout, Provenance, SampleType, Stamp},
        storage::Pool,
    };

    #[test]
    fn observed_handoff_preserves_into_allocation_and_event_order() {
        use crate::{
            measurements::{Stage, Trace},
            ordering::{Offer, channel_observed},
        };
        let format = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48000,
                2,
                Layout::Unknown,
            )
            .unwrap(),
        );
        for observed in [false, true] {
            let pool = Pool::new(
                StorageLimits::new(8, 16, 8, 1024).unwrap(),
                16,
                format.clone(),
            )
            .unwrap();
            let trace = Arc::new(Trace::new(1000).unwrap());
            let (mut producer, reader) = if observed {
                channel_observed(pool, 4, trace.clone()).unwrap()
            } else {
                channel(pool, 4).unwrap()
            };
            let stream = ReadStream::new(reader);
            let cancel = Cancellation::default();
            let mut destination = [0; 128];
            let warm_header = Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 16,
                sample_position: None,
                timestamp: None,
            };
            assert_eq!(
                producer.offer_silence(warm_header, Overflow::Fail),
                Offer::Queued
            );
            for offset in [0, 8] {
                let result = stream
                    .begin_into(
                        &mut destination,
                        format.clone(),
                        8,
                        false,
                        None,
                        cancel.clone(),
                    )
                    .unwrap()
                    .poll(0)
                    .unwrap();
                assert_eq!(result.frames, 8);
                assert_eq!(result.first_packet.unwrap().packet_offset, offset);
            }
            let measured = allocation_counter::measure(|| {
                for sequence in 2..=100 {
                    let header = Header {
                        sequence,
                        segment: 1,
                        generation: 1,
                        frames: 16,
                        sample_position: None,
                        timestamp: None,
                    };
                    assert_eq!(
                        producer.offer_silence(header, Overflow::Fail),
                        Offer::Queued
                    );
                    for offset in [0, 8] {
                        let result = stream
                            .begin_into(
                                &mut destination,
                                format.clone(),
                                8,
                                false,
                                None,
                                cancel.clone(),
                            )
                            .unwrap()
                            .poll(0)
                            .unwrap();
                        assert_eq!(result.frames, 8);
                        assert!(result.chunks.is_empty());
                        assert_eq!(result.first_packet.unwrap().packet_offset, offset);
                    }
                }
            });
            assert_eq!(measured.count_total, 0);
            assert!(!trace.overflowed());
            let events = trace.snapshot();
            if observed {
                assert_eq!(events.len(), 600);
                for (index, packet) in events.chunks_exact(6).enumerate() {
                    assert!(
                        packet
                            .iter()
                            .all(|event| event.sequence == index as u64 + 1)
                    );
                    assert_eq!(
                        packet.iter().map(|event| event.stage).collect::<Vec<_>>(),
                        [
                            Stage::PublishBegin,
                            Stage::PublishEnd,
                            Stage::DequeueBegin,
                            Stage::Dequeued,
                            Stage::ReadSatisfied,
                            Stage::ReadSatisfied
                        ]
                    );
                    assert!(
                        packet
                            .windows(2)
                            .all(|pair| pair[0].elapsed_ns <= pair[1].elapsed_ns)
                    );
                }
            } else {
                assert!(events.is_empty());
            }
        }
    }

    #[test]
    fn read_packet_anchors_preserve_raw_provenance() {
        let format = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48000,
                2,
                Layout::Unknown,
            )
            .unwrap(),
        );
        for mode in [Mode::Variable, Mode::Count, Mode::Into] {
            let pool = Pool::new(
                StorageLimits::new(8, 16, 8, 1024).unwrap(),
                16,
                format.clone(),
            )
            .unwrap();
            let (mut producer, reader) = channel(pool, 4).unwrap();
            let stream = ReadStream::new(reader);
            let header = Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 8,
                sample_position: None,
                timestamp: Some(Stamp {
                    ticks: 123,
                    ticks_per_second: 10_000_000,
                    clock_domain: 1,
                    provenance: Provenance::Native,
                    uncertainty_ns: None,
                }),
            };
            producer.offer(&[1; 64], header, Overflow::Fail);
            let mut destination = [0; 128];
            for offset in [0, 3] {
                let mut operation = if mode == Mode::Into {
                    stream
                        .begin_into(
                            &mut destination,
                            format.clone(),
                            3,
                            false,
                            None,
                            Cancellation::default(),
                        )
                        .unwrap()
                } else {
                    stream
                        .begin(mode, 3, false, None, Cancellation::default())
                        .unwrap()
                };
                let result = operation.poll(0).unwrap();
                let expected = Some(PacketAnchor {
                    header,
                    packet_offset: offset,
                    output_offset: 0,
                    frames: 3,
                });
                assert_eq!(result.first_packet, expected);
                assert_eq!(result.last_packet, expected);
                if mode == Mode::Into {
                    assert!(result.chunks.is_empty());
                }
            }
            let next = Header {
                sequence: 2,
                timestamp: None,
                ..header
            };
            producer.offer(&[2; 64], next, Overflow::Fail);
            let cancel = Cancellation::default();
            let mut operation = stream
                .begin_into(
                    &mut destination,
                    format.clone(),
                    16,
                    true,
                    None,
                    cancel.clone(),
                )
                .unwrap();
            assert!(operation.poll(0).is_none());
            cancel.cancel();
            let result = operation.poll(1).unwrap();
            assert_eq!(result.reason, PartialReason::Cancelled);
            assert_eq!(result.frames, 10);
            assert_eq!(
                result.first_packet,
                Some(PacketAnchor {
                    header,
                    packet_offset: 6,
                    output_offset: 0,
                    frames: 2
                })
            );
            assert_eq!(
                result.last_packet,
                Some(PacketAnchor {
                    header: next,
                    packet_offset: 0,
                    output_offset: 2,
                    frames: 8
                })
            );
            drop(operation);
            let mut empty = stream
                .begin(Mode::Variable, 1, false, None, Cancellation::default())
                .unwrap();
            let result = empty.poll(0).unwrap();
            assert_eq!(result.first_packet, None);
            assert_eq!(result.last_packet, None);
        }
    }
}
