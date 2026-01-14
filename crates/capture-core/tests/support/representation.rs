#![allow(dead_code)]

use capture_system_or_process_audio_core::{
    CapacityError, lifecycle::CompletionResult, ordering, schema::*, storage::OwnedBlock,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainLossRecord {
    pub known_frames: u64,
    pub has_unknown: u32,
    pub rate_hz: u32,
    pub cause: u32,
}

impl From<ordering::DomainLoss> for DomainLossRecord {
    fn from(loss: ordering::DomainLoss) -> Self {
        use ordering::LossCause;
        Self {
            known_frames: loss.known_frames,
            has_unknown: u32::from(loss.unknown),
            rate_hz: loss.rate_hz,
            cause: match loss.cause {
                LossCause::Source => 0,
                LossCause::QueueOldest => 1,
                LossCause::QueueNewest => 2,
                LossCause::PoolExhaustion => 3,
                LossCause::Abort => 4,
                LossCause::Uncertain => 5,
            },
        }
    }
}

impl TryFrom<DomainLossRecord> for ordering::DomainLoss {
    type Error = Reason;
    fn try_from(record: DomainLossRecord) -> Result<Self, Reason> {
        use ordering::LossCause;
        if record.has_unknown > 1 || record.rate_hz == 0 {
            return Err(Reason::InvalidFormat);
        }
        let cause = match record.cause {
            0 => LossCause::Source,
            1 => LossCause::QueueOldest,
            2 => LossCause::QueueNewest,
            3 => LossCause::PoolExhaustion,
            4 => LossCause::Abort,
            5 => LossCause::Uncertain,
            _ => return Err(Reason::InvalidFormat),
        };
        Ok(Self {
            known_frames: record.known_frames,
            unknown: record.has_unknown != 0,
            rate_hz: record.rate_hz,
            cause,
        })
    }
}

pub fn completion_code(result: CompletionResult) -> u32 {
    match result {
        CompletionResult::Complete => 1,
        CompletionResult::Pending => 2,
        CompletionResult::Timeout => 3,
        CompletionResult::SameWorker => 4,
    }
}

pub fn completion_from_code(code: u32) -> Result<CompletionResult, Reason> {
    match code {
        1 => Ok(CompletionResult::Complete),
        2 => Ok(CompletionResult::Pending),
        3 => Ok(CompletionResult::Timeout),
        4 => Ok(CompletionResult::SameWorker),
        _ => Err(Reason::InvalidFormat),
    }
}

#[derive(Clone, Debug)]
pub struct EventRecord {
    pub kind: u32,
    pub audio: Option<OwnedBlock>,
    pub control: Option<ordering::Control>,
    pub failure: u32,
}

impl From<ordering::Item> for EventRecord {
    fn from(item: ordering::Item) -> Self {
        use ordering::{Failure, Item};
        match item {
            Item::Audio(audio) => Self {
                kind: 1,
                audio: Some(audio),
                control: None,
                failure: 0,
            },
            Item::Control(control) => Self {
                kind: 2,
                audio: None,
                control: Some(control),
                failure: 0,
            },
            Item::Failed(failure) => Self {
                kind: 3,
                audio: None,
                control: None,
                failure: match failure {
                    Failure::QueueOverflow => 1,
                    Failure::PoolExhaustion => 2,
                    Failure::PacketTooLarge => 3,
                    Failure::InvalidPacket => 4,
                    Failure::TicketExhausted => 5,
                    Failure::ControlExhausted => 6,
                    Failure::Backend => 7,
                    Failure::Callback => 8,
                    Failure::CallbackPanic => 9,
                },
            },
        }
    }
}

impl TryFrom<EventRecord> for ordering::Item {
    type Error = Reason;
    fn try_from(record: EventRecord) -> Result<Self, Reason> {
        use ordering::{Failure, Item};
        let map_fail = |code: u32| match code {
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
        };
        match (record.kind, record.audio, record.control, record.failure) {
            (1, Some(audio), None, 0) => Ok(Item::Audio(audio)),
            (2, None, Some(control), 0) => Ok(Item::Control(control)),
            (3, None, None, code) => Ok(Item::Failed(map_fail(code).ok_or(Reason::InvalidFormat)?)),
            _ => Err(Reason::InvalidFormat),
        }
    }
}

pub fn reason_code(reason: Reason) -> u32 {
    match reason {
        Reason::AmbiguousApplication => 1,
        Reason::StaleProcess => 2,
        Reason::InvalidFormat => 3,
        Reason::UnsupportedFormat => 4,
        Reason::IncompleteFrame => 5,
        Reason::Capacity(CapacityError::Zero) => 6,
        Reason::Capacity(CapacityError::Overflow) => 7,
        Reason::Capacity(CapacityError::ExceedsByteCeiling) => 8,
        Reason::NoCandidate => 9,
        Reason::Unknown => 10,
        Reason::PermissionRequired => 11,
        Reason::Unavailable => 12,
        Reason::Unsupported => 13,
        Reason::SelectedOpenFailed => 14,
        Reason::UnsupportedChannelMapping => 15,
        Reason::FormatChanged => 16,
    }
}

pub fn reason_from_code(code: u32) -> Option<Reason> {
    Some(match code {
        1 => Reason::AmbiguousApplication,
        2 => Reason::StaleProcess,
        3 => Reason::InvalidFormat,
        4 => Reason::UnsupportedFormat,
        5 => Reason::IncompleteFrame,
        6 => Reason::Capacity(CapacityError::Zero),
        7 => Reason::Capacity(CapacityError::Overflow),
        8 => Reason::Capacity(CapacityError::ExceedsByteCeiling),
        9 => Reason::NoCandidate,
        10 => Reason::Unknown,
        11 => Reason::PermissionRequired,
        12 => Reason::Unavailable,
        13 => Reason::Unsupported,
        14 => Reason::SelectedOpenFailed,
        15 => Reason::UnsupportedChannelMapping,
        16 => Reason::FormatChanged,
        _ => return None,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatRecord {
    pub sample: u32,
    pub byte_order: u32,
    pub rate_hz: u32,
    pub channels: u32,
    pub layout_known: u32,
    pub labels: Vec<u32>,
}

impl From<&Format> for FormatRecord {
    fn from(format: &Format) -> Self {
        Self {
            sample: match format.sample() {
                SampleType::I16 => 1,
                SampleType::I24 => 2,
                SampleType::F32 => 3,
                SampleType::I32 => 4,
            },
            byte_order: match format.order() {
                ByteOrder::Little => 1,
                ByteOrder::Big => 2,
            },
            rate_hz: format.rate(),
            channels: u32::from(format.channels()),
            layout_known: u32::from(matches!(format.layout(), Layout::Known(_))),
            labels: match format.layout() {
                Layout::Unknown => Vec::new(),
                Layout::Known(labels) => labels
                    .iter()
                    .map(|label| match label {
                        Channel::Mono => 1,
                        Channel::FrontLeft => 2,
                        Channel::FrontRight => 3,
                        Channel::Center => 4,
                        Channel::LowFrequency => 5,
                        Channel::RearLeft => 6,
                        Channel::RearRight => 7,
                    })
                    .collect(),
            },
        }
    }
}

impl TryFrom<FormatRecord> for Format {
    type Error = Reason;
    fn try_from(record: FormatRecord) -> Result<Self, Reason> {
        let sample = match record.sample {
            1 => SampleType::I16,
            2 => SampleType::I24,
            3 => SampleType::F32,
            4 => SampleType::I32,
            _ => return Err(Reason::InvalidFormat),
        };
        let order = match record.byte_order {
            1 => ByteOrder::Little,
            2 => ByteOrder::Big,
            _ => return Err(Reason::InvalidFormat),
        };
        let layout = match record.layout_known {
            0 if record.labels.is_empty() => Layout::Unknown,
            1 => Layout::Known(Arc::from(
                record
                    .labels
                    .iter()
                    .map(|label| match label {
                        1 => Ok(Channel::Mono),
                        2 => Ok(Channel::FrontLeft),
                        3 => Ok(Channel::FrontRight),
                        4 => Ok(Channel::Center),
                        5 => Ok(Channel::LowFrequency),
                        6 => Ok(Channel::RearLeft),
                        7 => Ok(Channel::RearRight),
                        _ => Err(Reason::InvalidFormat),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            _ => return Err(Reason::InvalidFormat),
        };
        Format::new(
            sample,
            order,
            record.rate_hz,
            u16::try_from(record.channels).map_err(|_| Reason::InvalidFormat)?,
            layout,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderRecord {
    pub sequence: u64,
    pub segment: u64,
    pub generation: u64,
    pub frames: u64,
    pub valid: u32,
    pub sample_position: u64,
    pub ticks: u64,
    pub ticks_per_second: u64,
    pub clock_domain: u64,
    pub provenance: u32,
    pub uncertainty_ns: u64,
}

impl TryFrom<Header> for HeaderRecord {
    type Error = Reason;
    fn try_from(header: Header) -> Result<Self, Reason> {
        let mut record = Self {
            sequence: header.sequence,
            segment: header.segment,
            generation: header.generation,
            frames: u64::try_from(header.frames).map_err(|_| Reason::InvalidFormat)?,
            valid: u32::from(header.sample_position.is_some()),
            sample_position: header.sample_position.unwrap_or(0),
            ticks: 0,
            ticks_per_second: 0,
            clock_domain: 0,
            provenance: 0,
            uncertainty_ns: 0,
        };
        if let Some(stamp) = header.timestamp {
            if stamp.ticks_per_second == 0 {
                return Err(Reason::InvalidFormat);
            }
            record.valid |= 2;
            record.ticks = stamp.ticks;
            record.ticks_per_second = stamp.ticks_per_second;
            record.clock_domain = stamp.clock_domain;
            record.provenance = match stamp.provenance {
                Provenance::Native => 1,
                Provenance::Derived => 2,
                Provenance::HostArrival => 3,
                Provenance::Synthetic => 4,
            };
            if let Some(uncertainty) = stamp.uncertainty_ns {
                record.valid |= 4;
                record.uncertainty_ns = uncertainty;
            }
        }
        Ok(record)
    }
}

impl TryFrom<HeaderRecord> for Header {
    type Error = Reason;
    fn try_from(record: HeaderRecord) -> Result<Self, Reason> {
        if record.valid & !7 != 0 || (record.valid & 4 != 0 && record.valid & 2 == 0) {
            return Err(Reason::InvalidFormat);
        }
        let timestamp = if record.valid & 2 != 0 {
            if record.ticks_per_second == 0 {
                return Err(Reason::InvalidFormat);
            }
            Some(Stamp {
                ticks: record.ticks,
                ticks_per_second: record.ticks_per_second,
                clock_domain: record.clock_domain,
                provenance: match record.provenance {
                    1 => Provenance::Native,
                    2 => Provenance::Derived,
                    3 => Provenance::HostArrival,
                    4 => Provenance::Synthetic,
                    _ => return Err(Reason::InvalidFormat),
                },
                uncertainty_ns: (record.valid & 4 != 0).then_some(record.uncertainty_ns),
            })
        } else {
            None
        };
        Ok(Header {
            sequence: record.sequence,
            segment: record.segment,
            generation: record.generation,
            frames: usize::try_from(record.frames).map_err(|_| Reason::InvalidFormat)?,
            sample_position: (record.valid & 1 != 0).then_some(record.sample_position),
            timestamp,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LossRecord {
    pub frames_known: u32,
    pub frames: u64,
    pub rate_hz: u32,
}

impl From<Loss> for LossRecord {
    fn from(loss: Loss) -> Self {
        Self {
            frames_known: u32::from(loss.frames.is_some()),
            frames: loss.frames.unwrap_or(0),
            rate_hz: loss.rate_hz,
        }
    }
}

impl TryFrom<LossRecord> for Loss {
    type Error = Reason;
    fn try_from(record: LossRecord) -> Result<Self, Reason> {
        if record.frames_known > 1 || record.rate_hz == 0 {
            return Err(Reason::InvalidFormat);
        }
        Ok(Self {
            frames: (record.frames_known == 1).then_some(record.frames),
            rate_hz: record.rate_hz,
        })
    }
}
