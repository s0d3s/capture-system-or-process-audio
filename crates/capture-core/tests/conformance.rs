#![forbid(unsafe_code)]

use capture_system_or_process_audio_core::{CapacityError, StorageLimits};
use std::sync::Arc;

mod support;

#[test]
#[cfg(feature = "test-hooks")]
fn integrated_abort_during_publication() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, schema::Header, storage::Pool,
    };
    use std::sync::mpsc;
    use std::time::Duration;
    for _ in 0..100 {
        for check_terminal in [false, true] {
            let started = std::time::Instant::now();
            let format = support::formats()[0].clone();
            let (mut producer, mut reader) = channel(
                Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
                4,
            )
            .unwrap();
            let control = producer.control_handle();
            let (start, begin) = mpsc::sync_channel(1);
            let (entered, seen) = mpsc::sync_channel(1);
            let (release, gate) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                begin.recv_timeout(Duration::from_secs(1)).unwrap();
                assert_eq!(
                    producer.offer_with_publish_hook(
                        &[7; 256],
                        Header {
                            sequence: 1,
                            segment: 1,
                            generation: 1,
                            frames: 64,
                            sample_position: None,
                            timestamp: None,
                        },
                        Overflow::DropOldest,
                        || {
                            entered.send(()).unwrap();
                            gate.recv_timeout(Duration::from_secs(1)).unwrap();
                        },
                    ),
                    Offer::Queued
                );
            });
            let schedule = || {
                start.send(()).unwrap();
                seen.recv_timeout(Duration::from_secs(1)).unwrap();
                control.stop(Stop::Drain);
            };
            let ended = if check_terminal {
                assert!(reader.next_item_with_terminal_hook(schedule).is_none());
                control.delivery_done()
            } else {
                reader.ended_with_hook(schedule)
            };
            release.send(()).unwrap();
            worker.join().unwrap();
            assert!(!ended, "EOF observed before accepted packet publication");
            assert!(
                !control.delivery_done(),
                "delivery completion precedes accepted tail"
            );
            assert!(matches!(reader.next_item(), Some(Item::Audio(_))));
            assert!(reader.next_item().is_none());
            assert!(reader.ended());
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        let format = support::formats()[0].clone();
        let (mut producer, reader) = channel(
            Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
            4,
        )
        .unwrap();
        let control = producer.control_handle();
        let completion = Completion::pull(control.clone());
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let outcome = producer.offer_with_publish_hook(
                &[7; 256],
                Header {
                    sequence: 1,
                    segment: 1,
                    generation: 1,
                    frames: 64,
                    sample_position: None,
                    timestamp: None,
                },
                Overflow::DropOldest,
                || {
                    entered.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(1)).unwrap();
                },
            );
            assert_eq!(outcome, Offer::Queued);
        });
        seen.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(reader);
        assert_eq!(completion.poll(1000000, 1000000), CompletionResult::Timeout);
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(completion.poll(0, 1000000), CompletionResult::Complete);
        let state = control.snapshot();
        assert_eq!(state.queued, 0);
        assert_eq!(state.discarded, 64);
        assert_eq!(state.input, state.discarded);
        assert!(state.stable);
    }
    println!(
        "integrated publication schedules: abort=100, EOF=100, completion=100; orphan queue owners=0"
    );
}

#[test]
fn integrated_pressure_cancel_generation_failure() {
    use capture_system_or_process_audio_core::{
        lifecycle::*,
        ordering::*,
        reads::*,
        schema::{Header, Loss},
        storage::Pool,
    };
    let formats = support::formats();
    for _ in 0..100 {
        for cancel_first in [false, true] {
            for abort_first in [false, true] {
                let started = std::time::Instant::now();
                let (mut producer, mut reader) = channel(
                    Pool::new(
                        StorageLimits::new(8, 256, 18, 36864).unwrap(),
                        256,
                        formats[0].clone(),
                    )
                    .unwrap(),
                    4,
                )
                .unwrap();
                let control = producer.control_handle();
                let completion = Completion::pull(control.clone());
                let header = Header {
                    sequence: 1,
                    segment: 1,
                    generation: 1,
                    frames: 64,
                    sample_position: None,
                    timestamp: None,
                };
                for sequence in 1..=4 {
                    producer.offer(
                        &[sequence as u8; 256],
                        Header { sequence, ..header },
                        Overflow::DropOldest,
                    );
                }
                let held = match reader.next_item().unwrap() {
                    Item::Audio(block) => block,
                    _ => unreachable!(),
                };
                let view = held.view();
                for sequence in 5..=6 {
                    producer.offer(
                        &[sequence as u8; 256],
                        Header { sequence, ..header },
                        Overflow::DropOldest,
                    );
                }
                producer
                    .gap(
                        7,
                        2,
                        Loss {
                            frames: Some(7),
                            rate_hz: 48000,
                        },
                    )
                    .unwrap();
                let mut sequences = Vec::new();
                let mut losses = Vec::new();
                while let Some(item) = reader.next_item() {
                    match item {
                        Item::Audio(block) => sequences.push(block.header().sequence),
                        Item::Control(control) => {
                            losses.extend_from_slice(control.losses.entries())
                        }
                        _ => unreachable!(),
                    }
                }
                assert_eq!(sequences, [3, 4, 5, 6]);
                assert_eq!(losses.len(), 2);
                assert_eq!(
                    (losses[0].cause, losses[0].known_frames),
                    (LossCause::QueueOldest, 64)
                );
                assert_eq!(
                    (losses[1].cause, losses[1].known_frames),
                    (LossCause::Source, 7)
                );
                let stream = ReadStream::new(reader);
                let cancel = Cancellation::default();
                producer.offer(
                    &[8; 68],
                    Header {
                        sequence: 8,
                        segment: 2,
                        frames: 17,
                        ..header
                    },
                    Overflow::DropOldest,
                );
                let mut operation = stream
                    .begin(Mode::Count, 64, true, None, cancel.clone())
                    .unwrap();
                if !cancel_first {
                    assert!(operation.poll(0).is_none());
                }
                cancel.cancel();
                let result = operation.poll(0).unwrap();
                assert_eq!(result.reason, PartialReason::Cancelled);
                assert_eq!(result.frames, if cancel_first { 0 } else { 17 });
                drop(operation);
                if cancel_first {
                    let mut read = stream
                        .begin(Mode::Count, 17, false, None, Cancellation::default())
                        .unwrap();
                    assert_eq!(read.poll(0).unwrap().frames, 17);
                }
                drop(held);
                assert_eq!(view.bytes(), &[1; 256]);
                assert_eq!(
                    producer
                        .prepare_format(9, 3, formats[1].clone(), 4)
                        .unwrap(),
                    2
                );
                let mut boundary = stream
                    .begin(Mode::Variable, 256, false, None, Cancellation::default())
                    .unwrap();
                assert_eq!(boundary.poll(0).unwrap().reason, PartialReason::Boundary);
                drop(boundary);
                for sequence in 10..=12 {
                    producer.offer(
                        &[10; 256],
                        Header {
                            sequence,
                            generation: 2,
                            segment: 3,
                            ..header
                        },
                        Overflow::DropOldest,
                    );
                }
                let before = control.snapshot();
                assert_eq!(
                    before.queued_durations[0],
                    FrameDuration {
                        frames: 192,
                        rate_hz: 44100
                    }
                );
                producer.fail_capture(Failure::Backend);
                assert_eq!(control.snapshot().failure, Some(Failure::Backend));
                if abort_first {
                    control.stop(Stop::Abort);
                }
                drop(producer);
                let mut tail = stream
                    .begin(Mode::Count, 256, false, None, Cancellation::default())
                    .unwrap();
                let result = tail.poll(0).unwrap();
                assert_eq!(result.reason, PartialReason::Failure(Failure::Backend));
                assert_eq!(result.frames, if abort_first { 0 } else { 192 });
                drop(tail);
                control.stop(Stop::Abort);
                assert_eq!(completion.poll(0, 1000000), CompletionResult::Complete);
                let state = control.snapshot();
                assert!(state.stable);
                assert_eq!(state.input, 593);
                assert_eq!(state.queue_lost, 64);
                assert_eq!(state.discarded, if abort_first { 192 } else { 0 });
                assert_eq!(state.delivered, if abort_first { 337 } else { 529 });
                assert_eq!(
                    state.input,
                    state.delivered + state.queue_lost + state.discarded
                );
                assert_eq!(state.queued + state.in_flight, 0);
                drop(stream);
                drop(control);
                drop(completion);
                assert_eq!(view.bytes(), &[1; 256]);
                assert_eq!(view.format(), formats[0].as_ref());
                assert!(started.elapsed() < std::time::Duration::from_secs(2));
            }
        }
    }
    println!(
        "integrated cases=400; input=593; queue_loss=64; delivered=337/529; abort_discard=192/0; retained bytes exact"
    );
}

#[test]
fn lifecycle_representations_and_oracle_negatives() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, reads::*, schema::Header, storage::Pool,
    };
    use support::representation::*;
    let format = support::formats()[0].clone();
    let (mut producer, reader) = channel(
        Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
        4,
    )
    .unwrap();
    let control = producer.control_handle();
    let stream = ReadStream::new(reader);
    let cancel = Cancellation::default();
    let mut operation = stream
        .begin(Mode::Count, 64, true, None, cancel.clone())
        .unwrap();
    producer.offer(
        &[7; 68],
        Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 17,
            sample_position: None,
            timestamp: None,
        },
        Overflow::DropOldest,
    );
    assert!(operation.poll(0).is_none());
    cancel.cancel();
    assert_eq!(operation.poll(0).unwrap().frames, 17);
    drop(operation);
    control.fail(Failure::Backend);
    let completion = Completion::pull(control.clone());
    let valid = |delivered: u64,
                 pending: bool,
                 allocations: u64,
                 completed: bool,
                 failed: bool,
                 later_calls: u64| {
        delivered == 17 && !pending && allocations == 0 && !completed && failed && later_calls == 0
    };
    let state = control.snapshot();
    let pending = stream.is_pending();
    let completed = completion.poll(0, 1000000) == CompletionResult::Complete;
    let failed = state.failure.is_some();
    assert!(valid(state.delivered, pending, 0, completed, failed, 0));
    assert!(!valid(18, pending, 0, completed, failed, 0));
    assert!(!valid(state.delivered, true, 0, completed, failed, 0));
    let injected = allocation_counter::measure(|| {
        std::hint::black_box(vec![0u8; 64]);
    });
    assert!(!valid(
        state.delivered,
        pending,
        injected.count_total,
        completed,
        failed,
        0
    ));
    assert!(!valid(state.delivered, pending, 0, true, failed, 0));
    assert!(!valid(state.delivered, pending, 0, completed, false, 0));
    assert!(!valid(state.delivered, pending, 0, completed, failed, 1));
    for cause in [
        LossCause::Source,
        LossCause::QueueOldest,
        LossCause::QueueNewest,
        LossCause::PoolExhaustion,
        LossCause::Abort,
        LossCause::Uncertain,
    ] {
        let loss = DomainLoss {
            known_frames: 7,
            unknown: true,
            rate_hz: 44100,
            cause,
        };
        assert_eq!(
            DomainLoss::try_from(DomainLossRecord::from(loss)).unwrap(),
            loss
        );
        assert!(
            DomainLoss::try_from(DomainLossRecord {
                cause: 99,
                ..DomainLossRecord::from(loss)
            })
            .is_err()
        );
    }
    for result in [
        CompletionResult::Complete,
        CompletionResult::Pending,
        CompletionResult::Timeout,
        CompletionResult::SameWorker,
    ] {
        assert_eq!(completion_from_code(completion_code(result)), Ok(result));
    }
    assert!(completion_from_code(99).is_err());
    println!(
        "reads/lifecycle oracle negatives=6; loss causes=6; known+unknown preserved; completion mappings=4"
    );
}

#[test]
fn lifecycle_subpreferred_frame_readiness() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, schema::Header, storage::Pool,
    };
    use std::sync::mpsc;
    use std::time::Duration;
    for _ in 0..100 {
        let format = support::formats()[0].clone();
        let (mut producer, reader) = channel(
            Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
            4,
        )
        .unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let stream = CallbackStream::spawn(reader, move |delivery, context| {
            if let Delivery::Item(Item::Audio(block)) = delivery {
                send.send(block.view()).unwrap();
                context.control.stop(Stop::Drain);
            }
            Ok(())
        })
        .unwrap();
        producer.offer(
            &[7; 4],
            Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 1,
                sample_position: None,
                timestamp: None,
            },
            Overflow::DropOldest,
        );
        let block = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(block.header().frames, 1);
        drop(producer);
        assert_eq!(
            stream.completion().wait(Duration::from_secs(1)),
            CompletionResult::Complete
        );
        assert_eq!(block.bytes(), &[7; 4]);
    }
}

#[test]
fn lifecycle_repeated_stop_and_failure() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, reads::*, schema::Header, storage::Pool,
    };
    let started = std::time::Instant::now();
    for _ in 0..100 {
        for format in support::formats() {
            for stops in [
                [Stop::Drain, Stop::Drain],
                [Stop::Abort, Stop::Abort],
                [Stop::Drain, Stop::Abort],
                [Stop::Abort, Stop::Drain],
            ] {
                for failure_first in [false, true] {
                    let (mut producer, reader) = channel(
                        Pool::new(
                            StorageLimits::new(8, 256, 18, 36864).unwrap(),
                            256,
                            format.clone(),
                        )
                        .unwrap(),
                        4,
                    )
                    .unwrap();
                    let control = producer.control_handle();
                    let completion = Completion::pull(control.clone());
                    let bytes = vec![7; format.byte_count(64).unwrap()];
                    for sequence in 1..=3 {
                        producer.offer(
                            &bytes,
                            Header {
                                sequence,
                                segment: 1,
                                generation: 1,
                                frames: 64,
                                sample_position: None,
                                timestamp: None,
                            },
                            Overflow::DropOldest,
                        );
                    }
                    if failure_first {
                        producer.fail_capture(Failure::Backend);
                    }
                    for stop in stops {
                        control.stop(stop);
                    }
                    if !failure_first {
                        producer.fail_capture(Failure::Backend);
                    }
                    assert_eq!(control.snapshot().failure, Some(Failure::Backend));
                    drop(producer);
                    assert_eq!(completion.poll(1000000, 1000000), CompletionResult::Timeout);
                    let stream = ReadStream::new(reader);
                    let mut operation = stream
                        .begin(Mode::Count, 256, false, None, Cancellation::default())
                        .unwrap();
                    let result = operation.poll(0).unwrap();
                    assert_eq!(
                        result.frames,
                        if stops.contains(&Stop::Abort) { 0 } else { 192 }
                    );
                    assert_eq!(result.reason, PartialReason::Failure(Failure::Backend));
                    drop(operation);
                    let state = control.snapshot();
                    assert_eq!(state.input, state.delivered + state.discarded);
                    assert_eq!(state.queued + state.in_flight, 0);
                    assert_eq!(completion.poll(0, 1000000), CompletionResult::Complete);
                }
            }
            let (mut producer, mut reader) = channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap();
            let bytes = vec![13; format.byte_count(64).unwrap()];
            for sequence in 1..=2 {
                producer.offer(
                    &bytes,
                    Header {
                        sequence,
                        segment: 1,
                        generation: 1,
                        frames: 64,
                        sample_position: None,
                        timestamp: None,
                    },
                    Overflow::DropOldest,
                );
            }
            producer.reject_uncertain(3, 64).unwrap();
            let control = producer.control_handle();
            drop(producer);
            let mut held = None;
            let mut frames = 0;
            let mut uncertain = 0;
            while let Some(item) = reader.next_item() {
                match item {
                    Item::Audio(block) => {
                        frames += block.header().frames;
                        held = Some(block.view());
                    }
                    Item::Control(control) => {
                        let loss = control.losses.entries()[0];
                        assert_eq!(loss.cause, LossCause::Uncertain);
                        uncertain += loss.known_frames;
                    }
                    Item::Failed(failure) => assert_eq!(failure, Failure::Backend),
                }
            }
            drop(reader);
            assert_eq!(frames, 128);
            assert_eq!(uncertain, 64);
            assert_eq!(control.snapshot().discarded, 64);
            assert_eq!(held.unwrap().bytes(), bytes);
        }
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    println!(
        "lifecycle stop/failure schedules=100; formats=4; uncertain delivered=128 discarded=64; failure preserved"
    );
}

#[test]
fn lifecycle_callback_error_and_panic() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, schema::Header, storage::Pool,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    };
    use std::time::{Duration, Instant};
    for _ in 0..100 {
        for panic in [false, true] {
            let started = Instant::now();
            let format = support::formats()[0].clone();
            let (mut producer, reader) = channel(
                Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
                4,
            )
            .unwrap();
            for sequence in 1..=3 {
                producer.offer(
                    &[7; 256],
                    Header {
                        sequence,
                        segment: 1,
                        generation: 1,
                        frames: 64,
                        sample_position: None,
                        timestamp: None,
                    },
                    Overflow::DropOldest,
                );
            }
            drop(producer);
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let stream = CallbackStream::spawn(reader, move |_, _| {
                observed.fetch_add(1, SeqCst);
                if panic {
                    panic!("deliberate callback unwind");
                }
                Err(CallbackError::Failed)
            })
            .unwrap();
            let completion = stream.completion();
            assert_eq!(
                completion.wait(Duration::from_secs(1)),
                CompletionResult::Complete
            );
            assert_eq!(calls.load(SeqCst), 1);
            assert_eq!(
                completion.callback_failure(),
                Some(if panic {
                    Failure::CallbackPanic
                } else {
                    Failure::Callback
                })
            );
            let state = stream.control_handle().snapshot();
            assert_eq!(state.delivered, 64);
            assert_eq!(state.discarded, 128);
            assert!(started.elapsed() < Duration::from_secs(2));
        }
    }
    println!("lifecycle callback error/unwind repetitions=100; calls after failure=0");
}

#[test]
fn lifecycle_gated_abort_and_drop() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, schema::Header, storage::Pool,
    };
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    for _ in 0..100 {
        let started = Instant::now();
        let format = support::formats()[0].clone();
        let (mut producer, reader) = channel(
            Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
            4,
        )
        .unwrap();
        for sequence in 1..=3 {
            producer.offer(
                &[19; 256],
                Header {
                    sequence,
                    segment: 1,
                    generation: 1,
                    frames: 64,
                    sample_position: None,
                    timestamp: None,
                },
                Overflow::DropOldest,
            );
        }
        drop(producer);
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let stream = CallbackStream::spawn(reader, move |delivery, context| {
            if let Delivery::Item(Item::Audio(block)) = delivery {
                assert_eq!(
                    context.completion.wait(Duration::ZERO),
                    CompletionResult::SameWorker
                );
                context.control.stop(Stop::Abort);
                entered.send(block.view()).unwrap();
                gate.recv_timeout(Duration::from_secs(1)).unwrap();
            }
            Ok(())
        })
        .unwrap();
        let held = seen.recv_timeout(Duration::from_secs(1)).unwrap();
        let completion = stream.completion();
        let control = stream.control_handle();
        assert_eq!(control.snapshot().in_flight, 64);
        assert_eq!(control.snapshot().discarded, 128);
        assert_eq!(
            completion.wait(Duration::from_millis(10)),
            CompletionResult::Timeout
        );
        drop(stream);
        assert_eq!(completion.poll(1000000, 1000000), CompletionResult::Timeout);
        assert_eq!(held.bytes(), &[19; 256]);
        release.send(()).unwrap();
        assert_eq!(
            completion.wait(Duration::from_secs(1)),
            CompletionResult::Complete
        );
        assert_eq!(held.bytes(), &[19; 256]);
        assert_eq!(control.snapshot().in_flight, 0);
        assert_eq!(control.snapshot().delivered, 64);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    println!(
        "lifecycle gated abort/drop schedules=100; in-flight=64; discarded=128; timeout preserves callback; retained bytes exact"
    );
}

#[test]
fn lifecycle_callback_completion_probe() {
    use capture_system_or_process_audio_core::{
        lifecycle::*, ordering::*, schema::Header, storage::Pool,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
        mpsc,
    };
    use std::time::Duration;
    let format = support::formats()[0].clone();
    let (mut producer, reader) = channel(
        Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
        4,
    )
    .unwrap();
    for sequence in 1..=3 {
        producer.offer(
            &[7; 256],
            Header {
                sequence,
                segment: 1,
                generation: 1,
                frames: 64,
                sample_position: None,
                timestamp: None,
            },
            Overflow::DropOldest,
        );
    }
    drop(producer);
    let acquisition = std::thread::current().id();
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let (entered, seen) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    let stream = CallbackStream::spawn(reader, move |delivery, context| {
        assert_ne!(std::thread::current().id(), acquisition);
        assert_eq!(
            context.completion.wait(Duration::ZERO),
            CompletionResult::SameWorker
        );
        if let Delivery::Item(Item::Audio(block)) = delivery {
            assert_eq!(block.bytes(), &[7; 256]);
            if worker_calls.fetch_add(1, SeqCst) == 0 {
                context.control.stop(Stop::Drain);
                entered.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(1)).unwrap();
            }
        }
        Ok(())
    })
    .unwrap();
    seen.recv_timeout(Duration::from_secs(1)).unwrap();
    let completion = stream.completion();
    assert_eq!(completion.poll(1000000, 1000000), CompletionResult::Timeout);
    assert_eq!(
        completion.wait(Duration::from_millis(10)),
        CompletionResult::Timeout
    );
    release.send(()).unwrap();
    assert_eq!(
        completion.wait(Duration::from_secs(1)),
        CompletionResult::Complete
    );
    assert_eq!(calls.load(SeqCst), 3);
}

#[test]
fn lifecycle_partial_abort_ledger() {
    use capture_system_or_process_audio_core::{
        ordering::*, reads::*, schema::Header, storage::Pool,
    };
    let format = support::formats()[0].clone();
    let (mut producer, reader) = channel(
        Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
        4,
    )
    .unwrap();
    let stream = ReadStream::new(reader);
    let control = producer.control_handle();
    for sequence in 1..=3 {
        producer.offer(
            &[7; 256],
            Header {
                sequence,
                segment: 1,
                generation: 1,
                frames: 64,
                sample_position: None,
                timestamp: None,
            },
            Overflow::DropOldest,
        );
    }
    let mut operation = stream
        .begin(Mode::Count, 17, false, None, Cancellation::default())
        .unwrap();
    assert_eq!(operation.poll(0).unwrap().frames, 17);
    drop(operation);
    let state = stream.status();
    assert_eq!(state.delivered, 17);
    assert_eq!(state.in_flight, 47);
    assert_eq!(state.queued, 128);
    control.stop(Stop::Abort);
    producer.fail_capture(Failure::Backend);
    drop(producer);
    let mut operation = stream
        .begin(Mode::Count, 256, false, None, Cancellation::default())
        .unwrap();
    let result = operation.poll(0).unwrap();
    assert_eq!(result.frames, 0);
    assert_eq!(result.reason, PartialReason::Failure(Failure::Backend));
    let state = stream.status();
    assert_eq!(state.discarded, 175);
    assert_eq!(state.input, state.delivered + state.discarded);
    assert_eq!(state.in_flight, 0);
}

#[test]
fn lifecycle_stop_probe() {
    use capture_system_or_process_audio_core::{ordering::*, schema::Header, storage::Pool};
    for format in support::formats() {
        let (mut producer, mut reader) = channel(
            Pool::new(
                StorageLimits::new(8, 256, 18, 36864).unwrap(),
                256,
                format.clone(),
            )
            .unwrap(),
            4,
        )
        .unwrap();
        let control = producer.control_handle();
        let header = Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 64,
            sample_position: None,
            timestamp: None,
        };
        assert_eq!(
            producer.offer(
                &vec![7; format.byte_count(64).unwrap()],
                header,
                Overflow::DropOldest,
            ),
            Offer::Queued
        );
        producer.fail_capture(Failure::Backend);
        assert!(!reader.ended());
        assert!(matches!(reader.next_item(), Some(Item::Audio(_))));
        assert!(!reader.ended(), "ordered terminal failure is still unread");
        assert!(matches!(
            reader.next_item(),
            Some(Item::Failed(Failure::Backend))
        ));
        assert!(
            reader.ended(),
            "terminal failure must end delivery before producer drop"
        );
        assert!(reader.next_item().is_none());
        assert!(!control.resources_done());
        drop(producer);
        assert!(control.resources_done());
        for mode in [Stop::Drain, Stop::Abort] {
            let (mut producer, mut reader) = channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap();
            let control = producer.control_handle();
            let bytes = vec![7; format.byte_count(64).unwrap()];
            let header = Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 64,
                sample_position: None,
                timestamp: None,
            };
            for sequence in 1..=3 {
                assert_eq!(
                    producer.offer(&bytes, Header { sequence, ..header }, Overflow::DropOldest),
                    Offer::Queued
                );
            }
            control.stop(mode);
            control.stop(Stop::Drain);
            assert_eq!(
                producer.offer(
                    &bytes,
                    Header {
                        sequence: 4,
                        ..header
                    },
                    Overflow::DropOldest
                ),
                Offer::Stopped
            );
            assert!(!control.resources_done());
            drop(producer);
            assert!(control.resources_done());
            let mut frames = 0;
            while let Some(item) = reader.next_item() {
                if let Item::Audio(block) = item {
                    frames += block.header().frames;
                }
            }
            assert_eq!(frames, if mode == Stop::Drain { 192 } else { 0 });
            let state = control.snapshot();
            assert_eq!(state.discarded, if mode == Stop::Abort { 192 } else { 0 });
            assert!(control.delivery_done());
            assert_eq!(state.input, state.delivered + state.discarded);
        }
    }
}

#[test]
fn reads_ordered_trace_matrix() {
    use capture_system_or_process_audio_core::{ordering::*, reads::*, storage::Pool};
    for initial in 0..4 {
        for trace_kind in support::TRACES {
            let trace = support::synthetic::acquire(initial, trace_kind);
            support::oracle::verify(initial, trace_kind, &trace).unwrap();
            let first = match &trace[0] {
                support::TraceItem::Format { format, .. } => format.clone(),
                _ => unreachable!(),
            };
            let (mut producer, reader) = channel(
                Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, first).unwrap(),
                4,
            )
            .unwrap();
            let stream = ReadStream::new(reader);
            let mut frames = 0;
            let mut boundaries = 0;
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            let mut items = trace.into_iter().skip(1);
            let mut done = false;
            while !done {
                let mut operation = stream
                    .begin(Mode::Count, 257, true, None, Cancellation::default())
                    .unwrap();
                let result = loop {
                    if let Some(result) = operation.poll(0) {
                        break result;
                    }
                    match items.next().expect("fixture must terminate") {
                        support::TraceItem::Pcm { header, bytes, .. } => {
                            expected.extend_from_slice(&bytes);
                            assert_eq!(
                                producer.offer(&bytes, header, Overflow::DropOldest),
                                Offer::Queued
                            );
                        }
                        support::TraceItem::Gap {
                            sequence,
                            segment,
                            loss,
                        } => producer.gap(sequence, segment, loss).unwrap(),
                        support::TraceItem::Format {
                            sequence,
                            segment,
                            format,
                            ..
                        } => {
                            producer
                                .prepare_format(sequence, segment, format, 8)
                                .unwrap();
                        }
                        support::TraceItem::Terminal { failure, .. } => {
                            if failure.is_some() {
                                producer.fail_capture(Failure::Backend);
                            } else {
                                producer.close();
                            }
                        }
                    }
                };
                frames += result.frames;
                for chunk in result.chunks {
                    actual.extend_from_slice(chunk.bytes());
                }
                if result.reason == PartialReason::Boundary {
                    boundaries += 1;
                    assert_eq!(frames, 2076);
                    let control = result.control.unwrap();
                    if let Some(loss) = control.losses.entries().first() {
                        assert_eq!(
                            loss.known_frames,
                            if matches!(trace_kind, support::Trace::KnownGap) {
                                7
                            } else {
                                0
                            }
                        );
                        assert_eq!(
                            loss.unknown,
                            matches!(trace_kind, support::Trace::UnknownGap)
                        );
                    } else {
                        assert_eq!(control.generation, 2);
                        assert_eq!(control.format.rate(), 44100);
                    }
                }
                done = matches!(
                    result.reason,
                    PartialReason::Eof | PartialReason::Failure(_)
                );
                if done {
                    assert_eq!(
                        result.reason,
                        if matches!(trace_kind, support::Trace::Failure) {
                            PartialReason::Failure(Failure::Backend)
                        } else {
                            PartialReason::Eof
                        }
                    );
                }
            }
            assert_eq!(frames, 4096);
            assert_eq!(actual, expected);
            assert_eq!(
                boundaries,
                usize::from(matches!(
                    trace_kind,
                    support::Trace::KnownGap
                        | support::Trace::UnknownGap
                        | support::Trace::Generation
                ))
            );
        }
    }
    println!(
        "reads ordered traces=20; exact boundary frame=2076; known/unknown/format/error/EOF preserved"
    );
}

#[test]
fn reads_deadlines_iterator_and_invalid() {
    use capture_system_or_process_audio_core::{
        ordering::*, reads::*, schema::Header, storage::Pool,
    };
    let format = support::formats()[0].clone();
    for _ in 0..100 {
        for second_at in [999999, 1000001] {
            let (mut producer, reader) = channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap();
            let stream = ReadStream::new(reader);
            let mut operation = stream
                .begin(
                    Mode::Count,
                    64,
                    true,
                    Some(1000000),
                    Cancellation::default(),
                )
                .unwrap();
            let header = Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 17,
                sample_position: Some(0),
                timestamp: None,
            };
            producer.offer(&[1; 68], header, Overflow::DropOldest);
            assert!(operation.poll(0).is_none());
            producer.offer(
                &[2; 188],
                Header {
                    sequence: 2,
                    frames: 47,
                    ..header
                },
                Overflow::DropOldest,
            );
            let result = operation.poll(second_at).unwrap();
            assert_eq!(result.frames, if second_at < 1000000 { 64 } else { 17 });
            assert_eq!(
                result.reason,
                if second_at < 1000000 {
                    PartialReason::Complete
                } else {
                    PartialReason::Timeout
                }
            );
            drop(operation);
            assert_eq!(
                producer.snapshot().queued,
                if second_at < 1000000 { 0 } else { 47 }
            );
            let before = producer.snapshot();
            {
                let _iterator = stream.blocks();
            }
            assert_eq!(producer.snapshot(), before);
            let mut invalid = [99; 3];
            assert!(matches!(
                stream.begin_into(
                    &mut invalid,
                    format.clone(),
                    1,
                    false,
                    None,
                    Cancellation::default()
                ),
                Err(ReadError::InvalidDestination)
            ));
            assert_eq!(producer.snapshot(), before);
            if second_at > 1000000 {
                assert_eq!(stream.blocks().next().unwrap().unwrap().frames, 47);
            }
            for failure_first in [false, true] {
                let cancel = Cancellation::default();
                let mut operation = stream
                    .begin(Mode::Variable, 1, true, None, cancel.clone())
                    .unwrap();
                if failure_first {
                    producer.fail_capture(Failure::Backend);
                    cancel.cancel();
                } else {
                    cancel.cancel();
                    producer.fail_capture(Failure::Backend);
                }
                assert_eq!(operation.poll(0).unwrap().reason, PartialReason::Cancelled);
                assert_eq!(producer.snapshot().failure, Some(Failure::Backend));
                drop(operation);
                assert!(!stream.is_pending());
            }
        }
    }
    for reason in [
        PartialReason::Complete,
        PartialReason::WouldBlock,
        PartialReason::Boundary,
        PartialReason::Cancelled,
        PartialReason::Timeout,
        PartialReason::Eof,
        PartialReason::Failure(Failure::Backend),
        PartialReason::Ready,
    ] {
        let (code, failure) = partial_code(reason);
        assert_eq!(partial_from_code(code, failure), Ok(reason));
    }
    assert!(partial_from_code(99, 0).is_err());
    assert!(partial_from_code(1, 7).is_err());
    let wide = std::sync::Arc::new(
        capture_system_or_process_audio_core::schema::Format::new(
            capture_system_or_process_audio_core::schema::SampleType::F32,
            capture_system_or_process_audio_core::schema::ByteOrder::Little,
            48000,
            16,
            capture_system_or_process_audio_core::schema::Layout::Unknown,
        )
        .unwrap(),
    );
    let (mut producer, reader) = channel(
        Pool::new(StorageLimits::new(2, 1, 64, 128).unwrap(), 1, wide).unwrap(),
        2,
    )
    .unwrap();
    assert_eq!(
        producer.offer(
            &[0; 64],
            Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 1,
                sample_position: None,
                timestamp: None,
            },
            Overflow::DropOldest,
        ),
        Offer::Queued
    );
    let stream = ReadStream::new(reader);
    for requested in [usize::MAX / 64 + 1, isize::MAX as usize / 64 + 1] {
        assert!(matches!(
            stream.begin(Mode::Count, requested, false, None, Cancellation::default()),
            Err(ReadError::InvalidRequest)
        ));
    }
    assert!(!stream.is_pending());
    assert_eq!(stream.status().queued, 1);
    let mut valid = stream
        .begin(Mode::Count, 1, false, None, Cancellation::default())
        .unwrap();
    assert_eq!(valid.poll(0).unwrap().chunks[0].bytes(), &[0; 64]);
}

#[test]
fn reads_count_matrix() {
    use capture_system_or_process_audio_core::{
        ordering::*, reads::*, schema::Header, storage::Pool,
    };
    let started = std::time::Instant::now();
    for (format_index, format) in support::formats().into_iter().enumerate() {
        let trace = support::synthetic::acquire(format_index, support::Trace::Continuous);
        support::oracle::verify(format_index, support::Trace::Continuous, &trace).unwrap();
        let expected: Vec<u8> = trace
            .iter()
            .filter_map(|item| {
                if let support::TraceItem::Pcm { bytes, .. } = item {
                    Some(bytes.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .copied()
            .collect();
        for requested in [1, 64, 257, 1024] {
            for blocking in [false, true] {
                for into in [false, true] {
                    let (mut producer, reader) = channel(
                        Pool::new(
                            StorageLimits::new(8, 256, 18, 36864).unwrap(),
                            256,
                            format.clone(),
                        )
                        .unwrap(),
                        4,
                    )
                    .unwrap();
                    let stream = ReadStream::new(reader);
                    let stride = format.bytes_per_frame();
                    let mut storage = vec![99; (requested + 2) * stride];
                    let mut actual = Vec::new();
                    let mut position = 0;
                    let mut packet_index = 0;
                    let mut eof = false;
                    while !eof {
                        let mut operation = if into {
                            stream
                                .begin_into(
                                    &mut storage[stride..(requested + 1) * stride],
                                    format.clone(),
                                    requested,
                                    blocking,
                                    None,
                                    Cancellation::default(),
                                )
                                .unwrap()
                        } else {
                            stream
                                .begin(
                                    Mode::Count,
                                    requested,
                                    blocking,
                                    None,
                                    Cancellation::default(),
                                )
                                .unwrap()
                        };
                        let result = loop {
                            if let Some(result) = operation.poll(0) {
                                break result;
                            }
                            assert!(blocking);
                            if position == 4096 {
                                producer.close();
                            } else {
                                let frames =
                                    [1, 7, 64, 255, 256][packet_index % 5].min(4096 - position);
                                assert_eq!(
                                    producer.offer(
                                        &expected[position * stride..(position + frames) * stride],
                                        Header {
                                            sequence: packet_index as u64 + 1,
                                            segment: 1,
                                            generation: 1,
                                            frames,
                                            sample_position: Some(position as u64),
                                            timestamp: None
                                        },
                                        Overflow::DropOldest
                                    ),
                                    Offer::Queued
                                );
                                position += frames;
                                packet_index += 1;
                            }
                        };
                        drop(operation);
                        assert!(result.frames <= requested);
                        if into {
                            actual
                                .extend_from_slice(&storage[stride..(result.frames + 1) * stride]);
                        } else {
                            for slice in result.chunks {
                                actual.extend_from_slice(slice.bytes());
                            }
                        }
                        assert!(
                            storage[..stride]
                                .iter()
                                .chain(storage[(requested + 1) * stride..].iter())
                                .all(|byte| *byte == 99)
                        );
                        eof = result.reason == PartialReason::Eof;
                        if !blocking && result.reason == PartialReason::WouldBlock {
                            if position == 4096 {
                                producer.close();
                            } else {
                                let frames =
                                    [1, 7, 64, 255, 256][packet_index % 5].min(4096 - position);
                                assert_eq!(
                                    producer.offer(
                                        &expected[position * stride..(position + frames) * stride],
                                        Header {
                                            sequence: packet_index as u64 + 1,
                                            segment: 1,
                                            generation: 1,
                                            frames,
                                            sample_position: Some(position as u64),
                                            timestamp: None
                                        },
                                        Overflow::DropOldest
                                    ),
                                    Offer::Queued
                                );
                                position += frames;
                                packet_index += 1;
                            }
                        }
                    }
                    assert_eq!(actual, expected);
                    assert!(!stream.is_pending());
                    assert_eq!(producer.snapshot().delivered, 4096);
                }
            }
        }
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    println!("reads interval cases=64; exact frames=4096; counts=1,64,257,1024; sentinels intact");
}

#[test]
fn reads_overlap_and_cancel_schedules() {
    use capture_system_or_process_audio_core::{
        ordering::*, reads::*, schema::Header, storage::Pool,
    };
    let format = support::formats()[0].clone();
    for _ in 0..100 {
        let started = std::time::Instant::now();
        for active in [Mode::Variable, Mode::Count, Mode::Into, Mode::Readiness] {
            let (mut producer, reader) = channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap();
            let stream = ReadStream::new(reader);
            let mut destination = [0; 256];
            let cancel = Cancellation::default();
            let mut operation = if active == Mode::Into {
                stream
                    .begin_into(
                        &mut destination,
                        format.clone(),
                        64,
                        true,
                        None,
                        cancel.clone(),
                    )
                    .unwrap()
            } else {
                stream
                    .begin(active, 64, true, None, cancel.clone())
                    .unwrap()
            };
            assert!(operation.poll(0).is_none());
            for rejected in [Mode::Variable, Mode::Count, Mode::Into, Mode::Readiness] {
                let mut other = [0; 256];
                let rejected = if rejected == Mode::Into {
                    stream.begin_into(
                        &mut other,
                        format.clone(),
                        64,
                        false,
                        None,
                        Cancellation::default(),
                    )
                } else {
                    stream.begin(rejected, 64, false, None, Cancellation::default())
                };
                assert!(matches!(rejected, Err(ReadError::AlreadyPending)));
                assert_eq!(producer.snapshot().delivered, 0);
            }
            cancel.cancel();
            assert_eq!(
                producer.offer(
                    &[7; 256],
                    Header {
                        sequence: 1,
                        segment: 1,
                        generation: 1,
                        frames: 64,
                        sample_position: None,
                        timestamp: None
                    },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
            assert_eq!(operation.poll(0).unwrap().reason, PartialReason::Cancelled);
            drop(operation);
            assert_eq!(producer.snapshot().queued, 64);
            assert_eq!(producer.snapshot().delivered, 0);
            let mut ready = stream
                .begin(Mode::Readiness, 1, true, None, Cancellation::default())
                .unwrap();
            assert_eq!(ready.poll(0).unwrap().reason, PartialReason::Ready);
            drop(ready);
            assert_eq!(producer.snapshot().delivered, 0);
            let cancel = Cancellation::default();
            let mut read = stream
                .begin(Mode::Variable, 64, true, None, cancel.clone())
                .unwrap();
            assert_eq!(read.poll(0).unwrap().frames, 64);
            cancel.cancel();
            drop(read);
            let wait = stream
                .begin(Mode::Readiness, 1, true, None, Cancellation::default())
                .unwrap();
            drop(wait);
            assert!(!stream.is_pending());
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
    println!("reads overlap matrix=16; cancel schedules=100; hidden reservations=0");
}

#[test]
fn reads_warmed_into() {
    use capture_system_or_process_audio_core::{
        ordering::*, reads::*, schema::Header, storage::Pool,
    };
    for format in support::formats() {
        for frames in [1, 64, 256] {
            let (mut producer, reader) = channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap();
            let stream = ReadStream::new(reader);
            let source = vec![7; format.byte_count(frames).unwrap()];
            let mut storage = vec![99; source.len() + 2];
            let cancel = Cancellation::default();
            assert_eq!(
                producer.offer(
                    &source,
                    Header {
                        sequence: 1,
                        segment: 1,
                        generation: 1,
                        frames,
                        sample_position: None,
                        timestamp: None
                    },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
            let warm_res = stream
                .begin_into(
                    &mut storage[1..source.len() + 1],
                    format.clone(),
                    frames,
                    false,
                    None,
                    cancel.clone(),
                )
                .unwrap()
                .poll(0)
                .unwrap();
            assert_eq!(warm_res.frames, frames);
            let mut copies = warm_res.copies.bytes;
            let measured = allocation_counter::measure(|| {
                for sequence in 2..=10000 {
                    assert_eq!(
                        producer.offer(
                            &source,
                            Header {
                                sequence,
                                segment: 1,
                                generation: 1,
                                frames,
                                sample_position: None,
                                timestamp: None
                            },
                            Overflow::DropOldest
                        ),
                        Offer::Queued
                    );
                    let mut operation = stream
                        .begin_into(
                            &mut storage[1..source.len() + 1],
                            format.clone(),
                            frames,
                            false,
                            None,
                            cancel.clone(),
                        )
                        .unwrap();
                    let result = operation.poll(0).unwrap();
                    assert_eq!(result.frames, frames);
                    assert_eq!(result.reason, PartialReason::Complete);
                    assert_eq!(result.copies.destinations, 1);
                    copies += result.copies.bytes;
                }
            });
            assert_eq!(measured.count_total, 0);
            assert_eq!(copies, 10000 * source.len() as u64);
            assert_eq!(producer.pool().stats().bytes, copies);
            assert_eq!(storage[0], 99);
            assert_eq!(*storage.last().unwrap(), 99);
            assert_eq!(&storage[1..source.len() + 1], source);
            assert!(!stream.is_pending());
        }
    }
    println!(
        "reads warmed packets=120000; PCM allocations=0; acquisition copies=1; destination copies=1"
    );
}

#[test]
fn reads_partial_outcomes() {
    use capture_system_or_process_audio_core::{
        ordering::*,
        reads::*,
        schema::{Header, Loss},
        storage::Pool,
    };
    for format in support::formats() {
        for reason in [
            PartialReason::Boundary,
            PartialReason::Cancelled,
            PartialReason::Timeout,
            PartialReason::Eof,
            PartialReason::Failure(Failure::Backend),
        ] {
            let (mut producer, reader) = channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap();
            let stream = ReadStream::new(reader);
            let cancel = Cancellation::default();
            let mut operation = stream
                .begin(Mode::Count, 64, true, Some(1_000_000), cancel.clone())
                .unwrap();
            let bytes = vec![7; format.byte_count(17).unwrap()];
            assert_eq!(
                producer.offer(
                    &bytes,
                    Header {
                        sequence: 1,
                        segment: 1,
                        generation: 1,
                        frames: 17,
                        sample_position: Some(0),
                        timestamp: None
                    },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
            assert!(operation.poll(999999).is_none());
            match reason {
                PartialReason::Boundary => producer
                    .gap(
                        2,
                        2,
                        Loss {
                            frames: Some(7),
                            rate_hz: format.rate(),
                        },
                    )
                    .unwrap(),
                PartialReason::Cancelled => cancel.cancel(),
                PartialReason::Eof => producer.close(),
                PartialReason::Failure(failure) => producer.fail_capture(failure),
                _ => {}
            }
            let result = operation
                .poll(if reason == PartialReason::Timeout {
                    1000001
                } else {
                    999999
                })
                .unwrap();
            assert_eq!(result.frames, 17);
            assert_eq!(result.reason, reason);
            assert_eq!(result.chunks[0].bytes(), bytes);
            assert!(!stream.is_pending());
        }
    }
}

#[test]
fn reads_reservation_partial_probe() {
    use capture_system_or_process_audio_core::{
        ordering::*, reads::*, schema::Header, storage::Pool,
    };
    let format = support::formats()[0].clone();
    let (mut producer, reader) = channel(
        Pool::new(
            StorageLimits::new(8, 256, 18, 36864).unwrap(),
            256,
            format.clone(),
        )
        .unwrap(),
        4,
    )
    .unwrap();
    let stream = ReadStream::new(reader);
    let cancel = Cancellation::default();
    let mut destination = [99; 260];
    let mut operation = stream
        .begin_into(
            &mut destination[2..258],
            format,
            64,
            true,
            Some(1_000_000),
            cancel.clone(),
        )
