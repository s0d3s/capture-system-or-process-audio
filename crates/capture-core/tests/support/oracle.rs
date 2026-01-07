use super::{Trace, TraceItem};
use capture_system_or_process_audio_core::schema::*;

pub const PCM_PERIODS: [&[u8]; 4] = [
    &[0, 128, 255, 255, 0, 0, 255, 127],
    &[0, 0, 128, 191, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 128, 63],
    &[
        0, 0, 128, 255, 255, 255, 0, 0, 0, 255, 255, 127, 0, 0, 128, 255, 255, 255, 0, 0, 0, 255,
        255, 127, 0, 0, 128, 255, 255, 255, 0, 0, 0, 255, 255, 127,
    ],
    &[128, 0, 255, 255, 0, 0, 127, 255],
];

const CONTINUOUS: &[usize] = &[
    1, 7, 64, 255, 256, 1, 7, 64, 255, 256, 1, 7, 64, 255, 256, 1, 7, 64, 255, 256, 1, 7, 64, 255,
    256, 1, 7, 64, 255, 256, 1, 7, 64, 255, 256, 1, 7, 7,
];
pub fn verify(initial: usize, trace: Trace, items: &[TraceItem]) -> Result<(), &'static str> {
    let transition = matches!(
        trace,
        Trace::KnownGap | Trace::UnknownGap | Trace::Generation
    );
    let sizes = CONTINUOUS;
    let mut position = 0;
    let mut packet_index = 0;
    let mut controls = 0;
    let mut terminal = false;
    for (sequence, item) in items.iter().enumerate() {
        let after = transition && position >= 2076 && controls == 2;
        let identity = if matches!(trace, Trace::Generation) {
            usize::from(after)
        } else {
            initial
        };
        let generation = if matches!(trace, Trace::Generation) && after {
            2
        } else {
            1
        };
        let segment = if after { 2 } else { 1 };
        match item {
            TraceItem::Format {
                sequence: actual,
                generation: actual_generation,
                segment: actual_segment,
                format,
            } => {
                let expected_identity = if matches!(trace, Trace::Generation) && position == 2076 {
                    1
                } else if matches!(trace, Trace::Generation) {
                    0
                } else {
                    initial
                };
                let expected_generation = if position == 2076 { 2 } else { 1 };
                if terminal
                    || *actual != sequence as u64
                    || !((sequence == 0 && position == 0)
                        || (matches!(trace, Trace::Generation)
                            && position == 2076
                            && controls == 1))
                    || *actual_generation != expected_generation
                    || *actual_segment != expected_generation
                    || !format_matches(expected_identity, format)
                {
                    return Err("format boundary");
                }
                controls += 1;
            }
            TraceItem::Gap {
                sequence: actual,
                segment: actual_segment,
                loss,
            } => {
                if terminal
                    || !matches!(trace, Trace::KnownGap | Trace::UnknownGap)
                    || position != 2076
                    || controls != 1
                    || *actual != sequence as u64
                    || *actual_segment != 2
                    || loss.rate_hz != [48000, 44100, 96000, 48000][initial]
                    || loss.frames != matches!(trace, Trace::KnownGap).then_some(7)
                {
                    return Err("gap");
                }
                controls += 1;
            }
            TraceItem::Pcm {
                header,
                format,
                bytes,
            } => {
                if terminal || controls == 0 || (transition && position == 2076 && controls != 2) {
                    return Err("missing boundary");
                }
                let Some(&frames) = sizes.get(packet_index) else {
                    return Err("extra packet");
                };
                if header.sequence != sequence as u64
                    || header.segment != segment
                    || header.generation != generation
                    || header.frames != frames
                    || header.sample_position != Some(position as u64)
                    || !format_matches(identity, format)
                {
                    return Err("PCM metadata");
                }
                let expected_stamp = Stamp {
                    ticks: 1_000_000 + position as u64 * 10_000,
                    ticks_per_second: 1_000_000_000,
                    clock_domain: generation,
                    provenance: Provenance::Synthetic,
                    uncertainty_ns: Some(0),
                };
                if header.timestamp != Some(expected_stamp) {
                    return Err("timestamp");
                }
                let stride = [4, 4, 18, 4][identity];
                let period = PCM_PERIODS[identity];
                if bytes.len() != frames * stride
                    || bytes.iter().enumerate().any(|(offset, byte)| {
                        *byte != period[(position * stride + offset) % period.len()]
                    })
                {
                    return Err("PCM content");
                }
                position += frames;
                packet_index += 1;
            }
            TraceItem::Terminal {
                sequence: actual,
                failure,
            } => {
                if terminal
                    || position != 4096
                    || sequence + 1 != items.len()
                    || *actual != sequence as u64
                    || *failure != matches!(trace, Trace::Failure).then_some(Reason::Unavailable)
                {
                    return Err("terminal");
                }
                terminal = true;
            }
        }
    }
    if !terminal
        || position != 4096
        || packet_index != sizes.len()
        || controls != if transition { 2 } else { 1 }
    {
        return Err("coverage");
    }
    Ok(())
}

fn format_matches(identity: usize, format: &Format) -> bool {
    let layouts: [&[Channel]; 4] = [
        &[Channel::FrontLeft, Channel::FrontRight],
        &[Channel::Mono],
        &[
            Channel::FrontLeft,
            Channel::FrontRight,
            Channel::Center,
            Channel::LowFrequency,
            Channel::RearLeft,
            Channel::RearRight,
        ],
        &[],
    ];
    format.sample()
        == [
            SampleType::I16,
            SampleType::F32,
            SampleType::I24,
            SampleType::I16,
        ][identity]
        && format.order()
            == if identity == 3 {
                ByteOrder::Big
            } else {
                ByteOrder::Little
            }
        && format.rate() == [48000, 44100, 96000, 48000][identity]
        && format.channels() == [2, 1, 6, 2][identity]
        && match format.layout() {
            Layout::Unknown => identity == 3,
            Layout::Known(labels) => labels.as_ref() == layouts[identity],
        }
}
