use crate::{
    measurements::{Stage, Trace},
    ordering::{Failure, Stop},
    reads::{Cancellation, Mode, ReadError, ReadResult, ReadStream},
    schema::Format,
};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering::SeqCst},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    Stopped,
    TargetExited,
    Failed(Failure),
}

#[derive(Default)]
pub struct ReadySignal {
    lock: Mutex<u64>,
    cvar: Condvar,
}

impl ReadySignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify(&self) {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *guard = guard.wrapping_add(1);
        self.cvar.notify_all();
    }

    pub fn wait_timeout(&self, timeout: Duration) {
        let guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self
            .cvar
            .wait_timeout(guard, timeout.min(Duration::from_millis(20)));
    }
}

#[derive(Clone)]
pub struct StartupCleanup {
    stop: Arc<AtomicBool>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl StartupCleanup {
    pub fn new(stop: Arc<AtomicBool>, worker: JoinHandle<()>) -> Self {
        Self {
            stop,
            worker: Arc::new(Mutex::new(Some(worker))),
        }
    }

    pub fn cancel(&self) {
        self.stop.store(true, SeqCst);
    }

    pub fn is_complete(&self) -> bool {
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
        guard.is_none()
    }

    pub fn wait(&self, timeout: Duration) -> bool {
        self.cancel();
        let started = Instant::now();
        while !self.is_complete() && started.elapsed() < timeout {
            thread::sleep(Duration::from_millis(1));
        }
        self.is_complete()
    }
}

/// Deterministic FNV-1a endpoint clock-domain hash (`>= min_domain`, with `min_domain >= 2`
/// so hardware input/endpoint domains never collide with default output/process domain `1`).
pub fn endpoint_clock_domain(tag: &str, min_domain: u64) -> u64 {
    let floor = min_domain.max(2);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tag.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(floor)
}

/// Borrowed runtime context for backend `AudioStream` read loops (`0` allocations).
#[derive(Clone, Copy)]
pub struct StreamReadContext<'a> {
    pub reads: &'a ReadStream,
    pub ready: &'a ReadySignal,
    pub trace: Option<&'a Arc<Trace>>,
}

/// Polls queued packets before a zero-duration timeout and records `Stage::ReadReturned`
/// when tracing is active.
pub fn poll_read_stream(
    ctx: StreamReadContext<'_>,
    frames: usize,
    timeout: Duration,
    cancel: Cancellation,
) -> Result<ReadResult, ReadError> {
    let started = Instant::now();
    let blocking = !timeout.is_zero();
    let deadline = blocking.then(|| timeout.as_nanos().min(u64::MAX as u128) as u64);
    let mut operation = ctx
        .reads
        .begin(Mode::Variable, frames, blocking, deadline, cancel)?;
    loop {
        let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        if let Some(result) = operation.poll(elapsed_ns) {
            if let Some(tr) = ctx.trace
                && let Some(anchor) = result.last_packet
            {
                tr.record(Stage::ReadReturned, anchor.header.sequence);
            }
            return Ok(result);
        }
        ctx.ready
            .wait_timeout(timeout.saturating_sub(started.elapsed()));
    }
}

/// Shared blocking/non-blocking caller-destination `read_into` poll helper (`0` steady-state
/// heap allocations when paired with a cached `Cancellation` token).
pub fn poll_read_stream_into(
    ctx: StreamReadContext<'_>,
    format: Arc<Format>,
    destination: &mut [u8],
    frames: usize,
    timeout: Duration,
    cancel: Cancellation,
) -> Result<ReadResult, ReadError> {
    let started = Instant::now();
    let blocking = !timeout.is_zero();
    let deadline = blocking.then(|| timeout.as_nanos().min(u64::MAX as u128) as u64);
    let mut operation =
        ctx.reads
            .begin_into(destination, format, frames, blocking, deadline, cancel)?;
    loop {
        let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        if let Some(result) = operation.poll(elapsed_ns) {
            if let Some(tr) = ctx.trace
                && let Some(anchor) = result.last_packet
            {
                tr.record(Stage::ReadReturned, anchor.header.sequence);
            }
            return Ok(result);
        }
        ctx.ready
            .wait_timeout(timeout.saturating_sub(started.elapsed()));
    }
}

pub fn signal_stream_stop(
    control: &crate::ordering::ControlHandle,
    stop_flag: &AtomicBool,
    ready: &ReadySignal,
    mode: Stop,
) {
    control.stop(mode);
    stop_flag.store(true, SeqCst);
    ready.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        StorageLimits,
        ordering::{Overflow, channel},
        reads::PartialReason,
        schema::{ByteOrder, Channel, Header, Layout, SampleType},
        storage::Pool,
    };

    #[test]
    fn stream_runtime_nonblocking_zero_timeout_and_clock_domain_invariants() {
        let d_a = endpoint_clock_domain("usb-mic-0", 2);
        let d_b = endpoint_clock_domain("usb-mic-1", 2);
        let d_mac = endpoint_clock_domain("", 3);
        assert!(d_a >= 2);
        assert!(d_b >= 2);
        assert_ne!(d_a, d_b);
        assert!(d_mac >= 3);
        assert_eq!(endpoint_clock_domain("usb-mic-0", 2), d_a);

        let fmt = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .unwrap(),
        );
        let pool = Pool::new(
            StorageLimits::new(4, 64, 8, 4 * 64 * 8).unwrap(),
            64,
            fmt.clone(),
        )
        .unwrap();
        let (mut producer, reader) = channel(pool, 4).unwrap();
        let reads = ReadStream::new(reader);
        let ready = ReadySignal::new();
        let cancel = Cancellation::default();
        let ctx = StreamReadContext {
            reads: &reads,
            ready: &ready,
            trace: None,
        };

        let empty_res = poll_read_stream(ctx, 16, Duration::ZERO, cancel.clone()).unwrap();
        assert_eq!(empty_res.frames, 0);
        assert_eq!(empty_res.reason, PartialReason::WouldBlock);

        let payload = [0u8; 16 * 8];
        producer.offer(
            &payload,
            Header {
                sequence: 1,
                segment: 1,
                generation: 1,
                frames: 16,
                sample_position: Some(0),
                timestamp: None,
            },
            Overflow::DropOldest,
        );
        let mut dst = [0u8; 16 * 8];
        let allocs = allocation_counter::measure(|| {
            let into_res = poll_read_stream_into(
                ctx,
                fmt.clone(),
                &mut dst,
                16,
                Duration::ZERO,
                cancel.clone(),
            )
            .unwrap();
            assert_eq!(into_res.frames, 16);
            assert_eq!(into_res.reason, PartialReason::Complete);
        });
        assert_eq!(allocs.count_total, 0);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let handle = thread::spawn(move || {
            while !stop_bg.load(SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
        });
        let cleanup = StartupCleanup::new(stop, handle);
        assert!(cleanup.wait(Duration::from_millis(500)));
        assert!(cleanup.is_complete());
    }
}
