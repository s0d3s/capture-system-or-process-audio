pub mod oracle;
pub mod representation;
pub mod synthetic;

use capture_system_or_process_audio_core::schema::*;
use std::sync::Arc;

pub fn formats() -> [Arc<Format>; 4] {
    [
        Format::new(
            SampleType::I16,
            ByteOrder::Little,
            48000,
            2,
            Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
        )
        .unwrap(),
        Format::new(
            SampleType::F32,
            ByteOrder::Little,
            44100,
            1,
            Layout::Known(Arc::from([Channel::Mono])),
        )
        .unwrap(),
        Format::new(
            SampleType::I24,
            ByteOrder::Little,
            96000,
            6,
            Layout::Known(Arc::from([
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::Center,
                Channel::LowFrequency,
                Channel::RearLeft,
                Channel::RearRight,
            ])),
        )
        .unwrap(),
        Format::new(SampleType::I16, ByteOrder::Big, 48000, 2, Layout::Unknown).unwrap(),
    ]
    .map(Arc::new)
}

#[derive(Clone, Debug, PartialEq)]
pub enum TraceItem {
    Format {
        sequence: u64,
        generation: u64,
        segment: u64,
        format: Arc<Format>,
    },
    Pcm {
        header: Header,
        format: Arc<Format>,
        bytes: Vec<u8>,
    },
    Gap {
        sequence: u64,
        segment: u64,
        loss: Loss,
    },
    Terminal {
        sequence: u64,
        failure: Option<Reason>,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Trace {
    Continuous,
    KnownGap,
    UnknownGap,
    Generation,
    Failure,
}

pub const TRACES: [Trace; 5] = [
    Trace::Continuous,
    Trace::KnownGap,
    Trace::UnknownGap,
    Trace::Generation,
    Trace::Failure,
];

pub fn acquisition_gate(
    allocations: u64,
    copied_bytes: u64,
    destinations: u64,
    packet_bytes: u64,
    user_calls: u64,
) -> bool {
    allocations == 0 && copied_bytes <= packet_bytes && destinations <= 1 && user_calls == 0
}
