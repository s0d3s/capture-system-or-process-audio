use crate::{
    schema::{Format, Header, Loss},
    storage::{Block, OwnedBlock, Pool, StorageError},
};
use crossbeam_utils::atomic::AtomicCell;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::AtomicBool,
    atomic::{AtomicU32, AtomicU64, Ordering::SeqCst},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LossCause {
    #[default]
    Source,
    QueueOldest,
    QueueNewest,
    PoolExhaustion,
    Abort,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DomainLoss {
    pub rate_hz: u32,
    pub known_frames: u64,
    pub unknown: bool,
    pub cause: LossCause,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Losses {
    entries: [DomainLoss; 8],
    length: usize,
}

impl Losses {
    pub fn entries(&self) -> &[DomainLoss] {
        &self.entries[..self.length]
    }
    fn add_entry(&mut self, loss: DomainLoss) -> Result<(), Failure> {
        if loss.rate_hz == 0 {
            return Err(Failure::InvalidPacket);
        }
        if let Some(entry) = self.entries[..self.length]
            .iter_mut()
            .find(|entry| entry.rate_hz == loss.rate_hz && entry.cause == loss.cause)
        {
            entry.known_frames = entry
                .known_frames
                .checked_add(loss.known_frames)
                .ok_or(Failure::ControlExhausted)?;
            entry.unknown |= loss.unknown;
        } else {
            if self.length == self.entries.len() {
                return Err(Failure::ControlExhausted);
            }
            self.entries[self.length] = loss;
            self.length += 1;
        }
        Ok(())
    }
    fn merge(&mut self, other: &Self) -> Result<(), Failure> {
        for entry in other.entries() {
            self.add_entry(*entry)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlKind {
    Format,
    Gap,
    Recovered,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Control {
    pub sequence: u64,
    pub generation: u64,
    pub segment: u64,
    pub format: Arc<Format>,
    pub kind: ControlKind,
    pub losses: Losses,
    pub records: u64,
}

#[derive(Clone, Debug)]
pub struct TerminalDetails {
    pub failure: Failure,
    pub state: Control,
    pub unrecorded_loss: Option<DomainLoss>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Overflow {
    DropOldest,
    DropNewest,
    Fail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    QueueOverflow,
    PoolExhaustion,
    PacketTooLarge,
    InvalidPacket,
    TicketExhausted,
    ControlExhausted,
    Backend,
    Callback,
    CallbackPanic,
}

pub(crate) fn failure_code(failure: Failure) -> u32 {
    match failure {
        Failure::QueueOverflow => 1,
        Failure::PoolExhaustion => 2,
        Failure::PacketTooLarge => 3,
        Failure::InvalidPacket => 4,
        Failure::TicketExhausted => 5,
        Failure::ControlExhausted => 6,
        Failure::Backend => 7,
        Failure::Callback => 8,
        Failure::CallbackPanic => 9,
    }
}

pub(crate) fn failure_from_code(code: u32) -> Option<Failure> {
    match code {
        1 => Some(Failure::QueueOverflow),
        2 => Some(Failure::PoolExhaustion),
        3 => Some(Failure::PacketTooLarge),
        4 => Some(Failure::InvalidPacket),
        5 => Some(Failure::TicketExhausted),
        6 => Some(Failure::ControlExhausted),
        7 => Some(Failure::Backend),
        8 => Some(Failure::Callback),
        9 => Some(Failure::CallbackPanic),
        _ => None,
    }
}

struct Slot<Owner> {
    ticket: AtomicU64,
    logical: AtomicU64,
    frames: AtomicU64,
    rate: AtomicU32,
    owner: AtomicCell<Option<Arc<Owner>>>,
}

impl<Owner> Slot<Owner> {
    fn new() -> Self {
        Self {
            ticket: AtomicU64::new(0),
            logical: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            rate: AtomicU32::new(0),
            owner: AtomicCell::new(None),
        }
    }
    fn claim(&self, ticket: u64) -> bool {
        self.ticket
            .compare_exchange(ticket, 0, SeqCst, SeqCst)
            .is_ok()
    }
}

#[derive(Default)]
struct Counters {
    input: AtomicU64,
    delivered: AtomicU64,
    queued: AtomicU64,
    in_flight: AtomicU64,
    queue_lost: AtomicU64,
    pool_lost: AtomicU64,
    rejected: AtomicU64,
    discarded: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameDuration {
    pub frames: u64,
    pub rate_hz: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub input: u64,
    pub delivered: u64,
    pub queued: u64,
    pub in_flight: u64,
    pub queue_lost: u64,
    pub pool_lost: u64,
    pub rejected: u64,
    pub discarded: u64,
    pub failure: Option<Failure>,
    pub stable: bool,
    pub queued_durations: Vec<FrameDuration>,
    pub queued_domains: usize,
    pub in_flight_duration: FrameDuration,
}

struct Shared {
    trace: Option<Arc<crate::measurements::Trace>>,
    audio: Box<[Slot<Block>]>,
    controls: Box<[Slot<Control>]>,
    max_frame_bytes: usize,
    epoch: AtomicU64,
    reader_epoch: AtomicU64,
    reader_audio: AtomicU64,
    in_flight_rate: AtomicU32,
    published: AtomicU64,
    failure: AtomicU32,
    ended: AtomicBool,
    stop: AtomicU32,
    stop_epoch: AtomicU64,
    stop_serial: Mutex<()>,
    producer_done: AtomicBool,
    consumer_done: AtomicBool,
    terminal: OnceLock<TerminalDetails>,
    counters: Counters,
}

impl Shared {
    fn oldest_control(&self, horizon: u64) -> Option<(usize, u64, u64)> {
        let mut oldest = None;
        for (index, slot) in self.controls.iter().enumerate() {
            let ticket = slot.ticket.load(SeqCst);
            if ticket != 0 && ticket <= horizon {
                let logical = slot.logical.load(SeqCst);
                if oldest.is_none_or(|(_, _, previous)| logical < previous) {
                    oldest = Some((index, ticket, logical));
                }
            }
        }
        oldest
    }
    fn oldest_audio(&self, horizon: u64) -> (usize, Option<(usize, u64, u64)>) {
        let mut count = 0;
        let mut oldest = None;
        for (index, slot) in self.audio.iter().enumerate() {
            let ticket = slot.ticket.load(SeqCst);
            if ticket != 0 && ticket <= horizon {
                let logical = slot.logical.load(SeqCst);
                count += 1;
                if oldest.is_none_or(|(_, _, previous)| logical < previous) {
                    oldest = Some((index, ticket, logical));
                }
            }
        }
        (count, oldest)
    }

    fn snapshot(&self) -> Snapshot {
        let epoch = self.epoch.load(SeqCst);
        let stop_epoch = self.stop_epoch.load(SeqCst);
        let reader_epoch = self.reader_epoch.load(SeqCst);
        let counters = &self.counters;
        let mut queued_durations: Vec<FrameDuration> = Vec::with_capacity(self.audio.len());
        for slot in self.audio.iter() {
            if slot.ticket.load(SeqCst) == 0 {
                continue;
            }
            let rate = slot.rate.load(SeqCst);
            if let Some(domain) = queued_durations
                .iter_mut()
                .find(|domain| domain.rate_hz == rate)
            {
                domain.frames = domain.frames.saturating_add(slot.frames.load(SeqCst));
            } else {
                queued_durations.push(FrameDuration {
                    frames: slot.frames.load(SeqCst),
                    rate_hz: rate,
                });
            }
        }
        Snapshot {
            queued_domains: queued_durations.len(),
            queued_durations,
            in_flight_duration: FrameDuration {
                frames: counters.in_flight.load(SeqCst),
                rate_hz: self.in_flight_rate.load(SeqCst),
            },
            input: counters.input.load(SeqCst),
            delivered: counters.delivered.load(SeqCst),
            queued: counters.queued.load(SeqCst),
            in_flight: counters.in_flight.load(SeqCst),
            queue_lost: counters.queue_lost.load(SeqCst),
            pool_lost: counters.pool_lost.load(SeqCst),
            rejected: counters.rejected.load(SeqCst),
            discarded: counters.discarded.load(SeqCst),
            failure: failure_from_code(self.failure.load(SeqCst)),
            stable: epoch.is_multiple_of(2)
                && stop_epoch.is_multiple_of(2)
                && stop_epoch == self.stop_epoch.load(SeqCst)
                && reader_epoch.is_multiple_of(2)
                && epoch == self.epoch.load(SeqCst)
                && reader_epoch == self.reader_epoch.load(SeqCst),
        }
    }
}

struct Publication<'a>(&'a AtomicU64);
impl Drop for Publication<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, SeqCst);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Stop {
    #[default]
    Drain,
    Abort,
}

#[derive(Clone)]
pub struct ControlHandle {
    shared: Arc<Shared>,
}
impl ControlHandle {
    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot()
    }
    pub fn stop(&self, mode: Stop) {
        self.shared
            .stop
            .fetch_max(if mode == Stop::Abort { 2 } else { 1 }, SeqCst);
        self.shared.ended.store(true, SeqCst);
        if self.aborted() {
            self.discard_queued();
        }
    }
    pub fn aborted(&self) -> bool {
        self.shared.stop.load(SeqCst) == 2
    }
    pub fn stopping(&self) -> bool {
        self.shared.stop.load(SeqCst) != 0
    }
    pub fn delivery_done(&self) -> bool {
        self.shared.consumer_done.load(SeqCst)
    }
    pub fn resources_done(&self) -> bool {
        self.shared.producer_done.load(SeqCst)
    }
    pub fn fail(&self, failure: Failure) {
        let _ = self
            .shared
            .failure
            .compare_exchange(0, failure_code(failure), SeqCst, SeqCst);
        self.shared.ended.store(true, SeqCst);
    }
    fn discard_queued(&self) {
        let _guard = self
            .shared
            .stop_serial
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.stop_epoch.fetch_add(1, SeqCst);
        let _publication = Publication(&self.shared.stop_epoch);
        for slot in self.shared.audio.iter() {
            let ticket = slot.ticket.load(SeqCst);
            if ticket != 0 && slot.claim(ticket) {
                let owner = slot.owner.take().expect("claimed discard owner");
                let frames = owner.header.frames as u64;
                self.shared.counters.queued.fetch_sub(frames, SeqCst);
                self.shared.counters.discarded.fetch_add(frames, SeqCst);
            }
        }
        for slot in self.shared.controls.iter() {
            let ticket = slot.ticket.load(SeqCst);
            if ticket != 0 && slot.claim(ticket) {
                slot.owner.take();
            }
        }
    }
}

struct ProducerCompletion(Arc<Shared>);
impl Drop for ProducerCompletion {
    fn drop(&mut self) {
        self.0.producer_done.store(true, SeqCst);
    }
}

pub struct Producer {
    pool: Pool,
    shared: Arc<Shared>,
    capacity: usize,
    ticket: u64,
    attempts: usize,
    controls: Box<[Arc<Control>]>,
    last_sequence: Option<u64>,
    segment: u64,
    losses: Losses,
    unrecorded_loss: Option<DomainLoss>,
    _completion: ProducerCompletion,
}
pub struct Reader {
    shared: Arc<Shared>,
    pending: Vec<Item>,
    terminal_read: bool,
}

#[derive(Debug)]
pub enum Item {
    Audio(OwnedBlock),
    Control(Control),
    Failed(Failure),
}

impl Item {
    fn sequence(&self) -> u64 {
        match self {
            Self::Audio(block) => block.header().sequence,
            Self::Control(control) => control.sequence,
            Self::Failed(_) => u64::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Offer {
    Queued,
    QueueDropped,
    PoolDropped,
    Stopped,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueCapacity {
    pub handles: usize,
    pub frames: usize,
    pub duration_ns: u64,
    pub accounted_bytes: usize,
}

pub fn duration_capacity(
    format: &Format,
    duration_ms: u64,
    byte_ceiling: usize,
    packet_frames: usize,
    preferred_frames: usize,
) -> Result<QueueCapacity, Failure> {
    if duration_ms == 0 || packet_frames == 0 || preferred_frames == 0 {
        return Err(Failure::InvalidPacket);
    }
    let frames = u64::from(format.rate())
        .checked_mul(duration_ms)
        .ok_or(Failure::InvalidPacket)?
        .div_ceil(1000);
    let frames = usize::try_from(frames).map_err(|_| Failure::InvalidPacket)?;
    let per_handle = std::mem::size_of::<Slot<Block>>();
    let controls = 8usize
        .checked_mul(
            std::mem::size_of::<Slot<Control>>()
                + std::mem::size_of::<Control>()
                + 3 * std::mem::size_of::<usize>(),
        )
        .ok_or(Failure::InvalidPacket)?;
    let per_packet = format
        .byte_count(packet_frames)
        .map_err(|_| Failure::InvalidPacket)?
        .checked_add(per_handle)
        .ok_or(Failure::InvalidPacket)?;
    let maximum = byte_ceiling
        .checked_sub(controls)
        .ok_or(Failure::InvalidPacket)?
        / per_packet;
    let handles = frames.div_ceil(packet_frames).min(maximum);
    if handles == 0 {
        return Err(Failure::InvalidPacket);
    }
    let effective = handles
        .checked_mul(packet_frames)
        .ok_or(Failure::InvalidPacket)?;
    let pcm = format
        .byte_count(effective)
        .map_err(|_| Failure::InvalidPacket)?;
    let accounted_bytes = handles
        .checked_mul(per_handle)
        .and_then(|bytes| bytes.checked_add(pcm))
        .and_then(|bytes| bytes.checked_add(controls))
        .ok_or(Failure::InvalidPacket)?;
    if accounted_bytes > byte_ceiling {
        return Err(Failure::InvalidPacket);
    }
    let duration_ns = u64::try_from(effective)
        .ok()
        .and_then(|frames| frames.checked_mul(1_000_000_000))
        .ok_or(Failure::InvalidPacket)?
        / u64::from(format.rate());
    Ok(QueueCapacity {
        handles,
        frames: effective,
        duration_ns,
        accounted_bytes,
    })
}

pub fn channel(pool: Pool, capacity: usize) -> Result<(Producer, Reader), Failure> {
    channel_impl(pool, capacity, None)
}

pub fn channel_observed(
    pool: Pool,
    capacity: usize,
    trace: Arc<crate::measurements::Trace>,
) -> Result<(Producer, Reader), Failure> {
    channel_impl(pool, capacity, Some(trace))
}

fn channel_impl(
    pool: Pool,
    capacity: usize,
    trace: Option<Arc<crate::measurements::Trace>>,
) -> Result<(Producer, Reader), Failure> {
    if capacity == 0
        || capacity > pool.limits().slots()
        || !AtomicCell::<Option<Arc<Block>>>::is_lock_free()
    {
        return Err(Failure::InvalidPacket);
    }
    if !AtomicCell::<Option<Arc<Control>>>::is_lock_free() {
        return Err(Failure::InvalidPacket);
    }
    let initial = Control {
        sequence: 0,
        generation: pool.generation(),
        segment: 1,
        format: pool.descriptor(),
        kind: ControlKind::Format,
        losses: Losses::default(),
        records: 1,
    };
    let controls = (0..8).map(|_| Arc::new(initial.clone())).collect();
    let shared = Arc::new(Shared {
        trace,
        audio: (0..pool.limits().slots()).map(|_| Slot::new()).collect(),
        controls: (0..8).map(|_| Slot::new()).collect(),
        max_frame_bytes: pool.limits().bytes_per_slot() / pool.max_frames(),
        epoch: AtomicU64::new(0),
        reader_epoch: AtomicU64::new(0),
        reader_audio: AtomicU64::new(u64::MAX),
        in_flight_rate: AtomicU32::new(0),
        published: AtomicU64::new(0),
        failure: AtomicU32::new(0),
        ended: AtomicBool::new(false),
        stop: AtomicU32::new(0),
        stop_epoch: AtomicU64::new(0),
        stop_serial: Mutex::new(()),
        producer_done: AtomicBool::new(false),
        consumer_done: AtomicBool::new(false),
        terminal: OnceLock::new(),
        counters: Counters::default(),
    });
    Ok((
        Producer {
            pool,
            shared: shared.clone(),
            capacity,
            ticket: 1,
            attempts: 0,
            controls,
            last_sequence: None,
            segment: 1,
            losses: Losses::default(),
            unrecorded_loss: None,
            _completion: ProducerCompletion(shared.clone()),
        },
        Reader {
            shared,
            pending: Vec::with_capacity(9),
            terminal_read: false,
        },
    ))
}

impl Producer {
    pub fn trace(&self) -> Option<&crate::measurements::Trace> {
        self.shared.trace.as_deref()
    }

    pub fn reject_uncertain(&mut self, sequence: u64, frames: usize) -> Result<(), Failure> {
        if frames == 0
            || self.last_sequence.is_some_and(|last| sequence <= last)
            || self.shared.ended.load(SeqCst)
        {
            return Err(Failure::InvalidPacket);
        }
        let shared = self.shared.clone();
        shared.epoch.fetch_add(1, SeqCst);
        let _publication = Publication(&shared.epoch);
        shared.counters.input.fetch_add(frames as u64, SeqCst);
        shared.counters.discarded.fetch_add(frames as u64, SeqCst);
        self.last_sequence = Some(sequence);
        let result = self.loss(
            sequence,
            self.segment,
            self.pool.descriptor(),
            self.pool.generation(),
            Loss {
                frames: Some(frames as u64),
                rate_hz: self.pool.format().rate(),
            },
            LossCause::Uncertain,
        );
        self.fail(Failure::Backend);
        result
    }
    pub fn control_handle(&self) -> ControlHandle {
        ControlHandle {
            shared: self.shared.clone(),
        }
    }
    pub fn close(&mut self) {
        self.shared.ended.store(true, SeqCst);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot()
    }
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
    pub fn attempts(&self) -> usize {
        self.attempts
    }
    pub fn terminal_details(&self) -> Option<&TerminalDetails> {
        self.shared.terminal.get()
    }

    fn fail(&self, failure: Failure) -> Offer {
        if self
            .shared
            .failure
            .compare_exchange(0, failure_code(failure), SeqCst, SeqCst)
            .is_ok()
        {
            let _ = self.shared.terminal.set(TerminalDetails {
                failure,
                state: Control {
                    sequence: self.last_sequence.unwrap_or(0),
                    generation: self.pool.generation(),
                    segment: self.segment,
                    format: self.pool.descriptor(),
                    kind: ControlKind::Recovered,
                    losses: self.losses,
                    records: 0,
                },
                unrecorded_loss: self.unrecorded_loss,
            });
        }
        self.shared.ended.store(true, SeqCst);
        Offer::Failed(failure_from_code(self.shared.failure.load(SeqCst)).unwrap_or(failure))
    }

    pub fn fail_capture(&mut self, failure: Failure) {
        let shared = self.shared.clone();
        shared.epoch.fetch_add(1, SeqCst);
        let _publication = Publication(&shared.epoch);
        self.fail(failure);
    }

    fn publish_control_owner(&mut self, index: usize, owner: Arc<Control>) {
        let slot = &self.shared.controls[index];
        slot.logical.store(owner.sequence, SeqCst);
        assert!(slot.owner.swap(Some(owner)).is_none());
        slot.ticket.store(self.ticket, SeqCst);
        self.shared.published.store(self.ticket, SeqCst);
        self.ticket += 1;
    }

    fn control(&mut self, control: Control) -> Result<(), Failure> {
        if self.ticket > u64::MAX - 32 {
            return Err(Failure::TicketExhausted);
        }
        for _ in 0..=self.controls.len() {
            if let Some(index) = self
                .controls
                .iter_mut()
                .position(|owner| Arc::get_mut(owner).is_some())
            {
                *Arc::get_mut(&mut self.controls[index]).expect("free control") = control;
                self.publish_control_owner(index, self.controls[index].clone());
                return Ok(());
            }
            let mut pair = None;
            for (left, slot) in self.shared.controls.iter().enumerate() {
                let first_ticket = slot.ticket.load(SeqCst);
                if first_ticket == 0 {
                    continue;
                }
                let first_order = slot.logical.load(SeqCst);
                let mut next = None;
                for (right, following) in self.shared.controls.iter().enumerate() {
                    let ticket = following.ticket.load(SeqCst);
                    let order = following.logical.load(SeqCst);
                    if ticket != 0
                        && order > first_order
                        && next.is_none_or(|(_, _, previous)| order < previous)
                    {
                        next = Some((right, ticket, order));
                    }
                }
                if let Some((right, second_ticket, second_order)) = next {
                    let queued_separator = self.shared.audio.iter().any(|audio| {
                        let order = audio.logical.load(SeqCst);
                        audio.ticket.load(SeqCst) != 0
                            && first_order < order
                            && order < second_order
                    });
                    let pending = self.shared.reader_audio.load(SeqCst);
                    let separates =
                        queued_separator || (first_order < pending && pending < second_order);
                    if !separates {
                        pair = Some((left, first_ticket, right, second_ticket));
                        break;
                    }
                }
            }
            let (left, first_ticket, right, second_ticket) =
                pair.ok_or(Failure::ControlExhausted)?;
            if !self.shared.controls[left].claim(first_ticket) {
                continue;
            }
            let first = self.shared.controls[left]
                .owner
                .take()
                .expect("claimed control");
            if !self.shared.controls[right].claim(second_ticket) {
                self.publish_control_owner(left, first);
                continue;
            }
            let second = self.shared.controls[right]
                .owner
                .take()
                .expect("claimed control");
            let mut merged = (*second).clone();
            if let Err(error) = merged.losses.merge(&first.losses) {
                self.publish_control_owner(left, first);
                self.publish_control_owner(right, second);
                return Err(error);
            }
            merged.kind = ControlKind::Recovered;
            merged.records = merged
                .records
                .checked_add(first.records)
                .ok_or(Failure::ControlExhausted)?;
            drop(first);
            drop(second);
            *Arc::get_mut(&mut self.controls[right]).expect("claimed canonical control") = merged;
            self.publish_control_owner(right, self.controls[right].clone());
        }
        Err(Failure::ControlExhausted)
    }

    fn loss(
        &mut self,
        sequence: u64,
        segment: u64,
        format: Arc<Format>,
        generation: u64,
        loss: Loss,
        cause: LossCause,
    ) -> Result<(), Failure> {
        let entry = DomainLoss {
            rate_hz: loss.rate_hz,
            known_frames: loss.frames.unwrap_or(0),
            unknown: loss.frames.is_none(),
            cause,
        };
        if let Err(error) = self.losses.add_entry(entry) {
            self.unrecorded_loss = Some(entry);
            return Err(error);
        }
        let mut losses = Losses::default();
        losses.add_entry(entry)?;
        self.control(Control {
            sequence,
            generation,
            segment,
            format,
            kind: ControlKind::Gap,
            losses,
            records: 1,
        })
    }

    pub fn gap(&mut self, sequence: u64, segment: u64, loss: Loss) -> Result<(), Failure> {
        if self.shared.ended.load(SeqCst)
            || self.last_sequence.is_some_and(|last| sequence <= last)
            || loss.rate_hz == 0
            || self.shared.failure.load(SeqCst) != 0
        {
            return Err(Failure::InvalidPacket);
        }
        let shared = self.shared.clone();
        shared.epoch.fetch_add(1, SeqCst);
        let _publication = Publication(&shared.epoch);
        self.last_sequence = Some(sequence);
        self.segment = segment;
        let result = self.loss(
            sequence,
            segment,
            self.pool.descriptor(),
            self.pool.generation(),
            loss,
            LossCause::Source,
        );
        if let Err(error) = result {
            self.fail(error);
        }
        result
    }

    pub fn prepare_format(
        &mut self,
        sequence: u64,
        segment: u64,
        format: Arc<Format>,
        slots: usize,
    ) -> Result<u64, Failure> {
        if self.shared.ended.load(SeqCst)
            || self.last_sequence.is_some_and(|last| sequence <= last)
            || self.shared.failure.load(SeqCst) != 0
        {
            return Err(Failure::InvalidPacket);
        }
        let shared = self.shared.clone();
        shared.epoch.fetch_add(1, SeqCst);
        let _publication = Publication(&shared.epoch);
        let generation = self
            .pool
            .prepare_generation(format.clone(), slots)
            .map_err(|_| Failure::PoolExhaustion)?;
        self.last_sequence = Some(sequence);
        self.segment = segment;
        if let Err(error) = self.control(Control {
            sequence,
            generation,
            segment,
            format,
            kind: ControlKind::Format,
            losses: Losses::default(),
            records: 1,
        }) {
            self.fail(error);
            return Err(error);
        }
        Ok(generation)
    }

    pub fn offer(&mut self, bytes: &[u8], header: Header, policy: Overflow) -> Offer {
        self.offer_impl(Some(bytes), header, policy, || {})
    }

    pub fn offer_silence(&mut self, header: Header, policy: Overflow) -> Offer {
        self.offer_impl(None, header, policy, || {})
    }

    #[cfg(feature = "test-hooks")]
    pub fn offer_with_publish_hook(
        &mut self,
        bytes: &[u8],
        header: Header,
        policy: Overflow,
        hook: impl FnOnce(),
    ) -> Offer {
        self.offer_impl(Some(bytes), header, policy, hook)
    }

    fn offer_impl(
        &mut self,
        bytes: Option<&[u8]>,
        header: Header,
        policy: Overflow,
        hook: impl FnOnce(),
    ) -> Offer {
        let shared = self.shared.clone();
        shared.epoch.fetch_add(1, SeqCst);
        let _publication = Publication(&shared.epoch);
        if let Some(failure) = failure_from_code(self.shared.failure.load(SeqCst)) {
            return Offer::Failed(failure);
        }
        if self.shared.ended.load(SeqCst) {
            return Offer::Stopped;
        }
        shared
            .counters
            .input
            .fetch_add(header.frames as u64, SeqCst);
        self.attempts = 0;
        let invalid = if header.frames > self.pool.max_frames() {
            Some(Failure::PacketTooLarge)
        } else if header.frames == 0
            || header.generation != self.pool.generation()
            || bytes.is_some_and(|bytes| {
                self.pool.format().byte_count(header.frames) != Ok(bytes.len())
            })
            || self
                .last_sequence
                .is_some_and(|last| header.sequence <= last)
            || header.sequence == u64::MAX
        {
            Some(Failure::InvalidPacket)
        } else if self.ticket > u64::MAX - 33 {
            Some(Failure::TicketExhausted)
        } else {
            None
        };
        if let Some(failure) = invalid {
            shared
                .counters
                .rejected
                .fetch_add(header.frames as u64, SeqCst);
            return self.fail(failure);
        }
        self.last_sequence = Some(header.sequence);
        self.segment = header.segment;
        for _ in 0..=self.pool.limits().slots() {
            self.attempts += 1;
            let (queued, oldest) = shared.oldest_audio(u64::MAX);
            let pool_empty = self.pool.available() == 0;
            if queued >= self.capacity || pool_empty {
                if policy == Overflow::DropOldest
                    && let Some((index, ticket, _)) = oldest
                {
                    let slot = &shared.audio[index];
                    if !slot.claim(ticket) {
                        continue;
                    }
                    let owner = slot.owner.take().expect("claimed audio owner");
                    let frames = owner.header.frames as u64;
                    shared.counters.queued.fetch_sub(frames, SeqCst);
                    shared.counters.queue_lost.fetch_add(frames, SeqCst);
                    let lost_header = owner.header;
                    let lost_format = owner.format.clone();
                    drop(owner);
                    if let Err(error) = self.loss(
                        lost_header.sequence,
                        lost_header.segment,
                        lost_format.clone(),
                        lost_header.generation,
                        Loss {
                            frames: Some(frames),
                            rate_hz: lost_format.rate(),
                        },
                        LossCause::QueueOldest,
                    ) {
                        shared
                            .counters
                            .rejected
                            .fetch_add(header.frames as u64, SeqCst);
                        return self.fail(error);
                    }
                } else {
                    if policy == Overflow::Fail {
                        shared
                            .counters
                            .rejected
                            .fetch_add(header.frames as u64, SeqCst);
                        return self.fail(if queued >= self.capacity {
                            Failure::QueueOverflow
                        } else {
                            Failure::PoolExhaustion
                        });
                    }
                    let (counter, outcome) = if queued >= self.capacity {
                        (&shared.counters.queue_lost, Offer::QueueDropped)
                    } else {
                        (&shared.counters.pool_lost, Offer::PoolDropped)
                    };
                    counter.fetch_add(header.frames as u64, SeqCst);
                    let cause = if queued >= self.capacity {
                        LossCause::QueueNewest
                    } else {
                        LossCause::PoolExhaustion
                    };
                    if let Err(error) = self.loss(
                        header.sequence,
                        header.segment,
                        self.pool.descriptor(),
                        header.generation,
                        Loss {
                            frames: Some(header.frames as u64),
                            rate_hz: self.pool.format().rate(),
                        },
                        cause,
                    ) {
                        return self.fail(error);
                    }
                    return outcome;
                }
            }
            let stored = match bytes {
                Some(bytes) => self.pool.offer(bytes, header),
                None => self.pool.offer_silence(header),
            };
            let owner = match stored {
                Ok(block) => block.owner,
                Err(StorageError::Exhausted) => {
                    shared
                        .counters
                        .pool_lost
                        .fetch_add(header.frames as u64, SeqCst);
                    return Offer::PoolDropped;
                }
                Err(_) => {
                    shared
                        .counters
                        .rejected
                        .fetch_add(header.frames as u64, SeqCst);
                    return self.fail(Failure::InvalidPacket);
                }
            };
            let index = self
                .pool
                .blocks
                .iter()
                .position(|canonical| Arc::ptr_eq(canonical, &owner))
                .expect("canonical pool owner");
            let slot = &shared.audio[index];
            hook();
            slot.logical.store(header.sequence, SeqCst);
            slot.frames.store(header.frames as u64, SeqCst);
            slot.rate.store(self.pool.format().rate(), SeqCst);
            assert!(slot.owner.swap(Some(owner)).is_none());
            shared
                .counters
                .queued
                .fetch_add(header.frames as u64, SeqCst);
            slot.ticket.store(self.ticket, SeqCst);
            if let Some(trace) = &shared.trace {
                trace.record(crate::measurements::Stage::PublishBegin, header.sequence);
            }
            shared.published.store(self.ticket, SeqCst);
            if let Some(trace) = &shared.trace {
                trace.record(crate::measurements::Stage::PublishEnd, header.sequence);
            }
            self.ticket += 1;
            return Offer::Queued;
        }
        shared
            .counters
            .rejected
            .fetch_add(header.frames as u64, SeqCst);
        self.fail(Failure::ControlExhausted)
    }
}

impl Reader {
    pub(crate) fn trace(&self) -> Option<&crate::measurements::Trace> {
        self.shared.trace.as_deref()
    }

    pub(crate) fn max_frame_bytes(&self) -> usize {
        self.shared.max_frame_bytes
    }

    pub(crate) fn defer_delivery(&mut self, frames: usize) {
        self.shared.reader_epoch.fetch_add(1, SeqCst);
        let _reading = Publication(&self.shared.reader_epoch);
        self.shared
            .counters
            .delivered
            .fetch_sub(frames as u64, SeqCst);
        self.shared
            .counters
            .in_flight
            .fetch_add(frames as u64, SeqCst);
    }
    pub(crate) fn finish_delivery(&mut self, frames: usize, discard: bool) {
        self.shared.reader_epoch.fetch_add(1, SeqCst);
        let _reading = Publication(&self.shared.reader_epoch);
        self.shared
            .counters
            .in_flight
            .fetch_sub(frames as u64, SeqCst);
        let counter = if discard {
            &self.shared.counters.discarded
        } else {
            &self.shared.counters.delivered
        };
        counter.fetch_add(frames as u64, SeqCst);
    }
    pub fn control_handle(&self) -> ControlHandle {
        ControlHandle {
            shared: self.shared.clone(),
        }
    }
    pub fn ready(&self) -> bool {
        !self.pending.is_empty()
            || self.shared.oldest_audio(u64::MAX).0 != 0
            || self.shared.oldest_control(u64::MAX).is_some()
            || self.shared.failure.load(SeqCst) != 0
            || self.shared.ended.load(SeqCst)
    }

    pub fn ended(&self) -> bool {
        self.ended_impl(|| {})
    }

    #[cfg(feature = "test-hooks")]
    pub fn ended_with_hook(&self, hook: impl FnOnce()) -> bool {
        self.ended_impl(hook)
    }

    fn ended_impl(&self, hook: impl FnOnce()) -> bool {
        let epoch = self.shared.epoch.load(SeqCst);
        hook();
        epoch.is_multiple_of(2)
            && self.shared.ended.load(SeqCst)
            && (self.shared.failure.load(SeqCst) == 0 || self.terminal_read)
            && self.pending.is_empty()
            && self.shared.oldest_audio(u64::MAX).0 == 0
            && self.shared.oldest_control(u64::MAX).is_none()
            && epoch == self.shared.epoch.load(SeqCst)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot()
    }
    pub fn terminal_details(&self) -> Option<&TerminalDetails> {
        self.shared.terminal.get()
    }
    pub fn next_item(&mut self) -> Option<Item> {
        self.next_item_impl(|| {}, || {})
    }

    #[cfg(feature = "test-hooks")]
    pub fn next_item_with_claim_hook(&mut self, hook: impl FnOnce()) -> Option<Item> {
        self.next_item_impl(hook, || {})
    }

    #[cfg(feature = "test-hooks")]
    pub fn next_item_with_terminal_hook(&mut self, hook: impl FnOnce()) -> Option<Item> {
        self.next_item_impl(|| {}, hook)
    }

    fn next_item_impl(
        &mut self,
        hook: impl FnOnce(),
        terminal_hook: impl FnOnce(),
    ) -> Option<Item> {
        if self.shared.stop.load(SeqCst) == 2 {
            self.control_handle().discard_queued();
        }
        let mut hook = Some(hook);
        let shared = self.shared.clone();
        shared.reader_epoch.fetch_add(1, SeqCst);
        let _reading = Publication(&shared.reader_epoch);
        let epoch = shared.epoch.load(SeqCst);
        if !epoch.is_multiple_of(2) {
            return None;
        }
        let horizon = shared.published.load(SeqCst);
        for _ in 0..=shared.audio.len() + shared.controls.len() {
            let pending = self
                .pending
                .iter()
                .enumerate()
                .min_by_key(|(_, item)| item.sequence())
                .map(|(index, item)| (index, item.sequence()));
            let audio = shared.oldest_audio(horizon).1;
            let control = shared.oldest_control(horizon);
            let next_audio = audio
                .is_some_and(|(_, _, order)| control.is_none_or(|(_, _, other)| order < other));
            let candidate = if next_audio { audio } else { control };
            if let Some((index, order)) = pending
                && candidate.is_none_or(|(_, _, other)| order <= other)
            {
                if shared.epoch.load(SeqCst) != epoch {
                    return None;
                }
                let item = self.pending.swap_remove(index);
                if let Item::Audio(block) = &item {
                    shared
                        .counters
                        .in_flight
                        .fetch_sub(block.header().frames as u64, SeqCst);
                    shared
                        .counters
                        .delivered
                        .fetch_add(block.header().frames as u64, SeqCst);
                    shared.reader_audio.store(u64::MAX, SeqCst);
                }
                return Some(item);
            }
            let Some((index, ticket, order)) = candidate else {
                break;
            };
            assert!(
                self.pending.len() < self.pending.capacity(),
                "bounded reader pending storage"
            );
            if next_audio {
                shared.reader_audio.store(order, SeqCst);
                let slot = &shared.audio[index];
                let claim_started = shared.trace.as_ref().map(|_| std::time::Instant::now());
                if !slot.claim(ticket) {
                    shared.reader_audio.store(u64::MAX, SeqCst);
                    continue;
                }
                if let Some(hook) = hook.take() {
                    hook();
                }
                let owner = slot.owner.take().expect("claimed audio owner");
                if let Some(trace) = &shared.trace {
                    trace.record_at(
                        crate::measurements::Stage::DequeueBegin,
                        owner.header.sequence,
                        claim_started.unwrap(),
                    );
                    trace.record(crate::measurements::Stage::Dequeued, owner.header.sequence);
                }
                shared.in_flight_rate.store(owner.format.rate(), SeqCst);
                shared
                    .counters
                    .queued
                    .fetch_sub(owner.header.frames as u64, SeqCst);
                shared
                    .counters
                    .in_flight
                    .fetch_add(owner.header.frames as u64, SeqCst);
                self.pending.push(Item::Audio(OwnedBlock { owner }));
            } else {
                let slot = &shared.controls[index];
                if !slot.claim(ticket) {
                    continue;
                }
                if let Some(hook) = hook.take() {
                    hook();
                }
                let owner = slot.owner.take().expect("claimed control owner");
                self.pending.push(Item::Control((*owner).clone()));
            }
            if shared.epoch.load(SeqCst) != epoch {
                return None;
            }
        }
        if shared.epoch.load(SeqCst) != epoch || !self.pending.is_empty() {
            return None;
        }
        terminal_hook();
        let ended = shared.ended.load(SeqCst);
        let failure = failure_from_code(shared.failure.load(SeqCst));
        if shared.epoch.load(SeqCst) != epoch {
            return None;
        }
        if !self.terminal_read
            && let Some(failure) = failure
        {
            self.terminal_read = true;
            shared.consumer_done.store(true, SeqCst);
            return Some(Item::Failed(failure));
        }
        if ended {
            shared.consumer_done.store(true, SeqCst);
        }
        None
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.shared.ended.store(true, SeqCst);
        if self.shared.stop.load(SeqCst) == 2 {
            self.control_handle().discard_queued();
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.control_handle().stop(Stop::Abort);
        for item in self.pending.drain(..) {
            if let Item::Audio(block) = item {
                self.shared
                    .counters
                    .in_flight
                    .fetch_sub(block.header().frames as u64, SeqCst);
                self.shared
                    .counters
                    .discarded
                    .fetch_add(block.header().frames as u64, SeqCst);
            }
        }
        self.shared.consumer_done.store(true, SeqCst);
    }
}
