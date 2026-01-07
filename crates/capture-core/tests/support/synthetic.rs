use super::{Trace, TraceItem, formats};
use capture_system_or_process_audio_core::schema::*;

pub fn packet(format: &Format, first: usize, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for sample in
        first * usize::from(format.channels())..(first + frames) * usize::from(format.channels())
    {
        match format.sample() {
            SampleType::I16 => {
                let value = [i16::MIN, -1, 0, i16::MAX][sample % 4];
                bytes.extend_from_slice(&match format.order() {
                    ByteOrder::Little => value.to_le_bytes(),
                    ByteOrder::Big => value.to_be_bytes(),
                });
            }
            SampleType::I24 => bytes
                .extend_from_slice(&[-8388608i32, -1, 0, 8388607][sample % 4].to_le_bytes()[..3]),
            SampleType::I32 => {
                let value = [i32::MIN, -1, 0, i32::MAX][sample % 4];
                bytes.extend_from_slice(&match format.order() {
                    ByteOrder::Little => value.to_le_bytes(),
                    ByteOrder::Big => value.to_be_bytes(),
                });
            }
            SampleType::F32 => {
                bytes.extend_from_slice(&[-1.0f32, -0.0, 0.0, 1.0][sample % 4].to_le_bytes())
            }
        }
    }
    bytes
}

pub fn acquire(initial: usize, trace: Trace) -> Vec<TraceItem> {
    let formats = formats();
    let mut format = formats[if matches!(trace, Trace::Generation) {
        0
    } else {
        initial
    }]
    .clone();
    let mut generation = 1;
    let mut segment = 1;
    let mut sequence = 0;
    let mut items = vec![TraceItem::Format {
        sequence,
        generation,
        segment,
        format: format.clone(),
    }];
    let mut position = 0;
    let mut packet_index = 0;
    while position < 4096 {
        if position == 2076 {
            match trace {
                Trace::KnownGap | Trace::UnknownGap => {
                    segment += 1;
                    sequence += 1;
                    items.push(TraceItem::Gap {
                        sequence,
                        segment,
                        loss: Loss {
                            frames: matches!(trace, Trace::KnownGap).then_some(7),
                            rate_hz: format.rate(),
                        },
                    });
                }
                Trace::Generation => {
                    generation += 1;
                    segment += 1;
                    format = formats[1].clone();
                    sequence += 1;
                    items.push(TraceItem::Format {
                        sequence,
                        generation,
                        segment,
                        format: format.clone(),
                    });
                }
                _ => {}
            }
        }
        let frames = [1, 7, 64, 255, 256][packet_index % 5].min(4096 - position);
        sequence += 1;
        let header = Header {
            sequence,
            segment,
            generation,
            frames,
            sample_position: Some(position as u64),
            timestamp: Some(Stamp {
                ticks: 1_000_000 + position as u64 * 10_000,
                ticks_per_second: 1_000_000_000,
                clock_domain: generation,
                provenance: Provenance::Synthetic,
                uncertainty_ns: Some(0),
            }),
        };
        items.push(TraceItem::Pcm {
            header,
            format: format.clone(),
            bytes: packet(&format, position, frames),
        });
        position += frames;
        packet_index += 1;
    }
    sequence += 1;
    items.push(TraceItem::Terminal {
        sequence,
        failure: matches!(trace, Trace::Failure).then_some(Reason::Unavailable),
    });
    items
}
