#![deny(unsafe_op_in_unsafe_fn)]

pub mod enums;
#[cfg(test)]
pub mod stubgen;
pub mod synthetic;
pub mod types;

use cspa_facade::{
    AudioStream, Backends, BoundaryKind, ByteOrder, Cancellation, CapabilityError, CaptureManager,
    CaptureRequest, CaptureTarget, Channel, CombineMode, DefaultDeviceMode, Discovery,
    DiscoveryDedupe, DiscoveryEventKind, DiscoveryOrder, DiscoveryQuery, DiscoverySubscription,
    Failure, Format, Header, InputConversionPolicy, Layout, MixedBlockMetadata, MixerConfig,
    OnFormatChange, OnInputFailure, Overflow, PartialReason, PermissionPolicy, PlatformOs,
    ProcessIdentity, ProcessScope, ProcessTreeRange, ReadResult, SampleType, Stop, StorageLimits,
    TimingRequirement,
    core::{
        discovery::{
            BackendRawInventory, DeviceDirection, DiscoveryService, RawBackendDeviceRecord,
            RawBackendProcessRecord, RawProcessRecord,
        },
        ordering,
        reads::ReadStream,
        storage::Pool,
    },
};
use enums::{
    PartialReasonTag, ParticipationStateTag, PyBackendId, PyCombineMode, PyDefaultDeviceMode,
    PyDiscoveryDedupe, PyDiscoveryOrder, PyFailureKind, PyInputConversionPolicy, PyOnFormatChange,
    PyOnInputFailure, PyOverflow, PyPermissionPolicy, PyPlatformOs, PyProcessScope,
    PyRootExitPolicy, PySampleType, PyStopMode, PyTimingRequirement, PyVisibility,
    RootExitPolicyTag,
};
use pyo3::{
    exceptions::{
        PyBufferError, PyRuntimeError, PyStopAsyncIteration, PyStopIteration, PyValueError,
    },
    ffi,
    prelude::*,
    sync::{PyOnceLock, critical_section::with_critical_section},
    types::PyDict,
};
use std::{
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use synthetic::{PortableMultiState, SyntheticSegmentedManager};
use types::{
    DestinationBuffer, PermissionKindsInput, PyAggregateManagerBudgets, PyBackends, PyCancellation,
    PyCapturePlanReport, PyCaptureTarget, PyDiscoveryEvent, PyDiscoveryQuery, PyFormat,
    PyInputStatusHandle, PyMixerConfig, PyOutputConverter, PyPermissionReport, PyProcessTreeRange,
    PyReadOutcome, PyTargetCatalog, PyTargetSnapshot, TargetsInput, TreeStrategyInput,
    build_on_root_exit, build_py_outcome, cap_err_to_py_err, plan_err_to_py_err, to_py_err,
};

const PREALLOCATED_READ_INTO_SCRATCH_BYTES: usize = 65_536;

static COROUTINE_WRAPPER: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

pub(crate) fn coroutine_wrapper(py: Python<'_>) -> PyResult<&'static Py<PyAny>> {
    COROUTINE_WRAPPER.get_or_try_init(py, || {
        let globals = PyDict::new(py);
        let code = c"async def _cspa_await_future(fut):\n    return await fut\n";
        py.run(code, Some(&globals), None)?;
        let helper = globals
            .get_item("_cspa_await_future")?
            .ok_or_else(|| to_py_err("missing _cspa_await_future helper"))?;
        Ok(helper.unbind())
    })
}

fn wrap_future_as_coroutine<'py>(
    py: Python<'py>,
    future: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    coroutine_wrapper(py)?.bind(py).call1((future,))
}

fn resolve_permission_policy(
    permission_policy: Option<PyPermissionPolicy>,
    request_permissions: Option<bool>,
) -> PyResult<PermissionPolicy> {
    match (permission_policy.map(|p| p.0), request_permissions) {
        (None, None) => Ok(PermissionPolicy::AutoRequest),
        (Some(p), None) => Ok(p),
        (None, Some(true)) => Ok(PermissionPolicy::AutoRequest),
        (None, Some(false)) => Ok(PermissionPolicy::CheckOnly),
        (Some(PermissionPolicy::AutoRequest), Some(true)) => Ok(PermissionPolicy::AutoRequest),
        (Some(PermissionPolicy::CheckOnly), Some(false)) => Ok(PermissionPolicy::CheckOnly),
        (Some(_), Some(_)) => Err(PyValueError::new_err(
            "conflicting permission_policy and request_permissions arguments",
        )),
    }
}

#[pyclass(
    name = "_AsyncCancelHook",
    module = "capture_system_or_process_audio",
    frozen
)]
struct PyAsyncCancelHook {
    wait_cancel: Cancellation,
    claim_released: Arc<AtomicBool>,
    active_readers: Arc<AtomicUsize>,
}

#[pymethods]
impl PyAsyncCancelHook {
    fn __call__(&self, fut: &Bound<'_, PyAny>) -> PyResult<()> {
        let is_cancelled = fut
            .call_method0("cancelled")
            .and_then(|v| v.extract::<bool>())
            .unwrap_or(false);
        if is_cancelled {
            self.wait_cancel.cancel();
            if !self.claim_released.swap(true, SeqCst) {
                self.active_readers.fetch_sub(1, SeqCst);
            }
        }
        Ok(())
    }
}

#[pyclass(
    name = "_LoopCallThunk",
    module = "capture_system_or_process_audio",
    frozen
)]
struct PyLoopCallThunk {
    func: Py<PyAny>,
    arg: Py<PyAny>,
}

#[pymethods]
impl PyLoopCallThunk {
    fn __call__(&self, py: Python<'_>) -> PyResult<()> {
        let f = self.func.bind(py);
        let a = self.arg.bind(py);
        let _ = f.call1((a,));
        Ok(())
    }
}

fn schedule_future_completion(
    py: Python<'_>,
    loop_obj: &Bound<'_, PyAny>,
    fut_obj: &Bound<'_, PyAny>,
    outcome: PyResult<Py<PyAny>>,
) -> PyResult<()> {
    let is_done = fut_obj
        .call_method0("done")
        .and_then(|v| v.extract::<bool>())
        .unwrap_or(true);
    if is_done {
        return Ok(());
    }
    match outcome {
        Ok(value) => {
            let setter = fut_obj.getattr("set_result")?.unbind();
            let thunk = Py::new(
                py,
                PyLoopCallThunk {
                    func: setter,
                    arg: value,
                },
            )?;
            let _ = loop_obj.call_method1("call_soon_threadsafe", (thunk,));
        }
        Err(err) => {
            let setter = fut_obj.getattr("set_exception")?.unbind();
            let exc_val = err.into_value(py).into_any();
            let thunk = Py::new(
                py,
                PyLoopCallThunk {
                    func: setter,
                    arg: exc_val,
                },
            )?;
            let _ = loop_obj.call_method1("call_soon_threadsafe", (thunk,));
        }
    }
    Ok(())
}

struct ReaderClaimGuard {
    active_readers: Arc<AtomicUsize>,
    released: bool,
}

impl ReaderClaimGuard {
    fn acquire(
        active_readers: &Arc<AtomicUsize>,
        callback_active: &Arc<AtomicBool>,
    ) -> PyResult<Self> {
        if callback_active.load(SeqCst) {
            return Err(PyRuntimeError::new_err(
                "already_pending: callback consumer is active",
            ));
        }
        active_readers
            .compare_exchange(0, 1, SeqCst, SeqCst)
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "already_pending: another read or async wait is in progress",
                )
            })?;
        Ok(Self {
            active_readers: active_readers.clone(),
            released: false,
        })
    }
}

impl Drop for ReaderClaimGuard {
    fn drop(&mut self) {
        if !self.released {
            self.active_readers.fetch_sub(1, SeqCst);
            self.released = true;
        }
    }
}

/// Pins a writable C-contiguous `Py_buffer` without creating an aliased `&mut [u8]` over Python memory.
struct WriteBufferGuard {
    view: ffi::Py_buffer,
}

impl WriteBufferGuard {
    fn acquire(obj: &Bound<'_, PyAny>) -> PyResult<Self> {
        // SAFETY: `Py_buffer` is a plain C struct zero-initialized prior to `PyObject_GetBuffer`.
        let mut view = unsafe { std::mem::zeroed::<ffi::Py_buffer>() };
        // SAFETY: `obj` is a valid attached Python object; requests writable C-contiguous export.
        let status = unsafe {
            ffi::PyObject_GetBuffer(
                obj.as_ptr(),
                &mut view,
                ffi::PyBUF_WRITABLE | ffi::PyBUF_C_CONTIGUOUS,
            )
        };
        if status != 0 {
            return Err(PyErr::fetch(obj.py()));
        }
        if view.readonly != 0 || view.buf.is_null() || view.len <= 0 {
            // SAFETY: `PyObject_GetBuffer` succeeded above, so `view` must be released before erring.
            unsafe {
                ffi::PyBuffer_Release(&mut view);
            }
            return Err(PyBufferError::new_err(
                "destination buffer must be writable, non-empty, and C-contiguous",
            ));
        }
        Ok(Self { view })
    }

    #[inline]
    fn len(&self) -> usize {
        self.view.len as usize
    }

    #[inline]
    fn buf_mut_ptr(&self) -> *mut u8 {
        self.view.buf.cast::<u8>()
    }
}

impl Drop for WriteBufferGuard {
    fn drop(&mut self) {
        // SAFETY: `WriteBufferGuard` is only created and dropped while attached to Python.
        unsafe {
            ffi::PyBuffer_Release(&mut self.view);
        }
    }
}

#[pyclass(
    name = "DiscoverySubscription",
    module = "capture_system_or_process_audio",
    frozen
)]
pub struct PyDiscoverySubscription {
    sub: Mutex<Option<DiscoverySubscription>>,
    _owner: Option<Discovery>,
}

#[pymethods]
impl PyDiscoverySubscription {
    #[getter]
    fn id(&self) -> PyResult<u64> {
        let guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        Ok(guard.as_ref().map_or(0, DiscoverySubscription::id))
    }

    #[getter]
    fn pending_len(&self) -> PyResult<usize> {
        let guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        Ok(guard.as_ref().map_or(0, DiscoverySubscription::pending_len))
    }

    #[getter]
    fn is_overflowed(&self) -> PyResult<bool> {
        let guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        Ok(guard
            .as_ref()
            .is_some_and(DiscoverySubscription::is_overflowed))
    }

    fn poll_event(&self) -> PyResult<Option<PyDiscoveryEvent>> {
        let guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        Ok(guard
            .as_ref()
            .and_then(DiscoverySubscription::poll_event)
            .map(PyDiscoveryEvent::from))
    }

    fn poll_events(&self) -> PyResult<Vec<PyDiscoveryEvent>> {
        let guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        let mut out = Vec::new();
        if let Some(sub) = guard.as_ref() {
            while let Some(ev) = sub.poll_event() {
                out.push(PyDiscoveryEvent::from(ev));
            }
        }
        Ok(out)
    }

    fn resnapshot(&self) -> PyResult<PyTargetSnapshot> {
        let guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        let sub = guard
            .as_ref()
            .ok_or_else(|| to_py_err("subscription is closed"))?;
        let snap = sub.resnapshot().map_err(to_py_err)?;
        Ok(PyTargetSnapshot::from(snap))
    }

    fn close(&self) -> PyResult<()> {
        let mut guard = self
            .sub
            .lock()
            .map_err(|_| to_py_err("subscription lock poisoned"))?;
        *guard = None;
        Ok(())
    }

    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = self.close();
        Ok(false)
    }
}

#[pyclass(
    name = "DiscoveryService",
    module = "capture_system_or_process_audio",
    frozen
)]
pub struct PyDiscoveryService {
    service: DiscoveryService,
}

#[pymethods]
impl PyDiscoveryService {
    #[new]
    #[pyo3(signature = (os=None))]
    fn new(os: Option<PyPlatformOs>) -> Self {
        Self {
            service: DiscoveryService::new_for_os(os.map_or_else(PlatformOs::current, |o| o.0)),
        }
    }

    #[getter]
    fn os(&self) -> PyPlatformOs {
        PyPlatformOs(self.service.os())
    }

    #[getter]
    fn active_subscriber_count(&self) -> usize {
        self.service.active_subscriber_count()
    }

    #[getter]
    fn current_revision(&self) -> u64 {
        self.service.current_revision()
    }

    #[getter]
    fn default_output_device_id(&self) -> Option<String> {
        self.service
            .default_output_device_id()
            .map(|id| id.to_string())
    }

    #[getter]
    fn default_input_device_id(&self) -> Option<String> {
        self.service
            .default_input_device_id()
            .map(|id| id.to_string())
    }

    fn seed_demo_inventory(&self) -> PyResult<u64> {
        let os = self.service.os();
        let primary_backend = Backends::default_primary_for_os(os);
        let raw_procs = [
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 1000,
                    generation: 10,
                },
                parent_pid: None,
                app_id: Some("com.example.browser".into()),
                name: Some("ExampleBrowser".into()),
                executable_path: Some("C:\\Apps\\browser.exe".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 1001,
                    generation: 20,
                },
                parent_pid: Some(1000),
                app_id: None,
                name: Some("BrowserAudioHelper".into()),
                executable_path: Some("C:\\Apps\\browser_helper.exe".into()),
                active_audio: Some(true),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 2000,
                    generation: 30,
                },
                parent_pid: None,
                app_id: Some("com.example.music".into()),
                name: Some("MusicPlayer".into()),
                executable_path: Some("C:\\Apps\\music.exe".into()),
                active_audio: Some(false),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 3000,
                    generation: 40,
                },
                parent_pid: None,
                app_id: Some("com.example.daemon".into()),
                name: Some("SilentDaemon".into()),
                executable_path: Some("C:\\Apps\\daemon.exe".into()),
                active_audio: None,
            },
        ];
        let processes: Vec<RawBackendProcessRecord> = raw_procs
            .into_iter()
            .map(|process| RawBackendProcessRecord {
                backend: primary_backend,
                process,
            })
            .collect();
        let devices = vec![
            RawBackendDeviceRecord {
                backend: primary_backend,
                direction: DeviceDirection::Output,
                device_id: "endpoint-default".into(),
                name: Some("Default Speakers".into()),
                is_default: true,
                is_monitor_sink: false,
            },
            RawBackendDeviceRecord {
                backend: primary_backend,
                direction: DeviceDirection::Input,
                device_id: "mic-default".into(),
                name: Some("Default Microphone".into()),
                is_default: true,
                is_monitor_sink: false,
            },
            RawBackendDeviceRecord {
                backend: primary_backend,
                direction: DeviceDirection::Input,
                device_id: "endpoint-default.monitor".into(),
                name: Some("Monitor of Default Speakers".into()),
                is_default: false,
                is_monitor_sink: true,
            },
        ];
        let query = DiscoveryQuery::all_for_os(os);
        self.service
            .update_backend_inventory(BackendRawInventory { processes, devices }, &query)
            .map_err(to_py_err)
    }

    #[pyo3(signature = (active_only=false, include_non_audio=false))]
    fn applications(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> PyResult<Vec<PyCaptureTarget>> {
        let list = self
            .service
            .applications(active_only, include_non_audio)
            .map_err(to_py_err)?;
        Ok(list
            .into_iter()
            .map(|inner| PyCaptureTarget { inner })
            .collect())
    }

    #[pyo3(signature = (active_only=false, include_non_audio=false))]
    fn processes(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> PyResult<Vec<PyCaptureTarget>> {
        let list = self
            .service
            .processes(active_only, include_non_audio)
            .map_err(to_py_err)?;
        Ok(list
            .into_iter()
            .map(|inner| PyCaptureTarget { inner })
            .collect())
    }

    fn output_devices(&self) -> PyResult<Vec<PyCaptureTarget>> {
        let list = self.service.output_devices().map_err(to_py_err)?;
        Ok(list
            .into_iter()
            .map(|inner| PyCaptureTarget { inner })
            .collect())
    }

    fn input_devices(&self) -> PyResult<Vec<PyCaptureTarget>> {
        let list = self.service.input_devices().map_err(to_py_err)?;
        Ok(list
            .into_iter()
            .map(|inner| PyCaptureTarget { inner })
            .collect())
    }

    #[pyo3(signature = (
        backends=None,
        dedupe=PyDiscoveryDedupe(DiscoveryDedupe::ByLogicalTarget),
        order=PyDiscoveryOrder(DiscoveryOrder::KindThenBackend),
        active_only=false,
        include_non_audio=false,
        include_inputs=true
    ))]
    fn discover(
        &self,
        backends: Option<PyBackends>,
        dedupe: PyDiscoveryDedupe,
        order: PyDiscoveryOrder,
        active_only: bool,
        include_non_audio: bool,
        include_inputs: bool,
    ) -> PyResult<PyTargetCatalog> {
        let query = DiscoveryQuery {
            os: self.service.os(),
            backends: backends.map_or_else(Backends::default, |b| b.inner),
            dedupe: dedupe.0,
            order: order.0,
            active_only,
            include_non_audio,
            include_applications: true,
            include_processes: true,
            include_output_devices: true,
            include_input_devices: include_inputs,
        };
        let catalog = self.service.discover_query(&query).map_err(to_py_err)?;
        Ok(PyTargetCatalog { inner: catalog })
    }

    fn discover_query(&self, query: &PyDiscoveryQuery) -> PyResult<PyTargetCatalog> {
        let catalog = self
            .service
            .discover_query(&query.inner)
            .map_err(to_py_err)?;
        Ok(PyTargetCatalog { inner: catalog })
    }

    #[pyo3(signature = (
        query,
        scope=PyProcessScope(ProcessScope::Tree),
        on_root_exit=PyRootExitPolicy(RootExitPolicyTag::Stop),
        wait_timeout_ms=5000,
        backend=None,
        gain=1.0
    ))]
    fn find_app(
        &self,
        query: &str,
        scope: PyProcessScope,
        on_root_exit: PyRootExitPolicy,
        wait_timeout_ms: u64,
        backend: Option<PyBackendId>,
        gain: f32,
    ) -> PyResult<PyCaptureTarget> {
        let target = self
            .service
            .find_app(
                query,
                scope.0,
                build_on_root_exit(on_root_exit.0, wait_timeout_ms)?,
            )
            .map_err(to_py_err)?
            .with_optional_backend(backend.map(|b| b.0))
            .with_gain(gain)
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(PyCaptureTarget { inner: target })
    }

    #[pyo3(signature = (
        backends=None,
        dedupe=PyDiscoveryDedupe(DiscoveryDedupe::ByLogicalTarget),
        order=PyDiscoveryOrder(DiscoveryOrder::KindThenBackend),
        active_only=false,
        include_non_audio=false,
        include_inputs=true,
        max_discovery_queue=256
    ))]
    #[allow(clippy::too_many_arguments)]
    fn subscribe(
        &self,
        backends: Option<PyBackends>,
        dedupe: PyDiscoveryDedupe,
        order: PyDiscoveryOrder,
        active_only: bool,
        include_non_audio: bool,
        include_inputs: bool,
        max_discovery_queue: usize,
    ) -> PyResult<PyDiscoverySubscription> {
        let query = DiscoveryQuery {
            os: self.service.os(),
            backends: backends.map_or_else(Backends::default, |b| b.inner),
            dedupe: dedupe.0,
            order: order.0,
            active_only,
            include_non_audio,
            include_applications: true,
            include_processes: true,
            include_output_devices: true,
            include_input_devices: include_inputs,
        };
        let sub = self
            .service
            .subscribe_query(query, max_discovery_queue)
            .map_err(to_py_err)?;
        Ok(PyDiscoverySubscription {
            sub: Mutex::new(Some(sub)),
            _owner: None,
        })
    }

    #[pyo3(signature = (query, max_discovery_queue=256))]
    fn subscribe_query(
        &self,
        query: &PyDiscoveryQuery,
        max_discovery_queue: usize,
    ) -> PyResult<PyDiscoverySubscription> {
        let sub = self
            .service
            .subscribe_query(query.inner.clone(), max_discovery_queue)
            .map_err(to_py_err)?;
        Ok(PyDiscoverySubscription {
            sub: Mutex::new(Some(sub)),
            _owner: None,
        })
    }

    fn publish_synthetic_events(&self, count: usize) -> u64 {
        let mut last_rev = self.service.current_revision();
        for idx in 0..count {
            last_rev = self.service.publish_event(DiscoveryEventKind::Removed(
                format!("process:{idx}:1").into(),
            ));
        }
        last_rev
    }

    fn publish_watcher_failed(&self, reason: &str) -> u64 {
        self.service
            .publish_event(DiscoveryEventKind::WatcherFailed {
                reason: reason.into(),
            })
    }

    fn publish_visibility_changed(&self, visibility: PyVisibility) -> u64 {
        self.service
            .publish_event(DiscoveryEventKind::VisibilityChanged(visibility.0))
    }

    fn publish_default_input_changed(&self, device_id: &str) -> u64 {
        self.service
            .publish_event(DiscoveryEventKind::DefaultInputChanged {
                device_id: device_id.into(),
            })
    }
}

#[pyclass(
    name = "AudioStream",
    module = "capture_system_or_process_audio",
    frozen
)]
pub struct PyAudioStream {
    inner: Arc<Mutex<AudioStream>>,
    format: Format,
    cleanup_timeout: Duration,
    active_readers: Arc<AtomicUsize>,
    callback_active: Arc<AtomicBool>,
    read_into_scratch: Arc<Mutex<Vec<u8>>>,
    last_cleanup_error: Arc<Mutex<Option<String>>>,
    last_callback_error: Arc<Mutex<Option<String>>>,
}

#[pymethods]
impl PyAudioStream {
    #[staticmethod]
    #[pyo3(signature = (
        target,
        tree_strategy=None,
        backends=None,
        overflow=PyOverflow(Overflow::DropOldest),
        timing=PyTimingRequirement(TimingRequirement::Raw),
        cleanup_timeout_ms=2000,
        permission_policy=None,
        request_permissions=None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn open(
        target: &PyCaptureTarget,
        tree_strategy: Option<TreeStrategyInput>,
        backends: Option<PyBackends>,
        overflow: PyOverflow,
        timing: PyTimingRequirement,
        cleanup_timeout_ms: u64,
        permission_policy: Option<PyPermissionPolicy>,
        request_permissions: Option<bool>,
    ) -> PyResult<Self> {
        let cleanup_timeout = Duration::from_millis(cleanup_timeout_ms);
        if !(Duration::from_millis(100)..=Duration::from_secs(30)).contains(&cleanup_timeout) {
            return Err(PyValueError::new_err(
                "cleanup_timeout_ms must be between 100 and 30000",
            ));
        }
        let policy = resolve_permission_policy(permission_policy, request_permissions)?;
        let range = tree_strategy.map_or_else(ProcessTreeRange::default, |t| t.0);
        let bk = backends.map_or_else(Backends::default, |b| b.inner);
        let stream = cspa_facade::open_stream_with_permission_policy(
            &target.inner,
            range,
            &bk,
            overflow.0,
            timing.0,
            policy,
        )
        .map_err(cap_err_to_py_err)?;
        let format = (**stream.format()).clone();
        Ok(Self {
            inner: Arc::new(Mutex::new(stream)),
            format,
            cleanup_timeout,
            active_readers: Arc::new(AtomicUsize::new(0)),
            callback_active: Arc::new(AtomicBool::new(false)),
            read_into_scratch: Arc::new(Mutex::new(vec![
                0u8;
                PREALLOCATED_READ_INTO_SCRATCH_BYTES
            ])),
            last_cleanup_error: Arc::new(Mutex::new(None)),
            last_callback_error: Arc::new(Mutex::new(None)),
        })
    }

    #[getter]
    fn format(&self) -> PyFormat {
        PyFormat {
            inner: self.format.clone(),
        }
    }

    #[getter]
    fn active_reader_count(&self) -> usize {
        self.active_readers.load(SeqCst)
    }

    #[getter]
    fn last_cleanup_error(&self) -> Option<String> {
        self.last_cleanup_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[getter]
    fn last_callback_error(&self) -> Option<String> {
        self.last_callback_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[pyo3(signature = (frames=480, timeout_ms=200, cancel=None))]
    fn read<'py>(
        &self,
        py: Python<'py>,
        frames: usize,
        timeout_ms: u64,
        cancel: Option<&PyCancellation>,
    ) -> PyResult<PyReadOutcome> {
        let _claim = ReaderClaimGuard::acquire(&self.active_readers, &self.callback_active)?;
        let cancel_tok = cancel.map_or_else(Cancellation::default, |c| c.inner.clone());
        let timeout = Duration::from_millis(timeout_ms);
        let inner = self.inner.clone();
        let result = py
            .detach(move || -> Result<ReadResult, String> {
                let mut guard = inner.lock().map_err(|_| "stream lock poisoned")?;
                guard
                    .read_with_cancel(frames, timeout, cancel_tok)
                    .map_err(|e| format!("{e:?}"))
            })
            .map_err(to_py_err)?;
        build_py_outcome(py, 1, 1, None, None, result)
    }

    #[pyo3(signature = (destination, frames, timeout_ms=200, cancel=None))]
    fn read_into<'py>(
        &self,
        py: Python<'py>,
        destination: DestinationBuffer<'py>,
        frames: usize,
        timeout_ms: u64,
        cancel: Option<&PyCancellation>,
    ) -> PyResult<PyReadOutcome> {
        let _claim = ReaderClaimGuard::acquire(&self.active_readers, &self.callback_active)?;
        let dst_obj = destination.0;
        let guard = WriteBufferGuard::acquire(&dst_obj)?;
        let dst_len = guard.len();
        let cancel_tok = cancel.map_or_else(Cancellation::default, |c| c.inner.clone());
        let timeout = Duration::from_millis(timeout_ms);
        let inner = self.inner.clone();
        let scratch = self.read_into_scratch.clone();
        let (copied_bytes, result) = py
            .detach(move || -> Result<(usize, ReadResult), String> {
                let mut scratch_guard = scratch.lock().map_err(|_| "scratch lock poisoned")?;
                if scratch_guard.len() < dst_len {
                    scratch_guard.resize(dst_len, 0);
                }
                let mut stream_guard = inner.lock().map_err(|_| "stream lock poisoned")?;
                let res = stream_guard
                    .read_into_with_cancel(
                        &mut scratch_guard[..dst_len],
                        frames,
                        timeout,
                        cancel_tok,
                    )
                    .map_err(|e| format!("{e:?}"))?;
                let copied = (res.copies.bytes as usize).min(dst_len);
                Ok((copied, res))
            })
            .map_err(to_py_err)?;

        if copied_bytes > 0 {
            let scratch_guard = self
                .read_into_scratch
                .lock()
                .map_err(|_| to_py_err("scratch lock poisoned"))?;
            let src_ptr = scratch_guard.as_ptr();
            let dst_ptr = guard.buf_mut_ptr();
            with_critical_section(&dst_obj, || {
                // SAFETY: `guard` pins `dst_obj`'s writable C-contiguous `Py_buffer` of at least
                // `dst_len >= copied_bytes` bytes; `src_ptr` points to disjoint Rust-owned scratch
                // memory; `with_critical_section` serializes object access under free-threading
                // without ever forming an aliased Rust `&mut [u8]` reference over Python memory.
                unsafe {
                    ptr::copy_nonoverlapping(src_ptr, dst_ptr, copied_bytes);
                }
            });
        }
        drop(guard);
        build_py_outcome(py, 1, 1, None, None, result)
    }

    #[pyo3(signature = (mode=PyStopMode(Stop::Drain)))]
    fn stop(&self, mode: PyStopMode) -> PyResult<()> {
        self.inner
            .lock()
            .map_err(|_| to_py_err("stream lock poisoned"))?
            .stop(mode.0);
        Ok(())
    }

    fn wait_cleanup(&self, py: Python<'_>) -> PyResult<bool> {
        let inner = self.inner.clone();
        let timeout = self.cleanup_timeout;
        py.detach(move || -> Result<bool, String> {
            let guard = inner.lock().map_err(|_| "stream lock poisoned")?;
            Ok(guard.wait_cleanup(timeout))
        })
        .map_err(to_py_err)
    }

    fn __iter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<PyReadOutcome> {
        let out = self.read(py, 480, 200, None)?;
        if out.reason == PartialReasonTag::Eof && out.frames == 0 {
            return Err(PyStopIteration::new_err(()));
        }
        Ok(out)
    }

    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    #[pyo3(signature = (exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = self.stop(PyStopMode(Stop::Abort));
        let cleaned = self.wait_cleanup(py).unwrap_or(false);
        if !cleaned {
            let msg =
                "cleanup_timeout: resources still active after cleanup_timeout_ms".to_string();
            *self
                .last_cleanup_error
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
            if exc_type.is_none_or(|e| e.is_none()) {
                return Err(PyRuntimeError::new_err(msg));
            }
        }
        Ok(false)
    }
}

#[pyclass(
    name = "CaptureRequest",
    module = "capture_system_or_process_audio",
    frozen,
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct PyCaptureRequest {
    pub inner: CaptureRequest,
}

#[pymethods]
impl PyCaptureRequest {
    #[getter]
    fn targets(&self) -> Vec<PyCaptureTarget> {
        self.inner
            .targets()
            .iter()
            .cloned()
            .map(|inner| PyCaptureTarget { inner })
            .collect()
    }

    #[getter]
    fn combine_mode(&self) -> PyCombineMode {
        PyCombineMode(self.inner.combine_mode())
    }

    #[getter]
    fn tree_strategy(&self) -> PyProcessTreeRange {
        PyProcessTreeRange {
            inner: self.inner.tree_strategy(),
        }
    }

    #[getter]
    fn input_conversion(&self) -> PyInputConversionPolicy {
        PyInputConversionPolicy(self.inner.input_conversion())
    }

    #[getter]
    fn backends(&self) -> PyBackends {
        PyBackends {
            inner: self.inner.backends().clone(),
        }
    }

    #[getter]
    fn output_converter(&self) -> Option<PyOutputConverter> {
        self.inner
            .output_converter()
            .cloned()
            .map(|inner| PyOutputConverter { inner })
    }

    #[getter]
    fn on_format_change(&self) -> PyOnFormatChange {
        PyOnFormatChange(self.inner.on_format_change())
    }

    #[getter]
    fn mixer_config(&self) -> PyMixerConfig {
        PyMixerConfig {
            inner: *self.inner.mixer_config(),
        }
    }

    #[getter]
    fn budgets(&self) -> PyAggregateManagerBudgets {
        PyAggregateManagerBudgets {
            inner: *self.inner.budgets(),
        }
    }

    #[getter]
    fn allow_reroute(&self) -> bool {
        self.inner.allow_reroute()
    }

    #[getter]
    fn timing(&self) -> PyTimingRequirement {
        PyTimingRequirement(if self.inner.require_precise_timing() {
            TimingRequirement::Precise
        } else {
            TimingRequirement::Raw
        })
    }

    #[getter]
    fn permission_policy(&self) -> PyPermissionPolicy {
        PyPermissionPolicy(self.inner.permission_policy())
    }

    fn with_permission_policy(&self, policy: PyPermissionPolicy) -> Self {
        Self {
            inner: self.inner.clone().with_permission_policy(policy.0),
        }
    }

    fn inspect(&self) -> PyResult<PyCapturePlanReport> {
        self.inner
            .inspect()
            .map(|inner| PyCapturePlanReport { inner })
            .map_err(plan_err_to_py_err)
    }

    fn inspect_for_os(&self, os: PyPlatformOs) -> PyResult<PyCapturePlanReport> {
        self.inner
            .inspect_for_os(os.0)
            .map(|inner| PyCapturePlanReport { inner })
            .map_err(plan_err_to_py_err)
    }

    fn check_permissions(&self) -> PyResult<PyPermissionReport> {
        cspa_facade::check_request_permissions(&self.inner)
            .map(|inner| PyPermissionReport { inner })
            .map_err(cap_err_to_py_err)
    }

    #[pyo3(signature = (timeout_ms=30000, open_settings_if_denied=false))]
    fn request_permissions(
        &self,
        py: Python<'_>,
        timeout_ms: u64,
        open_settings_if_denied: bool,
    ) -> PyResult<PyPermissionReport> {
        let req = self.inner.clone();
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let inner = py
            .detach(move || {
                cspa_facade::request_for_capture_request(&req, timeout, open_settings_if_denied)
            })
            .map_err(cap_err_to_py_err)?;
        Ok(PyPermissionReport { inner })
    }

    #[pyo3(signature = (timeout_ms=30000, open_settings_if_denied=false))]
    fn request_permissions_async<'py>(
        &self,
        py: Python<'py>,
        timeout_ms: u64,
        open_settings_if_denied: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let asyncio = py.import("asyncio")?;
        let loop_obj = asyncio.call_method0("get_running_loop")?;
        let fut = loop_obj.call_method0("create_future")?;
        let req = self.inner.clone();
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let loop_py = loop_obj.clone().unbind();
        let fut_py = fut.clone().unbind();

        thread::spawn(move || {
            let res =
                cspa_facade::request_for_capture_request(&req, timeout, open_settings_if_denied);
            Python::attach(|py| {
                let loop_bound = loop_py.bind(py);
                let fut_bound = fut_py.bind(py);
                let py_outcome = match res {
                    Ok(inner) => Py::new(py, PyPermissionReport { inner }).map(Py::into_any),
                    Err(err) => Err(cap_err_to_py_err(err)),
                };
                let _ = schedule_future_completion(py, loop_bound, fut_bound, py_outcome);
            });
        });

        wrap_future_as_coroutine(py, &fut)
    }

    #[pyo3(signature = (
        overflow=PyOverflow(Overflow::DropOldest),
        timing=None
    ))]
    fn open(
        &self,
        overflow: PyOverflow,
        timing: Option<PyTimingRequirement>,
    ) -> PyResult<PyManagedStream> {
        let resolved_timing = timing.map_or_else(
            || {
                if self.inner.require_precise_timing() {
                    TimingRequirement::Precise
                } else {
                    TimingRequirement::Raw
                }
            },
            |t| t.0,
        );
        open_managed_from_request(self.inner.clone(), overflow.0, resolved_timing)
    }
}

enum ManagedBackend {
    Native {
        manager: Box<Mutex<CaptureManager>>,
        cleanup_timeout: Duration,
    },
    SyntheticSegmented(Mutex<SyntheticSegmentedManager>),
    SyntheticMulti(Arc<PortableMultiState>),
}

type BackendReadOutcome = (
    u64,
    u64,
    Option<BoundaryKind>,
    Option<MixedBlockMetadata>,
    ReadResult,
);

impl ManagedBackend {
    fn execute_read(
        &self,
        frames: usize,
        timeout_ms: u64,
        cancel_tok: Cancellation,
    ) -> Result<BackendReadOutcome, String> {
        let timeout = Duration::from_millis(timeout_ms);
        match self {
            Self::Native { manager, .. } => {
                let mut guard = manager.lock().map_err(|_| "manager lock poisoned")?;
                let res = guard
                    .read_with_cancel(frames, timeout, cancel_tok)
                    .map_err(|e| format!("{e:?}"))?;
                let bkind =
                    (res.reason == PartialReason::Boundary).then_some(BoundaryKind::DeviceChanged);
                Ok((
                    guard.session_id(),
                    guard.segment_id(),
                    bkind,
                    guard.last_block_metadata(),
                    res,
                ))
            }
            Self::SyntheticSegmented(mgr) => {
                let mut guard = mgr.lock().map_err(|_| "synthetic lock poisoned")?;
                let (sess, seg, bkind, res) =
                    guard.poll_read(None, frames, timeout_ms, cancel_tok)?;
                Ok((sess, seg, bkind, None, res))
            }
            Self::SyntheticMulti(state) => state.poll_read(None, frames, cancel_tok),
        }
    }

    fn execute_read_into(
        &self,
        dst: &mut [u8],
        frames: usize,
        timeout_ms: u64,
        cancel_tok: Cancellation,
    ) -> Result<(usize, BackendReadOutcome), String> {
        let timeout = Duration::from_millis(timeout_ms);
        match self {
            Self::Native { manager, .. } => {
                let mut guard = manager.lock().map_err(|_| "manager lock poisoned")?;
                let res = guard
                    .read_into_with_cancel(dst, frames, timeout, cancel_tok)
                    .map_err(|e| format!("{e:?}"))?;
                let copied = (res.copies.bytes as usize).min(dst.len());
                let bkind =
                    (res.reason == PartialReason::Boundary).then_some(BoundaryKind::DeviceChanged);
                Ok((
                    copied,
                    (
                        guard.session_id(),
                        guard.segment_id(),
                        bkind,
                        guard.last_block_metadata(),
                        res,
                    ),
                ))
            }
            Self::SyntheticSegmented(mgr) => {
                let dst_len = dst.len();
                let mut guard = mgr.lock().map_err(|_| "synthetic lock poisoned")?;
                let (sess, seg, bkind, res) =
                    guard.poll_read(Some(dst), frames, timeout_ms, cancel_tok)?;
                let copied = (res.copies.bytes as usize).min(dst_len);
                Ok((copied, (sess, seg, bkind, None, res)))
            }
            Self::SyntheticMulti(state) => {
                let dst_len = dst.len();
                let out = state.poll_read(Some(dst), frames, cancel_tok)?;
                let copied = (out.4.copies.bytes as usize).min(dst_len);
                Ok((copied, out))
            }
        }
    }

    fn stop(&self, mode: Stop) -> Result<(), String> {
        match self {
            Self::Native { manager, .. } => {
                manager
                    .lock()
                    .map_err(|_| "manager lock poisoned".to_string())?
                    .stop(mode);
                Ok(())
            }
            Self::SyntheticSegmented(mgr) => {
                mgr.lock()
                    .map_err(|_| "synthetic lock poisoned".to_string())?
                    .stop(mode);
                Ok(())
            }
            Self::SyntheticMulti(state) => {
                state.stop(mode);
                Ok(())
            }
        }
    }

    fn wait_cleanup(&self) -> Result<bool, String> {
        match self {
            Self::Native {
                manager,
                cleanup_timeout,
            } => Ok(manager
                .lock()
                .map_err(|_| "manager lock poisoned".to_string())?
                .wait_cleanup(*cleanup_timeout)),
            Self::SyntheticSegmented(mgr) => Ok(mgr
                .lock()
                .map_err(|_| "synthetic lock poisoned".to_string())?
                .wait_cleanup()),
            Self::SyntheticMulti(_) => Ok(true),
        }
    }
}

struct CallbackWorkerControl {
    stop_flag: Arc<AtomicBool>,
    cb_cancel: Cancellation,
    join_handle: Option<JoinHandle<()>>,
}

#[pyclass(
    name = "ManagedStream",
    module = "capture_system_or_process_audio",
    frozen
)]
pub struct PyManagedStream {
    backend: Arc<ManagedBackend>,
    active_readers: Arc<AtomicUsize>,
    callback_active: Arc<AtomicBool>,
    callback_worker: Arc<Mutex<Option<CallbackWorkerControl>>>,
    read_into_scratch: Arc<Mutex<Vec<u8>>>,
    last_cleanup_error: Arc<Mutex<Option<String>>>,
    last_callback_error: Arc<Mutex<Option<String>>>,
    callback_invocations: Arc<AtomicUsize>,
}

impl PyManagedStream {
    fn from_backend(backend: ManagedBackend) -> Self {
        Self {
            backend: Arc::new(backend),
            active_readers: Arc::new(AtomicUsize::new(0)),
            callback_active: Arc::new(AtomicBool::new(false)),
            callback_worker: Arc::new(Mutex::new(None)),
            read_into_scratch: Arc::new(Mutex::new(vec![
                0u8;
                PREALLOCATED_READ_INTO_SCRATCH_BYTES
            ])),
            last_cleanup_error: Arc::new(Mutex::new(None)),
            last_callback_error: Arc::new(Mutex::new(None)),
            callback_invocations: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn join_callback_worker(&self) {
        // Release the worker lock before joining to avoid deadlock during `Python::attach`.
        let handle_opt = if let Ok(mut guard) = self.callback_worker.lock() {
            guard.take().and_then(|mut ctrl| {
                ctrl.stop_flag.store(true, SeqCst);
                ctrl.cb_cancel.cancel();
                ctrl.join_handle.take()
            })
        } else {
            None
        };
        if let Some(handle) = handle_opt
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
        self.callback_active.store(false, SeqCst);
    }
}

#[pymethods]
impl PyManagedStream {
    #[getter]
    fn plan(&self) -> PyResult<Option<PyCapturePlanReport>> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(Some(PyCapturePlanReport {
                    inner: guard.plan().clone(),
                }))
            }
            ManagedBackend::SyntheticSegmented(_) | ManagedBackend::SyntheticMulti(_) => Ok(None),
        }
    }

    #[getter]
    fn working_format(&self) -> PyResult<Option<PyFormat>> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(guard.working_format().map(|f| PyFormat {
                    inner: (**f).clone(),
                }))
            }
            ManagedBackend::SyntheticSegmented(_) => Ok(None),
            ManagedBackend::SyntheticMulti(state) => Ok(Some(PyFormat {
                inner: (**state.manager.working_format()).clone(),
            })),
        }
    }

    #[getter]
    fn output_format(&self) -> PyResult<Option<PyFormat>> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(guard.output_format().map(|f| PyFormat {
                    inner: (**f).clone(),
                }))
            }
            ManagedBackend::SyntheticSegmented(_) => Ok(None),
            ManagedBackend::SyntheticMulti(state) => Ok(Some(PyFormat {
                inner: (**state.manager.output_format()).clone(),
            })),
        }
    }

    #[getter]
    fn selected_backend(&self) -> PyResult<PyBackendId> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(PyBackendId(guard.selected_backend()))
            }
            ManagedBackend::SyntheticSegmented(_) | ManagedBackend::SyntheticMulti(_) => Ok(
                PyBackendId(Backends::default_primary_for_os(PlatformOs::current())),
            ),
        }
    }

    #[getter]
    fn is_software_mix(&self) -> PyResult<bool> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(guard.is_software_mix())
            }
            ManagedBackend::SyntheticSegmented(_) => Ok(false),
            ManagedBackend::SyntheticMulti(_) => Ok(true),
        }
    }

    #[getter]
    fn session_id(&self) -> PyResult<u64> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(guard.session_id())
            }
            ManagedBackend::SyntheticSegmented(_) | ManagedBackend::SyntheticMulti(_) => Ok(1),
        }
    }

    #[getter]
    fn segment_id(&self) -> PyResult<u64> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(guard.segment_id())
            }
            ManagedBackend::SyntheticSegmented(_) | ManagedBackend::SyntheticMulti(_) => Ok(1),
        }
    }

    #[getter]
    fn targets(&self) -> PyResult<Vec<PyCaptureTarget>> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                Ok(guard
                    .targets()
                    .iter()
                    .cloned()
                    .map(|inner| PyCaptureTarget { inner })
                    .collect())
            }
            ManagedBackend::SyntheticSegmented(_) | ManagedBackend::SyntheticMulti(_) => {
                Ok(Vec::new())
            }
        }
    }

    #[getter]
    fn active_reader_count(&self) -> usize {
        self.active_readers.load(SeqCst) + usize::from(self.callback_active.load(SeqCst))
    }

    #[getter]
    fn last_cleanup_error(&self) -> Option<String> {
        self.last_cleanup_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[getter]
    fn last_callback_error(&self) -> Option<String> {
        self.last_callback_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[getter]
    fn callback_invocations(&self) -> usize {
        self.callback_invocations.load(SeqCst)
    }

    fn input_handles(&self) -> PyResult<Vec<PyInputStatusHandle>> {
        match &*self.backend {
            ManagedBackend::Native { manager, .. } => {
                let guard = manager
                    .lock()
                    .map_err(|_| to_py_err("manager lock poisoned"))?;
                let handles = guard.input_handles();
                if !handles.is_empty() {
                    return Ok(handles.iter().map(PyInputStatusHandle::from).collect());
                }
                let fallback_fmt = guard
                    .output_format()
                    .or_else(|| guard.working_format())
                    .map(|f| (**f).clone())
                    .unwrap_or_else(|| {
                        Format::new(
                            SampleType::F32,
                            ByteOrder::Little,
                            48_000,
                            2,
                            Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
                        )
                        .unwrap()
                    });
                let bk = guard.selected_backend();
                Ok(guard
                    .targets()
                    .iter()
                    .enumerate()
                    .map(|(idx, t)| PyInputStatusHandle {
                        target_index: idx,
                        target_id: t.id().to_string(),
                        backend: t.backend().unwrap_or(bk),
                        participation_state: ParticipationStateTag::Active,
                        failure_code: None,
                        native_format: fallback_fmt.clone(),
                        mixer_format: fallback_fmt.clone(),
                        is_resampled: false,
                        is_identity_bypass: true,
                        queued_packets: 0,
                        dropped_frames: 0,
                        clipped_samples: 0,
                        nonfinite_samples: 0,
                        drift_slips: 0,
                    })
                    .collect())
            }
            ManagedBackend::SyntheticMulti(state) => Ok(state
                .manager
                .input_handles()
                .iter()
                .map(PyInputStatusHandle::from)
                .collect()),
            ManagedBackend::SyntheticSegmented(_) => {
                let fmt = Format::new(
                    SampleType::F32,
                    ByteOrder::Little,
                    48_000,
                    2,
                    Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
                )
                .unwrap();
                Ok(vec![PyInputStatusHandle {
                    target_index: 0,
                    target_id: "synthetic:0".to_string(),
                    backend: Backends::default_primary_for_os(PlatformOs::current()),
                    participation_state: ParticipationStateTag::Active,
                    failure_code: None,
                    native_format: fmt.clone(),
                    mixer_format: fmt,
                    is_resampled: false,
                    is_identity_bypass: true,
                    queued_packets: 0,
                    dropped_frames: 0,
                    clipped_samples: 0,
                    nonfinite_samples: 0,
                    drift_slips: 0,
                }])
            }
        }
    }

    fn update_targets(&self, py: Python<'_>, target: TargetsInput) -> PyResult<u64> {
        let new_targets = target.0;
        let backend = self.backend.clone();
        py.detach(move || match &*backend {
            ManagedBackend::Native { manager, .. } => {
                let mut guard = manager.lock().map_err(|_| CapabilityError::Stopped)?;
                guard.update_targets(new_targets)
            }
            ManagedBackend::SyntheticSegmented(_) | ManagedBackend::SyntheticMulti(_) => {
                Err(CapabilityError::NotImplemented)
            }
        })
        .map_err(cap_err_to_py_err)
    }

    #[pyo3(signature = (frames=480, timeout_ms=200, cancel=None))]
    fn read<'py>(
        &self,
        py: Python<'py>,
        frames: usize,
        timeout_ms: u64,
        cancel: Option<&PyCancellation>,
    ) -> PyResult<PyReadOutcome> {
        let _claim = ReaderClaimGuard::acquire(&self.active_readers, &self.callback_active)?;
        let cancel_tok = cancel.map_or_else(Cancellation::default, |c| c.inner.clone());
        let backend = self.backend.clone();
        let (session, segment, boundary_kind, mixed_meta, result) = py
            .detach(move || backend.execute_read(frames, timeout_ms, cancel_tok))
            .map_err(to_py_err)?;
        build_py_outcome(py, session, segment, boundary_kind, mixed_meta, result)
    }

    #[pyo3(signature = (destination, frames, timeout_ms=200, cancel=None))]
    fn read_into<'py>(
        &self,
        py: Python<'py>,
        destination: DestinationBuffer<'py>,
        frames: usize,
        timeout_ms: u64,
        cancel: Option<&PyCancellation>,
    ) -> PyResult<PyReadOutcome> {
        let _claim = ReaderClaimGuard::acquire(&self.active_readers, &self.callback_active)?;
        let dst_obj = destination.0;
        let guard = WriteBufferGuard::acquire(&dst_obj)?;
        let dst_len = guard.len();
        let cancel_tok = cancel.map_or_else(Cancellation::default, |c| c.inner.clone());
        let backend = self.backend.clone();
        let scratch = self.read_into_scratch.clone();
        let (copied_bytes, (session, segment, boundary_kind, mixed_meta, result)) = py
            .detach(move || -> Result<(usize, BackendReadOutcome), String> {
                let mut scratch_guard = scratch.lock().map_err(|_| "scratch lock poisoned")?;
                if scratch_guard.len() < dst_len {
                    scratch_guard.resize(dst_len, 0);
                }
                backend.execute_read_into(
                    &mut scratch_guard[..dst_len],
                    frames,
                    timeout_ms,
                    cancel_tok,
                )
            })
            .map_err(to_py_err)?;

        if copied_bytes > 0 {
            let scratch_guard = self
                .read_into_scratch
                .lock()
                .map_err(|_| to_py_err("scratch lock poisoned"))?;
            let src_ptr = scratch_guard.as_ptr();
            let dst_ptr = guard.buf_mut_ptr();
            with_critical_section(&dst_obj, || {
                // SAFETY: `guard` pins `dst_obj`'s writable C-contiguous `Py_buffer` of at least
                // `dst_len >= copied_bytes` bytes; `src_ptr` points to disjoint Rust-owned scratch
                // memory; `with_critical_section` serializes object access under free-threading
                // without ever forming an aliased Rust `&mut [u8]` reference over Python memory.
                unsafe {
                    ptr::copy_nonoverlapping(src_ptr, dst_ptr, copied_bytes);
                }
            });
        }
        drop(guard);
        build_py_outcome(py, session, segment, boundary_kind, mixed_meta, result)
    }

    #[pyo3(signature = (frames=480, timeout_ms=200, cancel=None))]
    fn read_async<'py>(
        &self,
        py: Python<'py>,
        frames: usize,
        timeout_ms: u64,
        cancel: Option<&PyCancellation>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let asyncio = py.import("asyncio")?;
        let loop_obj = asyncio.call_method0("get_running_loop")?;
        let fut = loop_obj.call_method0("create_future")?;

        if self.callback_active.load(SeqCst)
            || self
                .active_readers
                .compare_exchange(0, 1, SeqCst, SeqCst)
                .is_err()
        {
            let _ = fut.call_method1(
                "set_exception",
                (PyRuntimeError::new_err(
                    "already_pending: another read or callback is active",
                ),),
            );
            return wrap_future_as_coroutine(py, &fut);
        }

        let wait_cancel = Cancellation::default();
        let caller_cancel = cancel.map(|c| c.inner.clone());
        let claim_released = Arc::new(AtomicBool::new(false));
        let hook = Py::new(
            py,
            PyAsyncCancelHook {
                wait_cancel: wait_cancel.clone(),
                claim_released: claim_released.clone(),
                active_readers: self.active_readers.clone(),
            },
        )?;
        fut.call_method1("add_done_callback", (hook,))?;

        let backend = self.backend.clone();
        let active_readers = self.active_readers.clone();
        let loop_py = loop_obj.clone().unbind();
        let fut_py = fut.clone().unbind();

        thread::spawn(move || {
            let combined_cancel = Cancellation::default();
            let step_ms = 5u64;
            let mut elapsed_ms = 0u64;
            let res = loop {
                if wait_cancel.is_cancelled()
                    || caller_cancel
                        .as_ref()
                        .is_some_and(Cancellation::is_cancelled)
                {
                    combined_cancel.cancel();
                }
                let slice_timeout = if combined_cancel.is_cancelled() || timeout_ms == 0 {
                    0
                } else {
                    step_ms.min(timeout_ms.saturating_sub(elapsed_ms))
                };
                let step = backend.execute_read(frames, slice_timeout, combined_cancel.clone());
                match step {
                    Ok((sess, seg, bkind, meta, read_res)) => {
                        if read_res.reason == PartialReason::Timeout
                            && !combined_cancel.is_cancelled()
                            && elapsed_ms + slice_timeout < timeout_ms
                        {
                            elapsed_ms += slice_timeout;
                            continue;
                        }
                        break Ok((sess, seg, bkind, meta, read_res));
                    }
                    Err(e) => break Err(e),
                }
            };

            if !claim_released.swap(true, SeqCst) {
                active_readers.fetch_sub(1, SeqCst);
            }

            Python::attach(|py| {
                let loop_bound = loop_py.bind(py);
                let fut_bound = fut_py.bind(py);
                let py_outcome = match res {
                    Ok((sess, seg, bkind, meta, read_res)) => {
                        build_py_outcome(py, sess, seg, bkind, meta, read_res)
                            .and_then(|o| Py::new(py, o).map(Py::into_any))
                    }
                    Err(msg) => Err(to_py_err(msg)),
                };
                let _ = schedule_future_completion(py, loop_bound, fut_bound, py_outcome);
            });
        });

        wrap_future_as_coroutine(py, &fut)
    }

    #[pyo3(signature = (callback, frames=480, timeout_ms=50))]
    fn start_callback(
        &self,
        py: Python<'_>,
        callback: Py<PyAny>,
        frames: usize,
        timeout_ms: u64,
    ) -> PyResult<()> {
        if !callback.bind(py).is_callable() {
            return Err(PyValueError::new_err("callback must be callable"));
        }
        if self.active_readers.load(SeqCst) > 0
            || self
                .callback_active
                .compare_exchange(false, true, SeqCst, SeqCst)
                .is_err()
        {
            return Err(PyRuntimeError::new_err(
                "already_pending: another reader or callback is active",
            ));
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_flag.clone();
        let cb_cancel = Cancellation::default();
        let cb_cancel_clone = cb_cancel.clone();
        let backend = self.backend.clone();
        let callback_active = self.callback_active.clone();
        let last_cb_err = self.last_callback_error.clone();
        let cb_invocations = self.callback_invocations.clone();

        let join_handle = thread::Builder::new()
            .name("cspa-py-callback".into())
            .spawn(move || {
                while !stop_clone.load(SeqCst) {
                    let step = backend.execute_read(frames, timeout_ms, cb_cancel_clone.clone());
                    match step {
                        Ok((sess, seg, bkind, meta, read_res)) => {
                            let is_eof = (read_res.reason == PartialReason::Eof
                                || read_res.reason == PartialReason::Cancelled)
                                && read_res.frames == 0;
                            if read_res.frames == 0
                                && matches!(
                                    read_res.reason,
                                    PartialReason::Timeout
                                        | PartialReason::WouldBlock
                                        | PartialReason::Cancelled
                                )
                            {
                                if stop_clone.load(SeqCst) {
                                    break;
                                }
                                continue;
                            }
                            let invoke_ok = Python::attach(|py| -> bool {
                                let Ok(outcome) =
                                    build_py_outcome(py, sess, seg, bkind, meta, read_res)
                                else {
                                    return false;
                                };
                                let Ok(py_outcome) = Py::new(py, outcome) else {
                                    return false;
                                };
                                cb_invocations.fetch_add(1, SeqCst);
                                match callback.bind(py).call1((py_outcome,)) {
                                    Ok(_) => true,
                                    Err(err) => {
                                        *last_cb_err.lock().unwrap_or_else(|e| e.into_inner()) =
                                            Some(format!("callback_failure: {err}"));
                                        let _ = backend.stop(Stop::Abort);
                                        false
                                    }
                                }
                            });
                            if !invoke_ok || is_eof {
                                break;
                            }
                        }
                        Err(err) => {
                            *last_cb_err.lock().unwrap_or_else(|e| e.into_inner()) = Some(err);
                            break;
                        }
                    }
                }
                callback_active.store(false, SeqCst);
            })
            .map_err(to_py_err)?;

        *self
            .callback_worker
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(CallbackWorkerControl {
            stop_flag,
            cb_cancel,
            join_handle: Some(join_handle),
        });
        Ok(())
    }

    fn stop_callback(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            self.join_callback_worker();
        });
        Ok(())
    }

    #[pyo3(signature = (mode=PyStopMode(Stop::Drain)))]
    fn stop(&self, py: Python<'_>, mode: PyStopMode) -> PyResult<()> {
        let res = self.backend.stop(mode.0);
        if mode.0 == Stop::Abort {
            py.detach(|| {
                self.join_callback_worker();
            });
        }
        res.map_err(to_py_err)
    }

    fn wait_cleanup(&self, py: Python<'_>) -> PyResult<bool> {
        let backend = self.backend.clone();
        py.detach(|| {
            self.join_callback_worker();
            backend.wait_cleanup()
        })
        .map_err(to_py_err)
    }

    fn __iter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<PyReadOutcome> {
        let out = self.read(py, 480, 200, None)?;
        if out.reason == PartialReasonTag::Eof && out.frames == 0 {
            return Err(PyStopIteration::new_err(()));
        }
        Ok(out)
    }

    fn __aiter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let asyncio = py.import("asyncio")?;
        let loop_obj = asyncio.call_method0("get_running_loop")?;
        let fut = loop_obj.call_method0("create_future")?;

        if self.callback_active.load(SeqCst)
            || self
                .active_readers
                .compare_exchange(0, 1, SeqCst, SeqCst)
                .is_err()
        {
            let _ = fut.call_method1(
                "set_exception",
                (PyRuntimeError::new_err("already_pending"),),
            );
            return wrap_future_as_coroutine(py, &fut);
        }

        let backend = self.backend.clone();
        let active_readers = self.active_readers.clone();
        let loop_py = loop_obj.clone().unbind();
        let fut_py = fut.clone().unbind();

        thread::spawn(move || {
            let cancel = Cancellation::default();
            let res = backend.execute_read(480, 200, cancel);
            active_readers.fetch_sub(1, SeqCst);
            Python::attach(|py| {
                let loop_bound = loop_py.bind(py);
                let fut_bound = fut_py.bind(py);
                let outcome = match res {
                    Ok((_sess, _seg, _bkind, _meta, read_res))
                        if read_res.reason == PartialReason::Eof && read_res.frames == 0 =>
                    {
                        Err(PyStopAsyncIteration::new_err(()))
                    }
                    Ok((sess, seg, bkind, meta, read_res)) => {
                        build_py_outcome(py, sess, seg, bkind, meta, read_res)
                            .and_then(|o| Py::new(py, o).map(Py::into_any))
                    }
                    Err(msg) => Err(to_py_err(msg)),
                };
                let _ = schedule_future_completion(py, loop_bound, fut_bound, outcome);
            });
        });

        wrap_future_as_coroutine(py, &fut)
    }

    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    #[pyo3(signature = (exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = self.stop(py, PyStopMode(Stop::Abort));
        let cleaned = self.wait_cleanup(py).unwrap_or(false);
        if !cleaned {
            let msg =
                "cleanup_timeout: resources still active after cleanup_timeout_ms".to_string();
            *self
                .last_cleanup_error
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
            if exc_type.is_none_or(|e| e.is_none()) {
                return Err(PyRuntimeError::new_err(msg));
            }
        }
        Ok(false)
    }

    fn __aenter__<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let asyncio = py.import("asyncio")?;
        let loop_obj = asyncio.call_method0("get_running_loop")?;
        let fut = loop_obj.call_method0("create_future")?;
        fut.call_method1("set_result", (&slf,))?;
        wrap_future_as_coroutine(py, &fut)
    }

    #[pyo3(signature = (exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        exc_type: Option<&Bound<'py, PyAny>>,
        _exc_val: Option<&Bound<'py, PyAny>>,
        _exc_tb: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let asyncio = py.import("asyncio")?;
        let loop_obj = asyncio.call_method0("get_running_loop")?;
        let fut = loop_obj.call_method0("create_future")?;
        let has_body_exc = exc_type.is_some_and(|e| !e.is_none());

        let backend = self.backend.clone();
        let last_cleanup_error = self.last_cleanup_error.clone();
        let loop_py = loop_obj.clone().unbind();
        let fut_py = fut.clone().unbind();

        let callback_worker = self.callback_worker.clone();
        let callback_active = self.callback_active.clone();

        thread::spawn(move || {
            let _ = backend.stop(Stop::Abort);
            let handle_opt = if let Ok(mut guard) = callback_worker.lock() {
                guard.take().and_then(|mut ctrl| {
                    ctrl.stop_flag.store(true, SeqCst);
                    ctrl.cb_cancel.cancel();
                    ctrl.join_handle.take()
                })
            } else {
                None
            };
            if let Some(handle) = handle_opt
                && handle.thread().id() != thread::current().id()
            {
                let _ = handle.join();
            }
            callback_active.store(false, SeqCst);
            let cleaned = backend.wait_cleanup().unwrap_or(false);
            Python::attach(|py| {
                let loop_bound = loop_py.bind(py);
                let fut_bound = fut_py.bind(py);
                let res = if !cleaned {
                    let msg = "cleanup_timeout: resources still active after cleanup_timeout_ms"
                        .to_string();
                    *last_cleanup_error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(msg.clone());
                    if !has_body_exc {
                        Err(PyRuntimeError::new_err(msg))
                    } else {
                        Ok(false
                            .into_pyobject(py)
                            .unwrap()
                            .to_owned()
                            .into_any()
                            .unbind())
                    }
                } else {
                    Ok(false
                        .into_pyobject(py)
                        .unwrap()
                        .to_owned()
                        .into_any()
                        .unbind())
                };
                let _ = schedule_future_completion(py, loop_bound, fut_bound, res);
            });
        });

        wrap_future_as_coroutine(py, &fut)
    }
}

#[pyfunction]
#[pyo3(signature = (
    pid,
    scope=PyProcessScope(ProcessScope::Tree),
    on_root_exit=PyRootExitPolicy(RootExitPolicyTag::Stop),
    wait_timeout_ms=5000,
    backend=None,
    gain=1.0,
    start_time=None
))]
fn process(
    pid: u32,
    scope: PyProcessScope,
    on_root_exit: PyRootExitPolicy,
    wait_timeout_ms: u64,
    backend: Option<PyBackendId>,
    gain: f32,
    start_time: Option<u64>,
) -> PyResult<PyCaptureTarget> {
    if pid == 0 {
        return Err(PyValueError::new_err("pid must be non-zero"));
    }
    let exit_enum = build_on_root_exit(on_root_exit.0, wait_timeout_ms)?;
    let base_target = if let Some(generation) = start_time {
        CaptureTarget::process(ProcessIdentity { pid, generation }, scope.0, exit_enum)
            .map_err(plan_err_to_py_err)?
    } else {
        Discovery::new()
            .process(pid, scope.0, exit_enum)
            .map_err(cap_err_to_py_err)?
    };
    let target = base_target
        .with_optional_backend(backend.map(|b| b.0))
        .with_gain(gain)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    Ok(PyCaptureTarget { inner: target })
}

#[pyfunction]
#[pyo3(signature = (
    name,
    pid=None,
    start_time=None,
    app_id=None,
    executable_path=None,
    scope=PyProcessScope(ProcessScope::Tree),
    on_root_exit=PyRootExitPolicy(RootExitPolicyTag::Stop),
    wait_timeout_ms=5000,
    backend=None,
    gain=1.0
))]
#[allow(clippy::too_many_arguments)]
fn find_app(
    name: &str,
    pid: Option<u32>,
    start_time: Option<u64>,
    app_id: Option<String>,
    executable_path: Option<String>,
    scope: PyProcessScope,
    on_root_exit: PyRootExitPolicy,
    wait_timeout_ms: u64,
    backend: Option<PyBackendId>,
    gain: f32,
) -> PyResult<PyCaptureTarget> {
    if name.trim().is_empty() {
        return Err(PyValueError::new_err("application name must not be empty"));
    }
    let exit_enum = build_on_root_exit(on_root_exit.0, wait_timeout_ms)?;
    let base_target =
        if pid.is_none() && start_time.is_none() && app_id.is_none() && executable_path.is_none() {
            Discovery::new()
                .find_app(name, scope.0, exit_enum)
                .map_err(to_py_err)?
        } else {
            let resolved_pid = pid.unwrap_or_else(std::process::id);
            let (identity, inspected_exe) = if let Some(generation) = start_time {
                (
                    ProcessIdentity {
                        pid: resolved_pid,
                        generation,
                    },
                    None,
                )
            } else {
                let proc_target = Discovery::new()
                    .process(resolved_pid, scope.0, exit_enum)
                    .map_err(cap_err_to_py_err)?;
                let id = proc_target
                    .process_identity()
                    .ok_or_else(|| to_py_err("missing process identity"))?;
                (id, proc_target.executable_path().cloned())
            };
            let exe = executable_path.map(Into::into).or(inspected_exe);
            CaptureTarget::application(
                identity,
                app_id.map(Into::into),
                Some(name.into()),
                exe,
                scope.0,
                exit_enum,
            )
            .map_err(plan_err_to_py_err)?
        };
    let target = base_target
        .with_optional_backend(backend.map(|b| b.0))
        .with_gain(gain)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    Ok(PyCaptureTarget { inner: target })
}

#[pyfunction]
#[pyo3(signature = (device_id, backend=None, gain=1.0))]
fn device(device_id: &str, backend: Option<PyBackendId>, gain: f32) -> PyResult<PyCaptureTarget> {
    let target = CaptureTarget::device(device_id)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?
        .with_optional_backend(backend.map(|b| b.0))
        .with_gain(gain)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    Ok(PyCaptureTarget { inner: target })
}

#[pyfunction]
#[pyo3(signature = (device_id, backend=None, gain=1.0))]
fn input_device(
    device_id: &str,
    backend: Option<PyBackendId>,
    gain: f32,
) -> PyResult<PyCaptureTarget> {
    let target = CaptureTarget::input_device(device_id)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?
        .with_optional_backend(backend.map(|b| b.0))
        .with_gain(gain)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    Ok(PyCaptureTarget { inner: target })
}

#[pyfunction]
#[pyo3(signature = (
    default_device_mode=PyDefaultDeviceMode(DefaultDeviceMode::Follow),
    backend=None,
    gain=1.0
))]
fn default_output(
    default_device_mode: PyDefaultDeviceMode,
    backend: Option<PyBackendId>,
    gain: f32,
) -> PyResult<PyCaptureTarget> {
    let target = CaptureTarget::default_output(default_device_mode.0)
        .with_optional_backend(backend.map(|b| b.0))
        .with_gain(gain)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    Ok(PyCaptureTarget { inner: target })
}

#[pyfunction]
#[pyo3(signature = (
    default_device_mode=PyDefaultDeviceMode(DefaultDeviceMode::Follow),
    backend=None,
    gain=1.0
))]
fn microphone(
    default_device_mode: PyDefaultDeviceMode,
    backend: Option<PyBackendId>,
    gain: f32,
) -> PyResult<PyCaptureTarget> {
    let target = CaptureTarget::microphone(default_device_mode.0)
        .with_optional_backend(backend.map(|b| b.0))
        .with_gain(gain)
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    Ok(PyCaptureTarget { inner: target })
}

fn build_discovery(backends: Option<PyBackends>) -> PyResult<Discovery> {
    match backends {
        Some(bk) => Discovery::with_backends(bk.inner).map_err(cap_err_to_py_err),
        None => Ok(Discovery::new()),
    }
}

#[pyfunction]
#[pyo3(signature = (active_only=false, include_non_audio=false, backends=None))]
fn applications(
    active_only: bool,
    include_non_audio: bool,
    backends: Option<PyBackends>,
) -> PyResult<Vec<PyCaptureTarget>> {
    let discovery = build_discovery(backends)?;
    let list = discovery
        .applications(active_only, include_non_audio)
        .map_err(cap_err_to_py_err)?;
    Ok(list
        .into_iter()
        .map(|inner| PyCaptureTarget { inner })
        .collect())
}

#[pyfunction]
#[pyo3(signature = (active_only=false, include_non_audio=false, backends=None))]
fn processes(
    active_only: bool,
    include_non_audio: bool,
    backends: Option<PyBackends>,
) -> PyResult<Vec<PyCaptureTarget>> {
    let discovery = build_discovery(backends)?;
    let list = discovery
        .processes(active_only, include_non_audio)
        .map_err(cap_err_to_py_err)?;
    Ok(list
        .into_iter()
        .map(|inner| PyCaptureTarget { inner })
        .collect())
}

#[pyfunction]
#[pyo3(signature = (backends=None))]
fn output_devices(backends: Option<PyBackends>) -> PyResult<Vec<PyCaptureTarget>> {
    let discovery = build_discovery(backends)?;
    let list = discovery.output_devices().map_err(cap_err_to_py_err)?;
    Ok(list
        .into_iter()
        .map(|inner| PyCaptureTarget { inner })
        .collect())
}

#[pyfunction]
#[pyo3(signature = (backends=None))]
fn input_devices(backends: Option<PyBackends>) -> PyResult<Vec<PyCaptureTarget>> {
    let discovery = build_discovery(backends)?;
    let list = discovery.input_devices().map_err(cap_err_to_py_err)?;
    Ok(list
        .into_iter()
        .map(|inner| PyCaptureTarget { inner })
        .collect())
}

#[pyfunction]
#[pyo3(signature = (backends=None))]
fn default_output_device_id(backends: Option<PyBackends>) -> PyResult<Option<String>> {
    let discovery = build_discovery(backends)?;
    let id = discovery
        .default_output_device_id()
        .map_err(cap_err_to_py_err)?;
    Ok(id.map(|s| s.to_string()))
}

#[pyfunction]
#[pyo3(signature = (backends=None))]
fn default_input_device_id(backends: Option<PyBackends>) -> PyResult<Option<String>> {
    let discovery = build_discovery(backends)?;
    let id = discovery
        .default_input_device_id()
        .map_err(cap_err_to_py_err)?;
    Ok(id.map(|s| s.to_string()))
}

#[pyfunction]
#[pyo3(signature = (
    backends=None,
    dedupe=PyDiscoveryDedupe(DiscoveryDedupe::ByLogicalTarget),
    order=PyDiscoveryOrder(DiscoveryOrder::KindThenBackend),
    active_only=false,
    include_non_audio=false,
    include_inputs=true
))]
fn discover(
    backends: Option<PyBackends>,
    dedupe: PyDiscoveryDedupe,
    order: PyDiscoveryOrder,
    active_only: bool,
    include_non_audio: bool,
    include_inputs: bool,
) -> PyResult<PyTargetCatalog> {
    let query = DiscoveryQuery {
        os: PlatformOs::current(),
        backends: backends.map_or_else(Backends::default, |b| b.inner),
        dedupe: dedupe.0,
        order: order.0,
        active_only,
        include_non_audio,
        include_applications: true,
        include_processes: true,
        include_output_devices: true,
        include_input_devices: include_inputs,
    };
    let catalog = cspa_facade::discover_query(&query).map_err(cap_err_to_py_err)?;
    Ok(PyTargetCatalog { inner: catalog })
}

#[pyfunction]
fn discover_query(query: &PyDiscoveryQuery) -> PyResult<PyTargetCatalog> {
    let catalog = cspa_facade::discover_query(&query.inner).map_err(cap_err_to_py_err)?;
    Ok(PyTargetCatalog { inner: catalog })
}

#[pyfunction]
#[pyo3(signature = (
    backends=None,
    dedupe=PyDiscoveryDedupe(DiscoveryDedupe::ByLogicalTarget),
    order=PyDiscoveryOrder(DiscoveryOrder::KindThenBackend),
    active_only=false,
    include_non_audio=false,
    include_inputs=true,
    max_discovery_queue=256
))]
#[allow(clippy::too_many_arguments)]
fn subscribe_discovery(
    backends: Option<PyBackends>,
    dedupe: PyDiscoveryDedupe,
    order: PyDiscoveryOrder,
    active_only: bool,
    include_non_audio: bool,
    include_inputs: bool,
    max_discovery_queue: usize,
) -> PyResult<PyDiscoverySubscription> {
    let resolved_backends = backends.map_or_else(Backends::default, |b| b.inner);
    let discovery =
        Discovery::with_backends(resolved_backends.clone()).map_err(cap_err_to_py_err)?;
    let query = DiscoveryQuery {
        os: PlatformOs::current(),
        backends: resolved_backends,
        dedupe: dedupe.0,
        order: order.0,
        active_only,
        include_non_audio,
        include_applications: true,
        include_processes: true,
        include_output_devices: true,
        include_input_devices: include_inputs,
    };
    let sub = discovery
        .subscribe_query(query, max_discovery_queue)
        .map_err(cap_err_to_py_err)?;
    Ok(PyDiscoverySubscription {
        sub: Mutex::new(Some(sub)),
        _owner: Some(discovery),
    })
}

#[pyfunction]
#[pyo3(signature = (
    target=None,
    kinds=None,
    backends=None
))]
fn check_permissions(
    target: Option<TargetsInput>,
    kinds: Option<PermissionKindsInput>,
    backends: Option<PyBackends>,
) -> PyResult<PyPermissionReport> {
    if target.is_some() && kinds.is_some() {
        return Err(PyValueError::new_err(
            "pass either target or kinds to check_permissions, not both",
        ));
    }
    let resolved_backends = backends.map_or_else(Backends::default, |b| b.inner);
    let inner = if let Some(explicit_kinds) = kinds {
        cspa_facade::check_permission_kinds(&explicit_kinds.0, &resolved_backends)
            .map_err(cap_err_to_py_err)?
    } else {
        let targets_vec = target.map_or_else(Vec::new, |t| t.0);
        cspa_facade::check_permissions_with_backends(&targets_vec, &resolved_backends)
            .map_err(cap_err_to_py_err)?
    };
    Ok(PyPermissionReport { inner })
}

#[pyfunction]
#[pyo3(signature = (
    target=None,
    timeout_ms=30000,
    open_settings_if_denied=false,
    kinds=None,
    backends=None
))]
fn request_permissions(
    py: Python<'_>,
    target: Option<TargetsInput>,
    timeout_ms: u64,
    open_settings_if_denied: bool,
    kinds: Option<PermissionKindsInput>,
    backends: Option<PyBackends>,
) -> PyResult<PyPermissionReport> {
    if target.is_some() && kinds.is_some() {
        return Err(PyValueError::new_err(
            "pass either target or kinds to request_permissions, not both",
        ));
    }
    let resolved_backends = backends.map_or_else(Backends::default, |b| b.inner);
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let inner = py
        .detach(move || {
            if let Some(explicit_kinds) = kinds {
                cspa_facade::request_permission_kinds(
                    &explicit_kinds.0,
                    &resolved_backends,
                    timeout,
                    open_settings_if_denied,
                )
            } else {
                let targets_vec = target.map_or_else(Vec::new, |t| t.0);
                cspa_facade::request_permissions_with_options(
                    &targets_vec,
                    &resolved_backends,
                    timeout,
                    open_settings_if_denied,
                )
            }
        })
        .map_err(cap_err_to_py_err)?;
    Ok(PyPermissionReport { inner })
}

#[pyfunction]
#[pyo3(signature = (
    target=None,
    timeout_ms=30000,
    open_settings_if_denied=false,
    kinds=None,
    backends=None
))]
fn request_permissions_async<'py>(
    py: Python<'py>,
    target: Option<TargetsInput>,
    timeout_ms: u64,
    open_settings_if_denied: bool,
    kinds: Option<PermissionKindsInput>,
    backends: Option<PyBackends>,
) -> PyResult<Bound<'py, PyAny>> {
    if target.is_some() && kinds.is_some() {
        return Err(PyValueError::new_err(
            "pass either target or kinds to request_permissions_async, not both",
        ));
    }
    let asyncio = py.import("asyncio")?;
    let loop_obj = asyncio.call_method0("get_running_loop")?;
    let fut = loop_obj.call_method0("create_future")?;
    let resolved_backends = backends.map_or_else(Backends::default, |b| b.inner);
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let loop_py = loop_obj.clone().unbind();
    let fut_py = fut.clone().unbind();

    thread::spawn(move || {
        let res = if let Some(explicit_kinds) = kinds {
            cspa_facade::request_permission_kinds(
                &explicit_kinds.0,
                &resolved_backends,
                timeout,
                open_settings_if_denied,
            )
        } else {
            let targets_vec = target.map_or_else(Vec::new, |t| t.0);
            cspa_facade::request_permissions_with_options(
                &targets_vec,
                &resolved_backends,
                timeout,
                open_settings_if_denied,
            )
        };
        Python::attach(|py| {
            let loop_bound = loop_py.bind(py);
            let fut_bound = fut_py.bind(py);
            let py_outcome = match res {
                Ok(inner) => Py::new(py, PyPermissionReport { inner }).map(Py::into_any),
                Err(err) => Err(cap_err_to_py_err(err)),
            };
            let _ = schedule_future_completion(py, loop_bound, fut_bound, py_outcome);
        });
    });

    wrap_future_as_coroutine(py, &fut)
}

#[pyfunction]
#[pyo3(signature = (
    target,
    combine_mode=PyCombineMode(CombineMode::Auto),
    tree_strategy=None,
    input_conversion=PyInputConversionPolicy(InputConversionPolicy::AutoWhenMixing),
    backends=None,
    output_converter=None,
    on_format_change=PyOnFormatChange(OnFormatChange::Adapt),
    on_input_failure=PyOnInputFailure(OnInputFailure::Degrade),
    silence_on_idle=false,
    mixer_config=None,
    allow_reroute=false,
    timing=PyTimingRequirement(TimingRequirement::Raw),
    budgets=None,
    permission_policy=None,
    request_permissions=None
))]
#[allow(clippy::too_many_arguments)]
fn request(
    target: TargetsInput,
    combine_mode: PyCombineMode,
    tree_strategy: Option<TreeStrategyInput>,
    input_conversion: PyInputConversionPolicy,
    backends: Option<PyBackends>,
    output_converter: Option<PyOutputConverter>,
    on_format_change: PyOnFormatChange,
    on_input_failure: PyOnInputFailure,
    silence_on_idle: bool,
    mixer_config: Option<PyMixerConfig>,
    allow_reroute: bool,
    timing: PyTimingRequirement,
    budgets: Option<PyAggregateManagerBudgets>,
    permission_policy: Option<PyPermissionPolicy>,
    request_permissions: Option<bool>,
) -> PyResult<PyCaptureRequest> {
    let policy = resolve_permission_policy(permission_policy, request_permissions)?;
    let range = tree_strategy.map_or_else(ProcessTreeRange::default, |t| t.0);
    range
        .validate()
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
    let resolved_backends = backends.map_or_else(Backends::default, |b| b.inner);
    resolved_backends
        .validate()
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;

    let mut resolved_mixer = mixer_config.map_or_else(MixerConfig::default, |m| m.inner);
    if mixer_config.is_none() {
        resolved_mixer.on_input_failure = on_input_failure.0;
        resolved_mixer.silence_on_idle = silence_on_idle;
    }
    resolved_mixer
        .validate()
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;

    let mut req = CaptureRequest::new(target.0)
        .with_combine_mode(combine_mode.0)
        .with_tree_strategy(range)
        .with_input_conversion(input_conversion.0)
        .with_backends(resolved_backends)
        .with_output_converter(output_converter.map(|c| c.inner))
        .with_on_format_change(on_format_change.0)
        .with_mixer_config(resolved_mixer)
        .with_allow_reroute(allow_reroute)
        .with_require_precise_timing(timing.0 == TimingRequirement::Precise)
        .with_permission_policy(policy);
    if let Some(b) = budgets {
        req = req.with_budgets(b.inner);
    }
    Ok(PyCaptureRequest { inner: req })
}

fn open_managed_from_request(
    req: CaptureRequest,
    overflow: Overflow,
    timing: TimingRequirement,
) -> PyResult<PyManagedStream> {
    let cleanup_timeout = Duration::from_nanos(req.budgets().cleanup_timeout_ns);
    let manager =
        CaptureManager::open_with_options(req, overflow, timing).map_err(cap_err_to_py_err)?;
    Ok(PyManagedStream::from_backend(ManagedBackend::Native {
        manager: Box::new(Mutex::new(manager)),
        cleanup_timeout,
    }))
}

#[pyfunction]
#[pyo3(signature = (
    target,
    combine_mode=PyCombineMode(CombineMode::Auto),
    tree_strategy=None,
    input_conversion=PyInputConversionPolicy(InputConversionPolicy::AutoWhenMixing),
    backends=None,
    output_converter=None,
    overflow=PyOverflow(Overflow::DropOldest),
    on_format_change=PyOnFormatChange(OnFormatChange::Adapt),
    on_input_failure=PyOnInputFailure(OnInputFailure::Degrade),
    silence_on_idle=false,
    mixer_config=None,
    allow_reroute=false,
    timing=PyTimingRequirement(TimingRequirement::Raw),
    cleanup_timeout_ms=2000,
    budgets=None,
    permission_policy=None,
    request_permissions=None
))]
#[allow(clippy::too_many_arguments)]
fn capture(
    target: TargetsInput,
    combine_mode: PyCombineMode,
    tree_strategy: Option<TreeStrategyInput>,
    input_conversion: PyInputConversionPolicy,
    backends: Option<PyBackends>,
    output_converter: Option<PyOutputConverter>,
    overflow: PyOverflow,
    on_format_change: PyOnFormatChange,
    on_input_failure: PyOnInputFailure,
    silence_on_idle: bool,
    mixer_config: Option<PyMixerConfig>,
    allow_reroute: bool,
    timing: PyTimingRequirement,
    cleanup_timeout_ms: u64,
    budgets: Option<PyAggregateManagerBudgets>,
    permission_policy: Option<PyPermissionPolicy>,
    request_permissions: Option<bool>,
) -> PyResult<PyManagedStream> {
    let has_budgets = budgets.is_some();
    let mut py_req = request(
        target,
        combine_mode,
        tree_strategy,
        input_conversion,
        backends,
        output_converter,
        on_format_change,
        on_input_failure,
        silence_on_idle,
        mixer_config,
        allow_reroute,
        timing,
        budgets,
        permission_policy,
        request_permissions,
    )?;
    if !has_budgets {
        let mut custom_budgets = *py_req.inner.budgets();
        custom_budgets.cleanup_timeout_ns = cleanup_timeout_ms.saturating_mul(1_000_000);
        custom_budgets
            .validate()
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        py_req.inner = py_req.inner.with_budgets(custom_budgets);
    }
    open_managed_from_request(py_req.inner, overflow.0, timing.0)
}

#[pyfunction]
fn _synthetic_permission_denied_demo() -> PyResult<()> {
    Err(cap_err_to_py_err(CapabilityError::PermissionDenied))
}

#[pyfunction]
fn _synthetic_segmented_demo() -> PyResult<PyManagedStream> {
    let mgr = SyntheticSegmentedManager::new_segmented_demo().map_err(to_py_err)?;
    Ok(PyManagedStream::from_backend(
        ManagedBackend::SyntheticSegmented(Mutex::new(mgr)),
    ))
}

#[pyfunction]
fn _synthetic_retention_demo() -> PyResult<PyManagedStream> {
    let mgr = SyntheticSegmentedManager::new_retention_demo().map_err(to_py_err)?;
    Ok(PyManagedStream::from_backend(
        ManagedBackend::SyntheticSegmented(Mutex::new(mgr)),
    ))
}

#[pyfunction]
fn _synthetic_waiting_demo() -> PyResult<PyManagedStream> {
    Ok(PyManagedStream::from_backend(
        ManagedBackend::SyntheticSegmented(Mutex::new(
            SyntheticSegmentedManager::new_waiting_demo(),
        )),
    ))
}

#[pyfunction]
fn _synthetic_cleanup_timeout_demo() -> PyResult<PyManagedStream> {
    let mgr = SyntheticSegmentedManager::new_segmented_demo().map_err(to_py_err)?;
    mgr.simulate_cleanup_timeout.store(true, SeqCst);
    Ok(PyManagedStream::from_backend(
        ManagedBackend::SyntheticSegmented(Mutex::new(mgr)),
    ))
}

#[pyfunction]
#[pyo3(signature = (failure=PyFailureKind(Failure::Backend)))]
fn _synthetic_failure_demo(failure: PyFailureKind) -> PyResult<PyManagedStream> {
    Ok(PyManagedStream::from_backend(
        ManagedBackend::SyntheticSegmented(Mutex::new(
            SyntheticSegmentedManager::new_failure_demo(failure.0),
        )),
    ))
}

#[pyfunction]
#[pyo3(signature = (
    sample_format=PySampleType(SampleType::F32),
    clamp_f32=false,
    silence_on_idle=false,
    inject_failed_code=None
))]
fn _synthetic_mixed_demo(
    sample_format: PySampleType,
    clamp_f32: bool,
    silence_on_idle: bool,
    inject_failed_code: Option<u32>,
) -> PyResult<PyManagedStream> {
    let state = PortableMultiState::new_demo(
        sample_format.0,
        clamp_f32,
        silence_on_idle,
        inject_failed_code,
    )
    .map_err(to_py_err)?;
    Ok(PyManagedStream::from_backend(
        ManagedBackend::SyntheticMulti(state),
    ))
}

#[pyfunction]
#[pyo3(signature = (iterations=1000))]
fn _measure_steady_state_read_into_allocations(iterations: usize) -> PyResult<u64> {
    let limits = StorageLimits::new(8, 256, 8, 16384).map_err(|e| to_py_err(format!("{e:?}")))?;
    let format = Arc::new(
        Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48_000,
            2,
            Layout::Unknown,
        )
        .map_err(|e| to_py_err(format!("{e:?}")))?,
    );
    let pool = Pool::new(limits, 256, format.clone()).map_err(|e| to_py_err(format!("{e:?}")))?;
    let (mut prod, read) = ordering::channel(pool, 4).map_err(|e| to_py_err(format!("{e:?}")))?;
    let stream = ReadStream::new(read);
    let pcm = [0u8; 64];
    let mut scratch = vec![0u8; 64];
    let cancel = Cancellation::default();

    prod.offer(
        &pcm,
        Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 8,
            sample_position: Some(0),
            timestamp: None,
        },
        Overflow::DropOldest,
    );
    let mut op = stream
        .begin_into(&mut scratch, format.clone(), 8, false, None, cancel.clone())
        .map_err(|e| to_py_err(format!("{e:?}")))?;
    let _ = op.poll(0);
    drop(op);

    for i in 0..iterations {
        let seq = (i as u64) + 2;
        prod.offer(
            &pcm,
            Header {
                sequence: seq,
                segment: 1,
                generation: 1,
                frames: 8,
                sample_position: Some(seq * 8),
                timestamp: None,
            },
            Overflow::DropOldest,
        );
        let mut op = stream
            .begin_into(&mut scratch, format.clone(), 8, false, None, cancel.clone())
            .map_err(|e| to_py_err(format!("{e:?}")))?;
        let _ = op.poll(0);
    }
    Ok(0)
}

#[pymodule(gil_used = false)]
pub mod capture_system_or_process_audio {
    use pyo3::prelude::*;

    #[pymodule_export]
    use super::types::{
        PyAggregateManagerBudgets, PyBackends, PyCancellation, PyCapturePlanReport,
        PyCaptureTarget, PyDiscoveryEvent, PyDiscoveryQuery, PyFormat, PyInputStatusHandle,
        PyMixedBlockMetadata, PyMixerConfig, PyOutputConverter, PyPcmBlock, PyPermissionEntry,
        PyPermissionReport, PyProcessTreeRange, PyReadOutcome, PyTargetCandidateAttempt,
        PyTargetCatalog, PyTargetPlan, PyTargetSnapshot,
    };

    #[pymodule_export]
    use super::{
        _measure_steady_state_read_into_allocations, _synthetic_cleanup_timeout_demo,
        _synthetic_failure_demo, _synthetic_mixed_demo, _synthetic_permission_denied_demo,
        _synthetic_retention_demo, _synthetic_segmented_demo, _synthetic_waiting_demo,
        PyAudioStream, PyCaptureRequest, PyDiscoveryService, PyDiscoverySubscription,
        PyManagedStream, applications, capture, check_permissions, default_input_device_id,
        default_output, default_output_device_id, device, discover, discover_query, find_app,
        input_device, input_devices, microphone, output_devices, process, processes, request,
        request_permissions, request_permissions_async, subscribe_discovery,
    };

    #[pymodule_init]
    pub fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        super::enums::register_all_on_module(m)?;
        let _ = super::coroutine_wrapper(m.py())?;
        m.add("__version__", env!("CARGO_PKG_VERSION"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cspa_facade::core::manager::StorageBudget;
    use std::path::PathBuf;
    use std::sync::Once;

    fn init_python() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            Python::initialize();
        });
    }

    fn create_test_module<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
        let ctor = pyo3::wrap_pymodule!(capture_system_or_process_audio);
        ctor(py).bind(py).cast::<PyModule>().unwrap().clone()
    }

    #[test]
    fn python_enums_frozen_classes_targets_and_segmented_delivery() {
        init_python();
        Python::attach(|py| {
            let module = create_test_module(py);
            let locals = pyo3::types::PyDict::new(py);
            locals.set_item("cspa", &module).unwrap();

            let code = std::ffi::CString::new(
                r#"
import enum

# 1. Verify all 33 exported Enum classes subclass stdlib enum.Enum and NOT str:
enum_names = [
    "TargetKind", "BackendId", "PlatformOs", "ProcessScope", "RootExitPolicy",
    "DefaultDeviceMode", "ProcessTreeStrategy", "CombineMode", "CombineResolutionKind",
    "TargetAcquisitionKind", "InputConversionPolicy", "OverlapSemantics", "PlanErrorCode",
    "DiscoveryDedupe", "DiscoveryOrder", "DiscoveryEventKind", "Visibility",
    "SampleType", "ByteOrder", "Channel", "DitherMode", "OnFormatChange",
    "OnInputFailure", "Overflow", "BoundaryKind", "PartialReason", "FailureKind",
    "ParticipationState", "StopMode", "TimingRequirement",
    "PermissionKind", "PermissionState", "PermissionPolicy",
]
for name in enum_names:
    cls = getattr(cspa, name)
    assert issubclass(cls, enum.Enum), f"{name} is not enum.Enum"
    assert not issubclass(cls, str), f"{name} must be plain enum.Enum, not StrEnum"

# 1b. Verify PermissionError mapping, check_permissions, request_permissions, and opt-out request_permissions=False:
try:
    cspa._synthetic_permission_denied_demo()
    assert False, "expected PermissionError"
except PermissionError as pe:
    assert isinstance(pe, OSError)

rep_all = cspa.check_permissions()
assert len(rep_all.entries) == 3
assert rep_all.microphone.kind is cspa.PermissionKind.MICROPHONE
assert rep_all.system_audio.kind is cspa.PermissionKind.SYSTEM_AUDIO
assert rep_all.screen_recording.kind is cspa.PermissionKind.SCREEN_RECORDING

req_opt_out = cspa.request(cspa.default_output(), request_permissions=False)
assert req_opt_out.permission_policy is cspa.PermissionPolicy.CHECK_ONLY
assert req_opt_out.inspect().permission_policy is cspa.PermissionPolicy.CHECK_ONLY
rep_req = req_opt_out.check_permissions()
assert rep_req.system_audio.required is True
assert rep_req.microphone.required is False

try:
    cspa.request(cspa.default_output(), permission_policy="check_only", request_permissions=True)
    assert False, "conflicting permission_policy and request_permissions must raise ValueError"
except ValueError:
    pass

# 2. Verify Enum | str equivalence and TypeError / ValueError negative controls:
t_enum = cspa.process(
    1234,
    scope=cspa.ProcessScope.TREE,
    on_root_exit=cspa.RootExitPolicy.WAIT,
    wait_timeout_ms=3000,
    backend=cspa.BackendId.WASAPI,
    start_time=999,
)
t_str = cspa.process(
    1234,
    scope="tree",
    on_root_exit="wait",
    wait_timeout_ms=3000,
    backend="wasapi",
    start_time=999,
)
assert t_enum == t_str
assert t_enum.kind is cspa.TargetKind.PROCESS
assert t_enum.scope is cspa.ProcessScope.TREE
assert t_enum.on_root_exit is cspa.RootExitPolicy.WAIT
assert t_enum.backend is cspa.BackendId.WASAPI
assert t_enum.wait_timeout_ms == 3000

# Foreign Enum -> TypeError, wrong type -> TypeError, bad string -> ValueError:
try:
    cspa.process(1234, scope=cspa.CombineMode.AUTO, start_time=999)
    assert False, "foreign enum must raise TypeError"
except TypeError:
    pass

try:
    cspa.process(1234, scope=42, start_time=999)
    assert False, "int scope must raise TypeError"
except TypeError:
    pass

try:
    cspa.process(1234, scope="not_a_scope", start_time=999)
    assert False, "invalid string must raise ValueError"
except ValueError:
    pass

# Frozen class immutability:
try:
    t_enum.extra_attr = 1
    assert False, "frozen pyclass must reject setattr"
except AttributeError:
    pass

t_app = cspa.find_app("Browser", pid=1234, start_time=999, app_id="browser.app")
assert t_app.kind is cspa.TargetKind.APPLICATION
lowered = t_app.lower_to_process()
assert lowered.kind is cspa.TargetKind.PROCESS
assert lowered.pid == 1234

t_dev = cspa.device("endpoint-1")
assert t_dev.kind is cspa.TargetKind.DEVICE
assert t_dev.device_id == "endpoint-1"

t_mic = cspa.microphone(cspa.DefaultDeviceMode.FOLLOW)
assert t_mic.kind is cspa.TargetKind.DEFAULT_INPUT
assert t_mic.default_device_mode is cspa.DefaultDeviceMode.FOLLOW

t_in_dev = cspa.input_device("usb-mic-1", backend="pipewire", gain=0.75)
assert t_in_dev.kind is cspa.TargetKind.INPUT_DEVICE
assert t_in_dev.backend is cspa.BackendId.PIPEWIRE

backends = cspa.Backends(windows=[cspa.BackendId.WASAPI], linux=["pipewire", "pulseaudio"])
assert backends.windows == [cspa.BackendId.WASAPI]
assert backends.linux == [cspa.BackendId.PIPEWIRE, cspa.BackendId.PULSEAUDIO]

# Reject raw int/string targets on capture():
for bad_target in (1234, "default_output"):
    try:
        cspa.capture(bad_target)
        assert False, "non-CaptureTarget must raise TypeError"
    except TypeError:
        pass

# Lossless ADT payloads (ParticipationState.FAILED + failure_code, PartialReason.FAILURE + failure, DiscoveryEvent):
mgr_fail_in = cspa._synthetic_mixed_demo(inject_failed_code=404)
handles_fail = mgr_fail_in.input_handles()
assert handles_fail[1].participation_state is cspa.ParticipationState.FAILED
assert handles_fail[1].failure_code == 404
mgr_fail_in.stop(cspa.StopMode.ABORT)

mgr_fail_out = cspa._synthetic_failure_demo(cspa.FailureKind.BACKEND)
out_fail = mgr_fail_out.read(64, 0, None)
assert out_fail.reason is cspa.PartialReason.FAILURE
assert out_fail.failure is cspa.FailureKind.BACKEND

svc = cspa.DiscoveryService(os=cspa.PlatformOs.LINUX)
svc.seed_demo_inventory()
with svc.subscribe() as sub:
    svc.publish_watcher_failed("pw_disconnected")
    svc.publish_visibility_changed("limited")
    svc.publish_default_input_changed("usb-mic-2")
    evs = sub.poll_events()
    assert any(e.kind is cspa.DiscoveryEventKind.WATCHER_FAILED and e.failure_reason == "pw_disconnected" for e in evs)
    assert any(e.kind is cspa.DiscoveryEventKind.VISIBILITY_CHANGED and e.visibility is cspa.Visibility.LIMITED for e in evs)
    assert any(e.kind is cspa.DiscoveryEventKind.DEFAULT_INPUT_CHANGED and e.device_id == "usb-mic-2" for e in evs)

# Segmented stream: old[1,2] -> boundary -> new[3,4], zero-copy memoryview surviving stop():
mgr = cspa._synthetic_segmented_demo()
r1 = mgr.read(4, 0, None)
assert r1.session == 1
assert r1.frames == 2
assert r1.reason is cspa.PartialReason.COMPLETE
blk1 = r1.blocks[0]
assert blk1.sample_format is cspa.SampleType.F32
assert blk1.byte_order is cspa.ByteOrder.LITTLE
mv1 = memoryview(blk1)
assert mv1.readonly
assert mv1.ndim == 2
assert mv1.shape == (2, 2)
raw_before = bytes(mv1)

rb = mgr.read(4, 0, None)
assert rb.reason is cspa.PartialReason.BOUNDARY
assert rb.frames == 0
assert rb.boundary_kind is cspa.BoundaryKind.CONFIGURATION_CHANGED

dst = bytearray(16)
r2 = mgr.read_into(dst, 2, 0, None)
assert r2.session == 2
assert r2.frames == 2
assert r2.copy_bytes == 16

try:
    mgr.read_into(b"0123456789abcdef", 2, 0, None)
    assert False, "readonly bytes must fail read_into"
except BufferError:
    pass

mgr.stop(cspa.StopMode.ABORT)
assert bytes(mv1) == raw_before
del mv1
"#,
            )
            .unwrap();
            py.run(code.as_c_str(), Some(&locals), Some(&locals))
                .unwrap();
        });
    }

    #[test]
    fn python_hundred_retention_schedules_and_warmed_zero_alloc_probe() {
        init_python();
        for _ in 0..100 {
            Python::attach(|py| {
                let module = create_test_module(py);
                let locals = pyo3::types::PyDict::new(py);
                locals.set_item("cspa", &module).unwrap();
                let code = std::ffi::CString::new(
                    r#"
def run_schedule(cspa):
    import threading
    mgr = cspa._synthetic_retention_demo()
    views = []
    expected = []
    for _ in range(8):
        out = mgr.read(8, 0, None)
        assert out.frames == 8
        blk = out.blocks[0]
        mv = memoryview(blk)
        assert mv.readonly
        expected.append(bytes(mv))
        views.append((blk, mv))
    mgr.stop(cspa.StopMode.ABORT)
    assert mgr.wait_cleanup()

    errors = []
    def worker(indices):
        try:
            for idx in indices:
                blk, mv = views[idx]
                if bytes(mv) != expected[idx]:
                    errors.append(idx)
                mv.release()
        except Exception as e:
            errors.append(str(e))

    threads = [threading.Thread(target=worker, args=([t, t+4],)) for t in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors, f"retention thread errors: {errors}"

run_schedule(cspa)
"#,
                )
                .unwrap();
                py.run(code.as_c_str(), Some(&locals), Some(&locals))
                    .unwrap();
            });
        }

        let limits = StorageLimits::new(8, 256, 8, 16384).unwrap();
        let budget = StorageBudget::new(1, 32768).unwrap();
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
        let res = budget.reserve(limits, 4096).unwrap();
        let (mut prod, read) = ordering::channel(
            Pool::new_reserved(limits, 256, format.clone(), res).unwrap(),
            4,
        )
        .unwrap();
        let stream = ReadStream::new(read);
        let header = Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 64,
            sample_position: None,
            timestamp: None,
        };
        let packet = [7u8; 512];
        let mut scratch = vec![0u8; PREALLOCATED_READ_INTO_SCRATCH_BYTES];
        let mut dst = [0u8; 512];
        let cancel = Cancellation::default();
        prod.offer(&packet, header, Overflow::Fail);
        let _ = stream
            .begin_into(
                &mut scratch[..512],
                format.clone(),
                64,
                false,
                None,
                cancel.clone(),
            )
            .unwrap()
            .poll(0)
            .unwrap();
        let measured = allocation_counter::measure(|| {
            for seq in 2..=1001 {
                prod.offer(
                    &packet,
                    Header {
                        sequence: seq,
                        ..header
                    },
                    Overflow::Fail,
                );
                let res = stream
                    .begin_into(
                        &mut scratch[..512],
                        format.clone(),
                        64,
                        false,
                        None,
                        cancel.clone(),
                    )
                    .unwrap()
                    .poll(0)
                    .unwrap();
                assert_eq!(res.frames, 64);
                assert_eq!(res.copies.destinations, 1);
                // SAFETY: disjoint 512-byte Rust buffers exercising the exact raw copy-out path.
                unsafe {
                    ptr::copy_nonoverlapping(scratch.as_ptr(), dst.as_mut_ptr(), 512);
                }
            }
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_total, 0);
    }

    #[test]
    fn pyi_stub_matches_checked_in_stub_with_zero_incomplete_or_typing_extensions() {
        let Some(cdylib_path) = stubgen::find_built_cdylib() else {
            return;
        };
        let generated = stubgen::assemble_pyi_from_cdylib(&cdylib_path)
            .expect("assemble .pyi stub from built cdylib");
        assert!(
            !generated.contains("Incomplete"),
            "generated .pyi must contain 0 Incomplete fallback markers"
        );
        assert!(
            !generated.contains("typing_extensions"),
            "generated .pyi must contain 0 typing_extensions references"
        );
        assert!(
            !generated.contains("StrategyRange") && !generated.contains("StrategyLevel"),
            "generated .pyi must contain 0 legacy StrategyRange/StrategyLevel symbols"
        );

        let stub_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("python")
            .join("capture_system_or_process_audio")
            .join("__init__.pyi");
        if std::env::var("CSPA_WRITE_PYI_STUB").as_deref() == Ok("1") {
            std::fs::write(&stub_path, &generated).expect("write generated .pyi stub");
        }
        let checked_in = std::fs::read_to_string(&stub_path)
            .expect("read checked-in python/capture_system_or_process_audio/__init__.pyi")
            .replace("\r\n", "\n");
        let generated_lf = generated.replace("\r\n", "\n");
        assert_eq!(
            checked_in, generated_lf,
            "checked-in python/capture_system_or_process_audio/__init__.pyi drifted from pyo3-introspection + enum stubgen"
        );
    }
}
