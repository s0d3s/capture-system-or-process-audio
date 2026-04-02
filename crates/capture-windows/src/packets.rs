use crate::core::{
    ordering::{Failure, Offer, Overflow, Producer},
    schema::{Header, Loss, Provenance, Stamp},
};

pub(crate) struct Packet<'a> {
    pub bytes: &'a [u8],
    pub silent: bool,
    pub frames: usize,
    pub discontinuity: bool,
    pub timestamp_error: bool,
    pub position: u64,
    pub qpc_100ns: u64,
}

pub(crate) struct Packets {
    sequence: u64,
    segment: u64,
    previous_end: Option<u64>,
    device_positions: bool,
    clock_domain: u64,
}

impl Packets {
    #[cfg(test)]
    pub fn new() -> Self {
        Self {
            sequence: 0,
            segment: 1,
            previous_end: None,
            device_positions: true,
            clock_domain: 1,
        }
    }

    pub fn with_clock_domain(clock_domain: u64, device_positions: bool) -> Self {
        Self {
            sequence: 0,
            segment: 1,
            previous_end: None,
            device_positions,
            clock_domain,
        }
    }

    #[cfg(test)]
    pub fn without_device_positions() -> Self {
        Self {
            device_positions: false,
            ..Self::new()
        }
    }

    pub fn publish(
        &mut self,
        producer: &mut Producer,
        packet: Packet<'_>,
        policy: Overflow,
    ) -> Offer {
        self.publish_observed(producer, packet, policy, None)
    }

    pub fn publish_observed(
        &mut self,
        producer: &mut Producer,
        packet: Packet<'_>,
        policy: Overflow,
        acquired: Option<std::time::Instant>,
    ) -> Offer {
        if self.sequence >= u64::MAX - 3 || self.segment == u64::MAX {
            producer.fail_capture(Failure::TicketExhausted);
            return Offer::Failed(Failure::TicketExhausted);
        }
        if packet.discontinuity
            || (!packet.timestamp_error
                && self.previous_end.is_some_and(|end| end != packet.position))
        {
            self.sequence += 1;
            self.segment += 1;
            let result = producer.gap(
                self.sequence,
                self.segment,
                Loss {
                    frames: None,
                    rate_hz: producer.pool().format().rate(),
                },
            );
            if let Err(error) = result {
                producer.fail_capture(error);
                return Offer::Failed(error);
            }
        }
        self.sequence += 1;
        if let Some(acquired) = acquired
            && let Some(trace) = producer.trace()
        {
            trace.record_at(
                crate::core::measurements::Stage::Acquired,
                self.sequence,
                acquired,
            );
        }
        self.previous_end = if packet.timestamp_error || !self.device_positions {
            None
        } else {
            packet.position.checked_add(packet.frames as u64)
        };
        let header = Header {
            sequence: self.sequence,
            segment: self.segment,
            generation: producer.pool().generation(),
            frames: packet.frames,
            sample_position: (!packet.timestamp_error && self.device_positions)
                .then_some(packet.position),
            timestamp: (!packet.timestamp_error && packet.qpc_100ns != 0).then_some(Stamp {
                ticks: packet.qpc_100ns,
                ticks_per_second: 10_000_000,
                clock_domain: self.clock_domain,
                provenance: Provenance::Native,
                uncertainty_ns: None,
            }),
        };
        if packet.silent {
            producer.offer_silence(header, policy)
        } else {
            producer.offer(packet.bytes, header, policy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        StorageLimits,
        ordering::{Item, channel},
        schema::{ByteOrder, Format, Layout, SampleType},
        storage::Pool,
    };
    use std::sync::Arc;

    #[test]
    fn unavailable_device_position_keeps_qpc_without_false_gaps() {
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
        let pool = Pool::new(StorageLimits::new(8, 64, 8, 4096).unwrap(), 64, format).unwrap();
        let (mut producer, mut reader) = channel(pool, 4).unwrap();
        let mut adapter = Packets::without_device_positions();
        for ticks in [100, 200] {
            assert_eq!(
                adapter.publish(
                    &mut producer,
                    Packet {
                        bytes: &[],
                        silent: true,
                        frames: 64,
                        discontinuity: false,
                        timestamp_error: false,
                        position: 0,
                        qpc_100ns: ticks
                    },
                    Overflow::Fail
                ),
                Offer::Queued
            );
            let Some(Item::Audio(block)) = reader.next_item() else {
                panic!("false gap before process audio")
            };
            assert_eq!(block.header().sample_position, None);
            assert_eq!(block.header().timestamp.unwrap().ticks, ticks);
        }
    }

    #[test]
    fn packet_flags_ordering_and_limits() {
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
        let (mut producer, mut reader) = channel(
            Pool::new(
                StorageLimits::new(8, 4096, 8, 262144).unwrap(),
                4096,
                format,
            )
            .unwrap(),
            4,
        )
        .unwrap();
        let mut adapter = Packets::new();
        let silent = [0; 512];
        assert_eq!(
            adapter.publish(
                &mut producer,
                Packet {
                    bytes: &[],
                    silent: true,
                    frames: 64,
                    discontinuity: true,
                    timestamp_error: false,
                    position: 0,
                    qpc_100ns: 100
                },
                Overflow::DropOldest
            ),
            Offer::Queued
        );
        assert!(matches!(reader.next_item(), Some(Item::Control(_))));
        let Some(Item::Audio(block)) = reader.next_item() else {
            panic!("missing silent packet")
        };
        assert_eq!(block.bytes(), silent);
        assert_eq!(block.header().timestamp.unwrap().ticks, 100);
        assert_eq!(
            adapter.publish(
                &mut producer,
                Packet {
                    bytes: &silent,
                    silent: false,
                    frames: 64,
                    discontinuity: false,
                    timestamp_error: true,
                    position: 999,
                    qpc_100ns: 999
                },
                Overflow::DropOldest
            ),
            Offer::Queued
        );
        let Some(Item::Audio(invalid_stamp)) = reader.next_item() else {
            panic!("missing packet")
        };
        assert_eq!(invalid_stamp.header().timestamp, None);
        assert_eq!(invalid_stamp.header().sample_position, None);
        assert_eq!(
            adapter.publish(
                &mut producer,
                Packet {
                    bytes: &[],
                    silent: true,
                    frames: 4097,
                    discontinuity: false,
                    timestamp_error: true,
                    position: 0,
                    qpc_100ns: 0
                },
                Overflow::DropOldest
            ),
            Offer::Failed(Failure::PacketTooLarge)
        );
        assert!(matches!(
            reader.next_item(),
            Some(Item::Failed(Failure::PacketTooLarge))
        ));
        assert!(reader.next_item().is_none());
        assert!(reader.ended());
        assert_eq!(block.bytes(), silent);
    }

    #[test]
    fn silence_pressure_and_retention() {
        for policy in [Overflow::DropOldest, Overflow::DropNewest, Overflow::Fail] {
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
            let pool = Pool::new(StorageLimits::new(8, 64, 8, 4096).unwrap(), 64, format).unwrap();
            let (mut producer, mut reader) = channel(pool, 4).unwrap();
            let mut adapter = Packets::new();
            let bytes = [1; 512];
            assert_eq!(
                adapter.publish(
                    &mut producer,
                    Packet {
                        bytes: &bytes,
                        silent: false,
                        frames: 64,
                        discontinuity: false,
                        timestamp_error: true,
                        position: 0,
                        qpc_100ns: 0
                    },
                    policy
                ),
                Offer::Queued
            );
            let Some(Item::Audio(retained)) = reader.next_item() else {
                panic!("missing retained packet")
            };
            for index in 0..5 {
                let outcome = adapter.publish(
                    &mut producer,
                    Packet {
                        bytes: &[],
                        silent: true,
                        frames: 64,
                        discontinuity: false,
                        timestamp_error: true,
                        position: 0,
                        qpc_100ns: 0,
                    },
                    policy,
                );
                let expected = if index < 4 || policy == Overflow::DropOldest {
                    Offer::Queued
                } else if policy == Overflow::DropNewest {
                    Offer::QueueDropped
                } else {
                    Offer::Failed(Failure::QueueOverflow)
                };
                assert_eq!(outcome, expected);
                assert_eq!(retained.bytes(), bytes);
            }
            producer.close();
            let mut audio = 0;
            while let Some(item) = reader.next_item() {
                if let Item::Audio(block) = item {
                    assert_eq!(block.bytes(), &[0; 512]);
                    audio += 1;
                }
            }
            assert_eq!(audio, 4);
            assert_eq!(
                producer.pool().limits().slots() * producer.pool().limits().bytes_per_slot(),
                4096
            );
        }
    }
}
