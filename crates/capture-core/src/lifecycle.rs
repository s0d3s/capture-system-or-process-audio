use crate::ordering::{ControlHandle, Failure, Item, Reader, Stop};
use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering::SeqCst},
    },
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum Delivery {
    Item(Item),
    End,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallbackError {
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionResult {
    Complete,
    Pending,
    Timeout,
    SameWorker,
}

struct Worker {
    identity: OnceLock<ThreadId>,
    handle: Mutex<Option<JoinHandle<()>>>,
    joined: AtomicBool,
    failure: AtomicU32,
}

#[derive(Clone)]
pub struct Completion {
    control: ControlHandle,
    worker: Option<Arc<Worker>>,
}
impl Completion {
    pub fn pull(control: ControlHandle) -> Self {
        Self {
            control,
            worker: None,
        }
    }
    pub fn callback_failure(&self) -> Option<Failure> {
        self.worker
            .as_ref()
            .and_then(|worker| crate::ordering::failure_from_code(worker.failure.load(SeqCst)))
    }
    fn on_worker(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| worker.identity.get() == Some(&thread::current().id()))
    }
    pub fn poll(&self, now_ns: u64, deadline_ns: u64) -> CompletionResult {
        if self.on_worker() {
            return CompletionResult::SameWorker;
        }
        let worker_done = self.worker.as_ref().is_none_or(|worker| {
            if worker.joined.load(SeqCst) {
                return true;
            }
            let mut handle = worker
                .handle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if handle.as_ref().is_some_and(JoinHandle::is_finished) {
                if handle.take().expect("finished worker").join().is_err() {
                    worker.failure.store(
                        crate::ordering::failure_code(Failure::CallbackPanic),
                        SeqCst,
                    );
                    self.control.fail(Failure::CallbackPanic);
                }
                worker.joined.store(true, SeqCst);
                true
            } else {
                worker.joined.load(SeqCst)
            }
        });
        if self.control.resources_done() && self.control.delivery_done() && worker_done {
            CompletionResult::Complete
        } else if now_ns >= deadline_ns {
            CompletionResult::Timeout
        } else {
            CompletionResult::Pending
        }
    }
    pub fn wait(&self, timeout: Duration) -> CompletionResult {
        let started = Instant::now();
        loop {
            let elapsed = started.elapsed();
            let result = self.poll(
                elapsed.as_nanos().min(u64::MAX as u128) as u64,
                timeout.as_nanos().min(u64::MAX as u128) as u64,
            );
            if result != CompletionResult::Pending {
                return result;
            }
            thread::park_timeout(
                timeout
                    .saturating_sub(elapsed)
                    .min(Duration::from_millis(1)),
            );
        }
    }
}

#[derive(Clone)]
pub struct CallbackContext {
    pub control: ControlHandle,
    pub completion: Completion,
}
pub struct CallbackStream {
    control: ControlHandle,
    completion: Completion,
}
impl CallbackStream {
    pub fn spawn(
        mut reader: Reader,
        mut callback: impl FnMut(Delivery, &CallbackContext) -> Result<(), CallbackError>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        let control = reader.control_handle();
        let worker = Arc::new(Worker {
            identity: OnceLock::new(),
            handle: Mutex::new(None),
            joined: AtomicBool::new(false),
            failure: AtomicU32::new(0),
        });
        let completion = Completion {
            control: control.clone(),
            worker: Some(worker.clone()),
        };
        let context = CallbackContext {
            control: control.clone(),
            completion: completion.clone(),
        };
        let worker_state = worker.clone();
        let handle = thread::Builder::new()
            .name("capture-delivery".into())
            .spawn(move || {
                let _ = worker_state.identity.set(thread::current().id());
                loop {
                    let (delivery, terminal) = match reader.next_item() {
                        Some(item) => {
                            let terminal = matches!(item, Item::Failed(_));
                            (Delivery::Item(item), terminal)
                        }
                        None if reader.ended() => (Delivery::End, true),
                        None => {
                            thread::park_timeout(Duration::from_millis(1));
                            continue;
                        }
                    };
                    let frames = match &delivery {
                        Delivery::Item(Item::Audio(block)) => block.header().frames,
                        _ => 0,
                    };
                    if frames != 0 {
                        reader.defer_delivery(frames);
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        callback(delivery, &context)
                    }));
                    if frames != 0 {
                        reader.finish_delivery(frames, false);
                    }
                    let failure = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(_)) => Some(Failure::Callback),
                        Err(_) => Some(Failure::CallbackPanic),
                    };
                    if let Some(failure) = failure {
                        worker_state
                            .failure
                            .store(crate::ordering::failure_code(failure), SeqCst);
                        context.control.fail(failure);
                        context.control.stop(Stop::Abort);
                        break;
                    }
                    if terminal {
                        break;
                    }
                }
                drop(callback);
                drop(reader);
            })?;
        *worker
            .handle
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
        Ok(Self {
            control,
            completion,
        })
    }
    pub fn stop(&self, mode: Stop) {
        self.control.stop(mode);
    }
    pub fn completion(&self) -> Completion {
        self.completion.clone()
    }
    pub fn control_handle(&self) -> ControlHandle {
        self.control.clone()
    }
}
impl Drop for CallbackStream {
    fn drop(&mut self) {
        self.control.stop(Stop::Abort);
    }
}
