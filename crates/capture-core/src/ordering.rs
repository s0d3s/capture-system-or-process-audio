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
