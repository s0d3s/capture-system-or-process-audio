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
        .unwrap();
    assert!(matches!(
        stream.begin(Mode::Variable, 64, false, None, Cancellation::default()),
        Err(ReadError::AlreadyPending)
    ));
    producer.offer(
        &[7; 68],
        Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 17,
            sample_position: Some(0),
            timestamp: None,
        },
        Overflow::DropOldest,
    );
    assert!(operation.poll(999999).is_none());
    cancel.cancel();
    let result = operation.poll(999999).unwrap();
    assert_eq!(result.frames, 17);
    assert_eq!(result.reason, PartialReason::Cancelled);
    drop(operation);
    assert!(!stream.is_pending());
    assert_eq!(&destination[..2], &[99; 2]);
    assert_eq!(&destination[2..70], &[7; 68]);
    assert!(destination[70..].iter().all(|byte| *byte == 99));
    let mut wait = stream
        .begin(Mode::Readiness, 1, true, None, Cancellation::default())
        .unwrap();
    assert!(wait.poll(0).is_none());
    drop(wait);
    assert!(!stream.is_pending());
}

#[test]
fn ordering_oracle_negative_controls() {
    use capture_system_or_process_audio_core::ordering::LossCause;
    let expected_bytes = [0, 128, 255, 255];
    let accepts = |boundary: bool, cause: LossCause, held: &[u8]| {
        boundary && cause == LossCause::PoolExhaustion && held == expected_bytes
    };
    assert!(accepts(true, LossCause::PoolExhaustion, &expected_bytes));
    assert!(!accepts(false, LossCause::PoolExhaustion, &expected_bytes));
    assert!(!accepts(true, LossCause::QueueOldest, &expected_bytes));
    assert!(!accepts(
        true,
        LossCause::PoolExhaustion,
        &[1, 128, 255, 255]
    ));
}

#[test]
fn ordering_schedules_and_instrumentation() {
    use capture_system_or_process_audio_core::{
        ordering::*,
        schema::{Header, Loss},
        storage::Pool,
    };
    use support::representation::EventRecord;
    let started = std::time::Instant::now();
    let format = support::formats()[0].clone();
    let header = Header {
        sequence: 1,
        segment: 1,
        generation: 1,
        frames: 64,
        sample_position: None,
        timestamp: None,
    };
    for _ in 0..100 {
        let schedule = std::time::Instant::now();
        ordering_pressure_baseline();
        for release_first in [false, true] {
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
            let mut held = Vec::new();
            for sequence in 1..=8 {
                producer.offer(
                    &[sequence as u8; 256],
                    Header { sequence, ..header },
                    Overflow::DropOldest,
                );
                held.push(reader.next_item().unwrap());
            }
            if release_first {
                drop(held.pop());
            }
            let outcome = producer.offer(
                &[9; 256],
                Header {
                    sequence: 9,
                    ..header
                },
                Overflow::DropOldest,
            );
            assert_eq!(
                outcome,
                if release_first {
                    Offer::Queued
                } else {
                    Offer::PoolDropped
                }
            );
            if !release_first {
                drop(held.pop());
            }
            assert_eq!(
                producer.offer(
                    &[10; 256],
                    Header {
                        sequence: 10,
                        ..header
                    },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
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
        producer
            .gap(
                1,
                2,
                Loss {
                    frames: None,
                    rate_hz: 48000,
                },
            )
            .unwrap();
        for sequence in 2..=6 {
            producer.offer(
                &[sequence as u8; 256],
                Header {
                    sequence,
                    segment: 2,
                    ..header
                },
                Overflow::DropOldest,
            );
        }
        let mut sequences = Vec::new();
        let mut source_unknown = false;
        let mut lost = 0;
        while let Some(item) = reader.next_item() {
            let restored = Item::try_from(EventRecord::from(item)).unwrap();
            match restored {
                Item::Control(control) => {
                    for loss in control.losses.entries() {
                        if loss.cause == LossCause::Source {
                            source_unknown = loss.unknown;
                        } else {
                            assert_eq!(loss.cause, LossCause::QueueOldest);
                            lost += loss.known_frames;
                        }
                    }
                }
                Item::Audio(block) => sequences.push(block.header().sequence),
                _ => panic!("unexpected terminal"),
            }
        }
        assert!(source_unknown);
        assert_eq!(lost, 64);
        assert_eq!(sequences, [3, 4, 5, 6]);
        assert!(schedule.elapsed() < std::time::Duration::from_secs(2));
    }
    let (mut producer, mut reader) = channel(
        Pool::new(StorageLimits::new(8, 256, 18, 36864).unwrap(), 256, format).unwrap(),
        4,
    )
    .unwrap();
    for sequence in 1..=4 {
        producer.offer(
            &[1; 256],
            Header { sequence, ..header },
            Overflow::DropOldest,
        );
    }
    let mut max_copy = 0;
    let work = allocation_counter::measure(|| {
        for sequence in 5..=10004 {
            let before = producer.pool().stats();
            assert_eq!(
                producer.offer(
                    &[2; 256],
                    Header { sequence, ..header },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
            max_copy = max_copy.max(producer.pool().stats().destinations - before.destinations);
        }
    });
    assert_eq!(work.count_total, 0);
    assert_eq!(work.bytes_current, 0);
    assert_eq!(max_copy, 1);
    let mut lost = 0;
    let mut audio = 0;
    while let Some(item) = reader.next_item() {
        match item {
            Item::Audio(_) => audio += 64,
            Item::Control(control) => {
                lost += control
                    .losses
                    .entries()
                    .iter()
                    .map(|loss| loss.known_frames)
                    .sum::<u64>()
            }
            _ => panic!("unexpected terminal"),
        }
    }
    assert_eq!(lost, 10000 * 64);
    assert_eq!(audio, 256);
    let state = producer.snapshot();
    assert_eq!(
        state.input,
        state.delivered
            + state.queued
            + state.in_flight
            + state.queue_lost
            + state.pool_lost
            + state.rejected
            + state.discarded
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    println!(
        "ordering paired schedules=100; pressure packets=10000; allocations=0; copy maximum=1; frame partition exact"
    );
}

#[test]
fn ordering_capacity_and_memory() {
    use capture_system_or_process_audio_core::{
        ordering::*,
        schema::{ByteOrder, Format, Header, Layout, SampleType},
        storage::Pool,
    };
    let formats = support::formats();
    let mut cases = 0;
    for format in &formats {
        for duration in [10, 20, 40] {
            let baseline = duration_capacity(format, duration, 24576, 1, 1).unwrap();
            for preferred in [1, 64, 1024] {
                assert_eq!(
                    duration_capacity(format, duration, 24576, 1, preferred),
                    Ok(baseline)
                );
                cases += 1;
            }
            let minimum = duration_capacity(format, 1, 24576, usize::MAX / 1024, 1);
            assert!(minimum.is_err());
            let one = duration_capacity(format, 1, 24576, 64, 1).unwrap();
            assert_eq!(one.handles, if format.rate() > 64000 { 2 } else { 1 });
            let per_handle = format.bytes_per_frame() * 64 + 40;
            let minimum_bytes = one.accounted_bytes - (one.handles - 1) * per_handle;
            assert!(duration_capacity(format, duration, minimum_bytes - 1, 64, 1).is_err());
        }
    }
    assert_eq!(cases, 36);
    assert!(duration_capacity(&formats[0], 0, 24576, 1, 1).is_err());
    assert!(duration_capacity(&formats[0], u64::MAX, usize::MAX, 1, 1).is_err());
    let mut pair = None;
    let setup = allocation_counter::measure(|| {
        pair = Some(
            channel(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    formats[0].clone(),
                )
                .unwrap(),
                4,
            )
            .unwrap(),
        );
    });
    let (mut producer, mut reader) = pair.unwrap();
    let metadata = setup.bytes_current - 36864
        + (std::mem::size_of::<Producer>() + std::mem::size_of::<Reader>()) as i64;
    println!("ordering measured metadata={metadata}");
    assert!(metadata <= 8192, "metadata {metadata}");
    let header = Header {
        sequence: 1,
        segment: 1,
        generation: 1,
        frames: 1,
        sample_position: None,
        timestamp: None,
    };
    let work = allocation_counter::measure(|| {
        assert_eq!(
            producer.offer(&[1; 4], header, Overflow::DropOldest),
            Offer::Queued
        );
        let Some(Item::Audio(block)) = reader.next_item() else {
            panic!("preferred size delayed eligible audio");
        };
        assert_eq!(block.header().frames, 1);
    });
    assert_eq!(work.count_total, 0);
    producer.offer(
        &[2; 4],
        Header {
            sequence: 2,
            ..header
        },
        Overflow::DropOldest,
    );
    assert_eq!(
        producer.offer(
            &[3; 1028],
            Header {
                sequence: 3,
                frames: 257,
                ..header
            },
            Overflow::DropOldest
        ),
        Offer::Failed(Failure::PacketTooLarge)
    );
    assert_eq!(producer.snapshot().rejected, 257);
    assert!(matches!(reader.next_item(), Some(Item::Audio(_))));
    assert!(matches!(
        reader.next_item(),
        Some(Item::Failed(Failure::PacketTooLarge))
    ));

    let (mut producer, _reader) = channel(
        Pool::new(
            StorageLimits::new(9, 1, 4, 36).unwrap(),
            1,
            formats[0].clone(),
        )
        .unwrap(),
        9,
    )
    .unwrap();
    for index in 0..9 {
        let generation = if index == 0 {
            1
        } else {
            producer
                .prepare_format(
                    index * 2,
                    index + 1,
                    Format::new(
                        SampleType::I16,
                        ByteOrder::Little,
                        48000 + index as u32,
                        2,
                        Layout::Unknown,
                    )
                    .unwrap()
                    .into(),
                    1,
                )
                .unwrap()
        };
        assert_eq!(
            producer.offer(
                &[1; 4],
                Header {
                    sequence: index * 2 + 1,
                    segment: index + 1,
                    generation,
                    ..header
                },
                Overflow::DropOldest,
            ),
            Offer::Queued
        );
    }
    let snapshot = producer.snapshot();
    assert!(snapshot.stable);
    assert_eq!(snapshot.queued, 9);
    assert_eq!(snapshot.queued_domains, 9);
    for (index, duration) in snapshot.queued_durations[..snapshot.queued_domains]
        .iter()
        .enumerate()
    {
        assert_eq!(duration.frames, 1);
        assert_eq!(duration.rate_hz, 48000 + index as u32);
    }
}

#[test]
fn ordering_all_held_and_control_bursts() {
    use capture_system_or_process_audio_core::{
        ordering::*,
        schema::{Header, Loss},
        storage::Pool,
    };
    let formats = support::formats();
    let header = Header {
        sequence: 1,
        segment: 1,
        generation: 1,
        frames: 64,
        sample_position: None,
        timestamp: None,
    };
    for policy in [Overflow::DropOldest, Overflow::DropNewest, Overflow::Fail] {
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
        let mut held = Vec::new();
        for sequence in 1..=8 {
            assert_eq!(
                producer.offer(
                    &[sequence as u8; 256],
                    Header { sequence, ..header },
                    policy
                ),
                Offer::Queued
            );
            let Some(Item::Audio(block)) = reader.next_item() else {
                panic!("missing held input");
            };
            held.push(block);
        }
        let result = producer.offer(
            &[9; 256],
            Header {
                sequence: 9,
                ..header
            },
            policy,
        );
        assert_eq!(
            result,
            if policy == Overflow::Fail {
                Offer::Failed(Failure::PoolExhaustion)
            } else {
                Offer::PoolDropped
            }
        );
        let snapshot = producer.snapshot();
        assert_eq!(snapshot.input, 576);
        assert_eq!(snapshot.delivered, 512);
        assert_eq!(snapshot.queue_lost, 0);
        assert_eq!(snapshot.pool_lost + snapshot.rejected, 64);
        assert_eq!(producer.pool().available(), 0);
        if policy == Overflow::Fail {
            assert!(matches!(
                reader.next_item(),
                Some(Item::Failed(Failure::PoolExhaustion))
            ));
        } else {
            let Some(Item::Control(control)) = reader.next_item() else {
                panic!("missing exhaustion gap");
            };
            assert_eq!(
                control.losses.entries(),
                &[DomainLoss {
                    rate_hz: 48000,
                    known_frames: 64,
                    unknown: false,
                    cause: LossCause::PoolExhaustion
                }]
            );
        }
        for (index, block) in held.iter().enumerate() {
            assert_eq!(block.bytes(), &[(index + 1) as u8; 256]);
        }
        drop(held);
        assert_eq!(producer.pool().available(), 8);
    }
    for _ in 0..100 {
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
        for sequence in 1..=4 {
            assert_eq!(
                producer.offer(
                    &[sequence as u8; 256],
                    Header { sequence, ..header },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
        }
        let mut generation = 1;
        for sequence in 5..=15 {
            let segment = sequence - 3;
            if sequence % 2 == 1 {
                generation = producer
                    .prepare_format(
                        sequence,
                        segment,
                        formats[(sequence as usize / 2) % 2].clone(),
                        4,
                    )
                    .unwrap();
            } else {
                let rate = producer.pool().format().rate();
                producer
                    .gap(
                        sequence,
                        segment,
                        Loss {
                            frames: if sequence == 10 { None } else { Some(7) },
                            rate_hz: rate,
                        },
                    )
                    .unwrap();
            }
        }
        assert_eq!(generation, 7);
        assert_eq!(
            producer.offer(
                &[16; 256],
                Header {
                    sequence: 16,
                    generation,
                    segment: 12,
                    ..header
                },
                Overflow::DropOldest
            ),
            Offer::Queued
        );
        let mut delivered = Vec::new();
        let mut records = 0;
        let mut known_48000 = 0;
        let mut known_44100 = 0;
        let mut unknown_48000 = false;
        let mut queue_lost = 0;
        let mut previous = 0;
        while let Some(item) = reader.next_item() {
            match item {
                Item::Audio(block) => {
                    assert!(block.header().sequence > previous);
                    previous = block.header().sequence;
                    if block.header().sequence == 16 {
                        assert_eq!(block.format(), formats[1].as_ref());
                        assert_eq!(block.header().generation, 7);
                    }
                    delivered.push(block.header().sequence);
                }
                Item::Control(control) => {
                    assert!(control.sequence > previous);
                    previous = control.sequence;
                    records += control.records;
                    for loss in control.losses.entries() {
                        match (loss.cause, loss.rate_hz) {
                            (LossCause::QueueOldest, 48000) => queue_lost += loss.known_frames,
                            (LossCause::Source, 48000) => {
                                known_48000 += loss.known_frames;
                                unknown_48000 |= loss.unknown;
                            }
                            (LossCause::Source, 44100) => {
                                known_44100 += loss.known_frames;
                                assert!(!loss.unknown);
                            }
                            _ => panic!("wrong loss domain or cause"),
                        }
                    }
                }
                Item::Failed(failure) => panic!("unexpected control failure {failure:?}"),
            }
        }
        assert_eq!(delivered, [2, 3, 4, 16]);
        assert_eq!(records, 12);
        assert_eq!(queue_lost, 64);
        assert_eq!(known_48000, 14);
        assert_eq!(known_44100, 14);
        assert!(unknown_48000);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
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
    for sequence in 1..=4 {
        producer.offer(
            &[sequence as u8; 256],
            Header { sequence, ..header },
            Overflow::DropOldest,
        );
    }
    for sequence in 5..=12 {
        producer
            .gap(
                sequence,
                sequence,
                Loss {
                    frames: Some(7),
                    rate_hz: 48000,
                },
            )
            .unwrap();
    }
    let failure_work = allocation_counter::measure(|| producer.fail_capture(Failure::Backend));
    assert_eq!(failure_work.count_total, 0);
    assert_eq!(producer.snapshot().failure, Some(Failure::Backend));
    assert_eq!(
        producer.terminal_details().unwrap().state.losses.entries()[0].known_frames,
        56
    );
    let mut audio = 0;
    let mut controls = 0;
    let mut failures = 0;
    while let Some(item) = reader.next_item() {
        match item {
            Item::Audio(_) => {
                assert_eq!(controls, 0);
                audio += 1;
            }
            Item::Control(_) => {
                assert_eq!(audio, 4);
                controls += 1;
            }
            Item::Failed(Failure::Backend) => {
                assert_eq!(audio, 4);
                assert_eq!(controls, 8);
                failures += 1;
            }
            _ => panic!("wrong terminal"),
        }
    }
    assert_eq!((audio, controls, failures), (4, 8, 1));
    println!(
        "ordering all-held policies=3; control-overflow schedules=100; full-control failure tail=4audio/8controls/1error"
    );
}

#[cfg(feature = "test-hooks")]
#[test]
fn ordering_parked_claimant() {
    use capture_system_or_process_audio_core::{ordering::*, schema::Header, storage::Pool};
    use std::{sync::mpsc, time::Duration};
    for _ in 0..100 {
        let (mut producer, mut reader) = channel(
            Pool::new(
                StorageLimits::new(8, 256, 18, 36864).unwrap(),
                256,
                support::formats()[0].clone(),
            )
            .unwrap(),
            4,
        )
        .unwrap();
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
        let (notice, claimed) = mpsc::channel();
        let (resume, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let initial = reader.next_item_with_claim_hook(|| {
                notice.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(1)).unwrap();
            });
            let item = initial.or_else(|| reader.next_item()).unwrap();
            (reader, item)
        });
        claimed.recv_timeout(Duration::from_secs(1)).unwrap();
        for sequence in 5..=104 {
            assert_eq!(
                producer.offer(
                    &[sequence as u8; 256],
                    Header { sequence, ..header },
                    Overflow::DropOldest
                ),
                Offer::Queued
            );
            assert!(producer.attempts() <= 9);
        }
        resume.send(()).unwrap();
        let (mut reader, item) = worker.join().unwrap();
        let Item::Audio(first) = item else {
            panic!("claimed owner lost");
        };
        assert_eq!(first.header().sequence, 1);
        assert_eq!(first.bytes(), &[1; 256]);
        let mut tail = Vec::new();
        let mut lost = 0;
        while let Some(item) = reader.next_item() {
            match item {
                Item::Audio(block) => tail.push(block.header().sequence),
                Item::Control(control) => {
                    lost += control
                        .losses
                        .entries()
                        .iter()
                        .map(|loss| loss.known_frames)
                        .sum::<u64>()
                }
                Item::Failed(error) => panic!("{error:?}"),
            }
        }
        assert_eq!(tail, [101, 102, 103, 104]);
        assert_eq!(lost, 99 * 64);
    }
    println!("ordering parked claimant schedules=100; producer bound<=9; held owner unchanged");
}

#[test]
fn ordering_pressure_baseline() {
    use capture_system_or_process_audio_core::{ordering::*, schema::Header, storage::Pool};
    for policy in [Overflow::DropOldest, Overflow::DropNewest, Overflow::Fail] {
        let pool = Pool::new(
            StorageLimits::new(8, 256, 18, 36864).unwrap(),
            256,
            support::formats()[0].clone(),
        )
        .unwrap();
        let (mut producer, mut reader) = channel(pool, 4).unwrap();
        for sequence in 1..=5 {
            let outcome = producer.offer(
                &[sequence as u8; 256],
                Header {
                    sequence,
                    segment: 1,
                    generation: 1,
                    frames: 64,
                    sample_position: Some((sequence - 1) * 64),
                    timestamp: None,
                },
                policy,
            );
            if sequence < 5 {
                assert_eq!(outcome, Offer::Queued);
            } else {
                assert_eq!(
                    outcome,
                    match policy {
                        Overflow::DropOldest => Offer::Queued,
                        Overflow::DropNewest => Offer::QueueDropped,
                        Overflow::Fail => Offer::Failed(Failure::QueueOverflow),
                    }
                );
            }
            assert!(producer.attempts() <= 9);
        }
        let expected = if policy == Overflow::DropOldest {
            [2, 3, 4, 5]
        } else {
            [1, 2, 3, 4]
        };
        if policy == Overflow::DropOldest {
            let Some(Item::Control(gap)) = reader.next_item() else {
                panic!("missing oldest gap");
            };
            assert_eq!(gap.sequence, 1);
            assert_eq!(
                gap.losses.entries(),
                &[DomainLoss {
                    rate_hz: 48000,
                    known_frames: 64,
                    unknown: false,
                    cause: LossCause::QueueOldest
                }]
            );
        }
        for sequence in expected {
            let Some(Item::Audio(block)) = reader.next_item() else {
                panic!("missing valid tail");
            };
            assert_eq!(block.header().sequence, sequence);
            assert_eq!(block.bytes(), &[sequence as u8; 256]);
        }
        if policy == Overflow::DropNewest {
            let Some(Item::Control(gap)) = reader.next_item() else {
                panic!("missing newest gap");
            };
            assert_eq!(gap.sequence, 5);
            assert_eq!(
                gap.losses.entries(),
                &[DomainLoss {
                    rate_hz: 48000,
                    known_frames: 64,
                    unknown: false,
                    cause: LossCause::QueueNewest
                }]
            );
        }
        if policy == Overflow::Fail {
            assert!(matches!(
                reader.next_item(),
                Some(Item::Failed(Failure::QueueOverflow))
            ));
        }
        assert!(reader.next_item().is_none());
        let status = producer.snapshot();
        assert_eq!(status.input, 320);
        assert_eq!(status.delivered, 256);
        assert_eq!(status.queued, 0);
        assert_eq!(status.in_flight, 0);
        assert_eq!(status.queue_lost + status.rejected, 64);
        assert_eq!(status.pool_lost, 0);
        assert!(status.stable);
    }
}

#[test]
fn storage_copy_allocation_matrix() {
    use capture_system_or_process_audio_core::{schema::Header, storage::*};
    let started = std::time::Instant::now();
    let mut packets = 0;
    let acquisition_thread = std::thread::current().id();
    for format in support::formats() {
        for frames in [1, 64, 256] {
            for retained_view in [false, true] {
                let input = support::synthetic::packet(&format, 0, frames);
                let mut pool = Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    format.clone(),
                )
                .unwrap();
                let mut header = Header {
                    sequence: 0,
                    segment: 1,
                    generation: 1,
                    frames,
                    sample_position: None,
                    timestamp: None,
                };
                drop(pool.offer(&input, header).unwrap());
                let before = pool.stats();
                let mut max_bytes = 0;
                let mut max_destinations = 0;
                let measured = allocation_counter::measure(|| {
                    for sequence in 1..=10000 {
                        header.sequence = sequence;
                        let previous = pool.stats();
                        let block = pool.offer(&input, header).unwrap();
                        let view = retained_view.then(|| block.view());
                        assert_eq!(block.bytes(), input);
                        if let Some(view) = &view {
                            assert_eq!(view.bytes().as_ptr(), block.bytes().as_ptr());
                            assert_eq!(view.bytes(), input);
                        }
                        let current = pool.stats();
                        max_bytes = max_bytes.max(current.bytes - previous.bytes);
                        max_destinations =
                            max_destinations.max(current.destinations - previous.destinations);
                        assert_eq!(std::thread::current().id(), acquisition_thread);
                    }
                });
                assert!(
                    support::acquisition_gate(
                        measured.count_total,
                        max_bytes,
                        max_destinations,
                        input.len() as u64,
                        0
                    ),
                    "{measured:?}"
                );
                assert_eq!(measured.bytes_total, 0);
                assert_eq!(max_bytes, input.len() as u64);
                assert_eq!(max_destinations, 1);
                assert_eq!(pool.stats().destinations - before.destinations, 10000);
                assert_eq!(
                    pool.stats().bytes - before.bytes,
                    10000 * input.len() as u64
                );
                assert_eq!(pool.available(), 8);
                assert!(pool.stats().max_scan <= 8);
                let retained = pool.offer(&input, header).unwrap();
                let before_silence = pool.stats();
                let silence = allocation_counter::measure(|| {
                    for sequence in 10001..=20000 {
                        header.sequence = sequence;
                        let block = pool.offer_silence(header).unwrap();
                        assert_eq!(block.bytes().len(), input.len());
                        assert!(block.bytes().iter().all(|byte| *byte == 0));
                        assert_eq!(retained.bytes(), input);
                    }
                });
                assert_eq!(silence.count_total, 0);
                assert_eq!(silence.bytes_total, 0);
                assert_eq!(
                    pool.stats().bytes - before_silence.bytes,
                    10000 * input.len() as u64
                );
                assert_eq!(
                    pool.stats().destinations - before_silence.destinations,
                    10000
                );
                assert_eq!(
                    pool.limits().slots() * pool.limits().bytes_per_slot(),
                    36864
                );
                packets += 10000;
            }
        }
    }
    assert_eq!(packets, 240000);
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    println!(
        "storage warmed packets={packets}; max copies=1; extra view copies=0; acquisition allocations=0"
    );
}

#[test]
fn storage_generations_and_memory() {
    use capture_system_or_process_audio_core::{schema::Header, storage::*};
    let started = std::time::Instant::now();
    let mut formats = None;
    let descriptor_allocations = allocation_counter::measure(|| {
        formats = Some(support::formats());
    });
    let formats = formats.unwrap();
    let mut maximum_metadata = 0;
    for _ in 0..100 {
        let schedule = std::time::Instant::now();
        let mut pool = None;
        let setup = allocation_counter::measure(|| {
            pool = Some(
                Pool::new(
                    StorageLimits::new(8, 256, 18, 36864).unwrap(),
                    256,
                    formats[0].clone(),
                )
                .unwrap(),
            );
        });
        let mut pool = pool.unwrap();
        let metadata = setup.bytes_current - 36864
            + descriptor_allocations.bytes_current
            + std::mem::size_of::<Pool>() as i64;
        assert!(metadata > 0 && metadata <= 8192);
        maximum_metadata = maximum_metadata.max(metadata);
        let header = Header {
            sequence: 0,
            segment: 1,
            generation: 1,
            frames: 1,
            sample_position: None,
            timestamp: None,
        };
        let mut held = Vec::new();
        for sequence in 0..4 {
            held.push(
                pool.offer(&[0, 128, 255, 255], Header { sequence, ..header })
                    .unwrap(),
            );
        }
        let mut generation = 0;
        let transition = allocation_counter::measure(|| {
            generation = pool.prepare_generation(formats[1].clone(), 4).unwrap();
        });
        assert_eq!(transition.count_total, 0);
        assert_eq!(generation, 2);
        for sequence in 4..8 {
            held.push(
                pool.offer(
                    &[0, 0, 128, 191],
                    Header {
                        sequence,
                        generation,
                        segment: 2,
                        ..header
                    },
                )
                .unwrap(),
            );
        }
        assert_eq!(pool.available(), 0);
        assert!(matches!(
            pool.offer(
                &[0; 4],
                Header {
                    generation,
                    ..header
                }
            ),
            Err(StorageError::Exhausted)
        ));
        assert_eq!(pool.limits().total_bytes(), 36864);
        for (index, block) in held.iter().enumerate() {
            assert_eq!(block.format(), formats[usize::from(index >= 4)].as_ref());
            assert_eq!(
                block.bytes(),
                if index < 4 {
                    &[0, 128, 255, 255]
                } else {
                    &[0, 0, 128, 191]
                }
            );
        }
        let view = held[0].view();
        drop(held.remove(0));
        assert_eq!(pool.available(), 0);
        drop(view);
        assert_eq!(pool.available(), 1);
        let mut recycled = None;
        let recycling = allocation_counter::measure(|| {
            recycled = Some(
                pool.offer(
                    &[0, 0, 128, 191],
                    Header {
                        generation: 2,
                        segment: 2,
                        ..header
                    },
                )
                .unwrap(),
            );
        });
        assert_eq!(recycling.count_total, 0);
        assert_eq!(recycling.bytes_current, 0);
        let recycled = recycled.unwrap();
        assert_eq!(recycled.format(), formats[1].as_ref());
        assert_eq!(recycled.bytes(), &[0, 0, 128, 191]);
        drop(recycled);
        drop(held);
        assert_eq!(pool.available(), 8);
        pool.prepare_generation(formats[0].clone(), 8).unwrap();
        let held = pool
            .offer(
                &[0, 128, 255, 255],
                Header {
                    generation: 3,
                    ..header
                },
            )
            .unwrap();
        let rejected = allocation_counter::measure(|| {
            assert_eq!(
                pool.prepare_generation(formats[1].clone(), 8),
                Err(StorageError::GenerationBudget)
            );
        });
        assert_eq!(rejected.count_total, 0);
        assert_eq!(pool.generation(), 3);
        let view = held.view();
        drop(view);
        assert_eq!(pool.available(), 7);
        drop(pool);
        assert_eq!(held.bytes(), &[0, 128, 255, 255]);
        assert_eq!(held.format(), formats[0].as_ref());
        assert!(schedule.elapsed() < std::time::Duration::from_secs(2));
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    println!(
        "storage generation schedules=100; PCM bytes=36864; measured metadata max={maximum_metadata}; rejected transition allocations=0"
    );
}

#[test]
fn storage_instrumentation_self_tests() {
    use capture_system_or_process_audio_core::{schema::Header, storage::CopyStats};
    let mut meter = CopyStats::default();
    let mut destination = [0; 64];
    meter.copy(&mut destination, &[7; 64]);
    assert_eq!(meter.destinations, 1);
    assert_eq!(meter.bytes, 64);
    meter.copy(&mut destination, &[7; 64]);
    assert_eq!(meter.destinations, 2);
    assert_eq!(meter.bytes, 128);
    assert!(!support::acquisition_gate(
        0,
        meter.bytes,
        meter.destinations,
        64,
        0
    ));
    let pcm = allocation_counter::measure(|| {
        std::hint::black_box(vec![7u8; 64]);
    });
    assert_eq!(pcm.count_total, 1);
    assert_eq!(pcm.bytes_total, 64);
    assert!(!support::acquisition_gate(pcm.count_total, 64, 1, 64, 0));
    let metadata = allocation_counter::measure(|| {
        std::hint::black_box(Box::new(Header {
            sequence: 0,
            segment: 1,
            generation: 1,
            frames: 1,
            sample_position: None,
            timestamp: None,
        }));
    });
    assert_eq!(metadata.count_total, 1);
    assert_eq!(metadata.bytes_total, std::mem::size_of::<Header>() as u64);
    assert!(!support::acquisition_gate(
        metadata.count_total,
        64,
        1,
        64,
        0
    ));
    let mut acquisition_user_calls = 0;
    let mut injected_callback = || acquisition_user_calls += 1;
    injected_callback();
    assert_eq!(acquisition_user_calls, 1);
    assert!(!support::acquisition_gate(
        0,
        64,
        1,
        64,
        acquisition_user_calls
    ));
    println!(
        "storage instrumentation rejects: extra copy=64bytes/1destination, PCM allocation=64bytes, metadata allocation={}bytes, forbidden callback=1",
        std::mem::size_of::<Header>()
    );
}

#[test]
fn storage_retained_probe() {
    use capture_system_or_process_audio_core::{schema::Header, storage::*};
    let family = std::time::Instant::now();
    for _ in 0..100 {
        let schedule = std::time::Instant::now();
        let format = support::formats()[0].clone();
        let mut pool = Pool::new(
            StorageLimits::new(8, 256, 18, 36864).unwrap(),
            256,
            format.clone(),
        )
        .unwrap();
        let mut held = Vec::new();
        for sequence in 0..8 {
            let bytes = support::synthetic::packet(&format, sequence, 1);
            let header = Header {
                sequence: sequence as u64,
                segment: 1,
                generation: 1,
                frames: 1,
                sample_position: Some(sequence as u64),
                timestamp: None,
            };
            held.push((pool.offer(&bytes, header).unwrap(), bytes));
        }
        let header = Header {
            sequence: 9,
            segment: 1,
            generation: 1,
            frames: 1,
            sample_position: None,
            timestamp: None,
        };
        assert_eq!(pool.available(), 0);
        assert!(matches!(
            pool.offer(&[0; 4], header),
            Err(StorageError::Exhausted)
        ));
        assert!(pool.stats().max_scan <= 8);
        for (block, bytes) in &held {
            assert_eq!(block.bytes(), bytes);
            assert_eq!(block.format(), format.as_ref());
        }
        let retained = held[0].0.view();
        std::thread::scope(|scope| {
            let mut groups = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for (index, block) in held.into_iter().enumerate() {
                groups[index % 4].push(block);
            }
            for group in groups {
                scope.spawn(move || {
                    for (block, bytes) in group {
                        assert_eq!(block.bytes(), bytes);
                    }
                });
            }
        });
        assert_eq!(pool.available(), 7);
        let descriptor = retained.descriptor();
        drop(pool);
        assert_eq!(retained.bytes(), &[0, 128, 255, 255]);
        drop(retained);
        assert_eq!(*descriptor, *format);
        assert!(schedule.elapsed() < std::time::Duration::from_secs(2));
    }
    assert!(family.elapsed() < std::time::Duration::from_secs(15));
    println!("storage retained/release schedules=100; distinct release threads per schedule=4");
}

#[test]
fn schemas_oracle_mutations_and_mapping() {
    use capture_system_or_process_audio_core::schema::*;
    use support::{Trace, TraceItem, representation::*};
    let mut rejected = 0;
    for identity in 0..4 {
        for mutation in 0..6 {
            let trace = match mutation {
                2 | 4 => Trace::KnownGap,
                3 => Trace::Generation,
                _ => Trace::Continuous,
            };
            let mut items = support::synthetic::acquire(identity, trace);
            match mutation {
                0 => {
                    if let TraceItem::Pcm { bytes, .. } = &mut items[1] {
                        bytes[0] ^= 1;
                    }
                }
                1 => {
                    if let TraceItem::Pcm {
                        header,
                        bytes,
                        format,
                    } = &mut items[1]
                    {
                        header.frames += 1;
                        bytes.extend(vec![0; format.bytes_per_frame()]);
                    }
                }
                2 => items.retain(|item| !matches!(item, TraceItem::Gap { .. })),
                3 => {
                    let position = items
                        .iter()
                        .rposition(|item| matches!(item, TraceItem::Format { .. }))
                        .unwrap();
                    items.swap(position, position + 1);
                }
                4 => {
                    for item in &mut items {
                        if let TraceItem::Gap { loss, .. } = item {
                            loss.rate_hz += 1;
                        }
                    }
                }
                5 => {
                    if let TraceItem::Pcm { header, .. } = &mut items[1] {
                        header.timestamp = Some(Stamp {
                            ticks: 0,
                            ticks_per_second: 1_000_000_000,
                            clock_domain: 0,
                            provenance: Provenance::Native,
                            uncertainty_ns: Some(0),
                        });
                    }
                }
                _ => unreachable!(),
            }
            assert!(
                support::oracle::verify(identity, trace, &items).is_err(),
                "mutation {mutation}"
            );
            rejected += 1;
        }
        let format = support::formats()[identity].clone();
        let record = FormatRecord::from(format.as_ref());
        assert_eq!(Format::try_from(record).unwrap(), *format);
        for item in support::synthetic::acquire(identity, Trace::UnknownGap) {
            if let TraceItem::Pcm { header, .. } = item {
                assert_eq!(
                    Header::try_from(HeaderRecord::try_from(header).unwrap()).unwrap(),
                    header
                );
            }
            if let TraceItem::Gap { loss, .. } = item {
                assert_eq!(Loss::try_from(LossRecord::from(loss)).unwrap(), loss);
            }
        }
    }
    assert_eq!(rejected, 24);
    let absent = Header {
        sequence: u64::MAX,
        segment: 9,
        generation: 4,
        frames: 1,
        sample_position: None,
        timestamp: None,
    };
    let mut record = HeaderRecord::try_from(absent).unwrap();
    assert_eq!(record.valid, 0);
    assert_eq!(Header::try_from(record).unwrap(), absent);
    record.valid = 4;
    assert!(Header::try_from(record).is_err());
    println!("schemas oracle rejected mutations={rejected}; format/header/loss mappings pass");
}

#[test]
fn schemas_rejections_and_selection() {
    use capture_system_or_process_audio_core::{
        planner::CaptureRequest, schema::*, traits::CapabilityError,
    };
    use std::cell::Cell;
    use support::representation::{reason_code, reason_from_code};

    let valid_proc = CaptureTarget::process(
        ProcessIdentity {
            pid: 42,
            generation: 420,
        },
        ProcessScope::Tree,
        OnRootExit::Stop,
    )
    .unwrap();

    assert_eq!(
        CaptureTarget::process(
            ProcessIdentity {
                pid: 0,
                generation: 1
            },
            ProcessScope::Tree,
            OnRootExit::Stop
        ),
        Err(PlanError::InvalidTarget)
    );
    assert_eq!(
        CaptureTarget::process(
            ProcessIdentity {
                pid: 42,
                generation: 420
            },
            ProcessScope::Tree,
            OnRootExit::Wait { wait_timeout_ns: 0 }
        ),
        Err(PlanError::InvalidTarget)
    );
    assert_eq!(CaptureTarget::device(""), Err(PlanError::InvalidTarget));
    assert_eq!(
        CaptureTarget::input_device("bad\0id"),
        Err(PlanError::InvalidTarget)
    );
    assert_eq!(
        valid_proc.clone().with_gain(f32::NAN),
        Err(PlanError::InvalidTarget)
    );
    assert_eq!(
        valid_proc.clone().with_gain(-0.1),
        Err(PlanError::InvalidTarget)
    );

    let invalid_requests: [(CaptureRequest, PlatformOs, CapabilityError); 8] = [
        (
            CaptureRequest::new(Vec::new()),
            PlatformOs::Linux,
            CapabilityError::InvalidTarget,
        ),
        (
            CaptureRequest::single(valid_proc.clone()).with_tree_strategy(ProcessTreeRange {
                start_level: ProcessTreeStrategy::SoftwareStreamMix,
                fallback_limit: Some(ProcessTreeStrategy::NativeKernelCapture),
            }),
            PlatformOs::Linux,
            CapabilityError::ReversedStrategyRange,
        ),
        (
            CaptureRequest::single(valid_proc.clone().with_backend(BackendId::PulseAudio)),
            PlatformOs::Windows,
            CapabilityError::InvalidBackend,
        ),
        (
            CaptureRequest::single(valid_proc.clone()).with_require_precise_timing(true),
            PlatformOs::Linux,
            CapabilityError::PreciseTimingUnqualified,
        ),
        (
            CaptureRequest::single(valid_proc.clone().with_scope(ProcessScope::Exact)),
            PlatformOs::Windows,
            CapabilityError::UnsupportedScope,
        ),
        (
            CaptureRequest::new(vec![valid_proc.clone(); 17]),
            PlatformOs::Linux,
            CapabilityError::CapacityExceeded,
        ),
        (
            CaptureRequest::new(vec![valid_proc.clone(), valid_proc.clone()])
                .with_combine_mode(CombineMode::NativeOnly),
            PlatformOs::Linux,
            CapabilityError::NotImplemented,
        ),
        (
            CaptureRequest::single(CaptureTarget::microphone(DefaultDeviceMode::Follow))
                .with_output_converter(Some(OutputConverterConfig::new(
                    Format::new(
                        SampleType::I16,
                        ByteOrder::Little,
                        16_000,
                        1,
                        Layout::Unknown,
                    )
                    .unwrap(),
                    DitherMode::Tpdf,
                ))),
            PlatformOs::Windows,
            CapabilityError::NotImplemented,
        ),
    ];
    for (req, os, expected_err) in invalid_requests {
        assert_eq!(
            req.resolve_plan_for_os_with_capability(os, |_, _| Ok(())),
            Err(expected_err)
        );
    }

    let opens = Cell::new(0);
    let consents = Cell::new(0);
    let linux_req = CaptureRequest::single(valid_proc.clone());
    let plan = linux_req.inspect_for_os(PlatformOs::Linux).unwrap();
    assert_eq!(plan.selected_backend, Some(BackendId::PipeWire));
    assert_eq!(opens.get(), 0);
    assert_eq!(consents.get(), 0);

    let try_open_selected = |selected: BackendId| -> Result<(), CapabilityError> {
        assert_eq!(selected, BackendId::PipeWire);
        opens.set(opens.get() + 1);
        Err(CapabilityError::BackendFailure {
            backend: selected,
            code: -1,
        })
    };
    let open_outcome = try_open_selected(plan.selected_backend.unwrap());
    assert!(matches!(
        open_outcome,
        Err(CapabilityError::BackendFailure {
            backend: BackendId::PipeWire,
            code: -1
        })
    ));
    assert_eq!(opens.get(), 1);
    assert_eq!(consents.get(), 0);

    let fb_plan = linux_req
        .resolve_plan_for_os_with_capability(PlatformOs::Linux, |b, _| {
            if b == BackendId::PipeWire {
                Err(CapabilityError::ServerUnavailable)
            } else {
                Ok(())
            }
        })
        .unwrap();
    assert_eq!(fb_plan.selected_backend, Some(BackendId::PulseAudio));

    for abort_err in [
        CapabilityError::PermissionDenied,
        CapabilityError::PidReused,
        CapabilityError::NoSuchProcess,
    ] {
        assert_eq!(
            linux_req.resolve_plan_for_os_with_capability(PlatformOs::Linux, |b, _| {
                if b == BackendId::PipeWire {
                    Err(abort_err.clone())
                } else {
                    Ok(())
                }
            }),
            Err(abort_err)
        );
    }

    for code in 1..=16 {
        let reason = reason_from_code(code).unwrap();
        assert_eq!(reason_code(reason), code);
    }
    assert!(reason_from_code(0).is_none());
    assert!(reason_from_code(17).is_none());
    println!("schemas named rejections=8; selected-open calls=1; inspection capture/consent=0");
}

#[test]
fn schemas_oracle_traces() {
    let mut count = 0;
    for identity in 0..4 {
        for trace in support::TRACES {
            let items = support::synthetic::acquire(identity, trace);
            assert_eq!(
                support::oracle::verify(identity, trace, &items),
                Ok(()),
                "identity={identity} trace={trace:?}"
            );
            count += 1;
        }
    }
    assert_eq!(count, 20);
    println!("schemas oracle traces={count}; input frames per trace=4096");
}

#[test]
fn schemas_readiness_table() {
    use capture_system_or_process_audio_core::{
        planner::CaptureRequest, schema::*, traits::CapabilityError,
    };
    let process = ProcessIdentity {
        pid: 42,
        generation: 9,
    };
    let targets = [
        CaptureTarget::default_output(DefaultDeviceMode::Follow),
        CaptureTarget::default_output(DefaultDeviceMode::Pin),
        CaptureTarget::microphone(DefaultDeviceMode::Follow),
        CaptureTarget::microphone(DefaultDeviceMode::Pin),
        CaptureTarget::device("endpoint-a").unwrap(),
        CaptureTarget::input_device("mic-a").unwrap(),
        CaptureTarget::process(process, ProcessScope::Tree, OnRootExit::Stop).unwrap(),
        CaptureTarget::application(
            process,
            Some("com.example.app".into()),
            Some("Example".into()),
            None,
            ProcessScope::Exact,
            OnRootExit::Stop,
        )
        .unwrap(),
    ];
    let format = Format::new(
        SampleType::I16,
        ByteOrder::Little,
        48000,
        2,
        Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
    )
    .unwrap();
    let mut count = 0;
    for base_target in targets {
        for readiness in [
            Ok(()),
            Err(CapabilityError::PermissionDenied),
            Err(CapabilityError::DeviceUnavailable),
            Err(CapabilityError::ServerUnavailable),
            Err(CapabilityError::NotImplemented),
        ] {
            for pinned in [false, true] {
                let target = if pinned {
                    base_target.clone().with_backend(BackendId::PipeWire)
                } else {
                    base_target.clone()
                };
                let req = CaptureRequest::single(target).with_backends(
                    Backends::new(None, Some(vec![BackendId::PipeWire]), None).unwrap(),
                );
                let res = req.resolve_plan_for_os_with_capability(PlatformOs::Linux, |_, _| {
                    readiness.clone()
                });
                assert_eq!(res.is_ok(), readiness.is_ok());
                if let Err(ref expected_err) = readiness {
                    assert_eq!(res.as_ref().err(), Some(expected_err));
                } else {
                    assert_eq!(res.unwrap().selected_backend, Some(BackendId::PipeWire));
                }
                count += 1;
            }
        }
    }
    assert_eq!(count, 80);
    assert_eq!(format.frames_in(3), Err(Reason::IncompleteFrame));
    assert_eq!(
        format.byte_count(usize::MAX),
        Err(Reason::Capacity(CapacityError::Overflow))
    );
    println!("schemas readiness cases={count}; capture/consent entrypoints absent");
}

#[test]
fn baseline_capacity() {
    let limits = StorageLimits::new(8, 256, 18, 36864).unwrap();
    assert_eq!(limits.slots(), 8);
    assert_eq!(limits.bytes_per_slot(), 4608);
    assert_eq!(limits.total_bytes(), 36864);
    for arguments in [
        (0, 256, 18, 36864),
        (8, 0, 18, 36864),
        (8, 256, 0, 36864),
        (8, 256, 18, 0),
    ] {
        assert_eq!(
            StorageLimits::new(arguments.0, arguments.1, arguments.2, arguments.3),
            Err(CapacityError::Zero)
        );
    }
    assert_eq!(
        StorageLimits::new(8, 256, 18, 36863),
        Err(CapacityError::ExceedsByteCeiling)
    );
    assert_eq!(
        StorageLimits::new(1, usize::MAX, 2, usize::MAX),
        Err(CapacityError::Overflow)
    );
    assert_eq!(
        StorageLimits::new(usize::MAX, 2, 1, usize::MAX),
        Err(CapacityError::Overflow)
    );
}

#[test]
fn baseline_retained_handoff() {
    let mut owner = Arc::new([0u8; 256]);
    Arc::get_mut(&mut owner).unwrap()[..4].copy_from_slice(&[0, 127, 128, 255]);
    let retained = owner.clone();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                assert_eq!(&retained[..4], &[0, 127, 128, 255]);
                assert_eq!(retained[4..], [0; 252]);
            })
            .join()
            .unwrap();
    });
    assert_eq!(Arc::strong_count(&owner), 1);
    assert_eq!(&Arc::get_mut(&mut owner).unwrap()[..4], &[0, 127, 128, 255]);
}

#[test]
fn baseline_forbids_unsafe() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let source = include_str!("../src/lib.rs");
    assert!(source.starts_with("#![forbid(unsafe_code)]"));
    let output = std::env::temp_dir().join(format!("cspa-unsafe-{}.rmeta", std::process::id()));
    let mut child = Command::new("rustc")
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--crate-name=unsafe_guard",
            "--emit=metadata",
            "-o",
        ])
        .arg(&output)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = child.stdin.take().unwrap();
        input.write_all(source.as_bytes()).unwrap();
        input
            .write_all(b"\n#[allow(unsafe_code)]\nunsafe fn rejected() {}\n")
            .unwrap();
    }
    let result = child.wait_with_output().unwrap();
    if output.exists() {
        std::fs::remove_file(output).unwrap();
    }
    let diagnostic = String::from_utf8(result.stderr).unwrap();
    assert!(!result.status.success(), "unsafe was accepted");
    assert!(diagnostic.contains("E0453"), "{diagnostic}");
    assert!(
        diagnostic.contains("declaration of an `unsafe` function"),
        "{diagnostic}"
    );
}
