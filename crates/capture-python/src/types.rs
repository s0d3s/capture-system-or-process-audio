use crate::enums::{
    CombineResolutionTag, DiscoveryEventTag, PartialReasonTag, ParticipationStateTag, PlanErrorTag,
    PyBackendId, PyBoundaryKind, PyByteOrder, PyChannel, PyCombineMode, PyCombineResolutionKind,
    PyDefaultDeviceMode, PyDiscoveryDedupe, PyDiscoveryEventKind, PyDiscoveryOrder, PyDitherMode,
    PyFailureKind, PyInputConversionPolicy, PyOnInputFailure, PyOverlapSemantics, PyPartialReason,
    PyParticipationState, PyPermissionKind, PyPermissionPolicy, PyPermissionState, PyPlanErrorCode,
    PyPlatformOs, PyProcessScope, PyProcessTreeStrategy, PyRootExitPolicy, PySampleType,
    PyTargetAcquisitionKind, PyTargetKind, PyVisibility, RootExitPolicyTag, TargetAcquisitionTag,
};
use cspa_facade::{
    AggregateManagerBudgets, BackendId, Backends, BoundaryKind, ByteOrder, Cancellation,
    CapabilityError, CapturePlanReport, CaptureTarget, Channel, DiscoveryDedupe, DiscoveryEvent,
    DiscoveryEventKind, DiscoveryOrder, DiscoveryQuery, DitherMode, Failure, Format,
    InputStatusHandle, Layout, MixedBlockMetadata, MixerConfig, OnInputFailure, OnRootExit,
    OutputConverterConfig, PartialReason, PermissionEntry, PermissionKind, PermissionReport,
    PlanError, PlatformOs, ProcessTreeRange, ProcessTreeStrategy, ReadResult, SampleType, Slice,
    TargetCatalog, TargetPlan, TargetSnapshot,
    core::{planner::TargetCandidateAttempt, schema::DiagnosticOptions, storage::OwnedBlock},
};
use pyo3::{
    Borrowed, Bound, FromPyObject, Py, PyAny, PyErr, PyResult, Python,
    exceptions::{
        PyBufferError, PyImportError, PyPermissionError, PyRuntimeError, PyTypeError, PyValueError,
    },
    ffi,
    prelude::*,
    type_hint_union,
    types::{PyBytes, PyString},
};
use std::{
    ffi::CString,
    os::raw::{c_char, c_int, c_void},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::SeqCst},
    },
};

pub fn to_py_err(message: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(message.to_string())
}

pub fn cap_err_to_py_err(err: CapabilityError) -> PyErr {
    match err {
        CapabilityError::PermissionDenied => PyPermissionError::new_err(err.to_string()),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

pub fn plan_err_to_py_err(err: PlanError) -> PyErr {
    match err {
        PlanError::PermissionDenied => PyPermissionError::new_err(err.to_string()),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

pub fn build_on_root_exit(policy: RootExitPolicyTag, wait_timeout_ms: u64) -> PyResult<OnRootExit> {
    match policy {
        RootExitPolicyTag::Stop => Ok(OnRootExit::Stop),
        RootExitPolicyTag::Wait => {
            if wait_timeout_ms == 0 {
                return Err(PyValueError::new_err("wait_timeout_ms must be > 0"));
            }
            Ok(OnRootExit::Wait {
                wait_timeout_ns: wait_timeout_ms.saturating_mul(1_000_000),
            })
        }
    }
}

pub struct TargetsInput(pub Vec<CaptureTarget>);

impl<'a, 'py> FromPyObject<'a, 'py> for TargetsInput {
    type Error = PyErr;

    const INPUT_TYPE: pyo3::inspect::PyStaticExpr = pyo3::type_hint_union!(
        pyo3::type_hint_identifier!("capture_system_or_process_audio", "CaptureTarget"),
        pyo3::type_hint_subscript!(
            pyo3::type_hint_identifier!("collections.abc", "Sequence"),
            pyo3::type_hint_identifier!("capture_system_or_process_audio", "CaptureTarget")
        )
    );

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        if let Ok(single) = obj.extract::<PyCaptureTarget>() {
            return Ok(Self(vec![single.inner]));
        }
        if obj.is_instance_of::<PyString>() || obj.is_instance_of::<PyBytes>() {
            return Err(PyTypeError::new_err(
                "expected CaptureTarget or Sequence[CaptureTarget], got str/bytes",
            ));
        }
        if let Ok(seq) = obj.extract::<Vec<PyCaptureTarget>>() {
            if seq.is_empty() {
                return Err(PyValueError::new_err("target sequence must not be empty"));
            }
            return Ok(Self(seq.into_iter().map(|t| t.inner).collect()));
        }
        Err(PyTypeError::new_err(
            "expected CaptureTarget or Sequence[CaptureTarget]",
        ))
    }
}

pub struct PermissionKindsInput(pub Vec<PermissionKind>);

impl<'a, 'py> FromPyObject<'a, 'py> for PermissionKindsInput {
    type Error = PyErr;

    const INPUT_TYPE: pyo3::inspect::PyStaticExpr = pyo3::type_hint_union!(
        pyo3::type_hint_identifier!("capture_system_or_process_audio", "PermissionKind"),
        pyo3::type_hint_identifier!("builtins", "str"),
        pyo3::type_hint_subscript!(
            pyo3::type_hint_identifier!("collections.abc", "Sequence"),
            pyo3::type_hint_union!(
                pyo3::type_hint_identifier!("capture_system_or_process_audio", "PermissionKind"),
                pyo3::type_hint_identifier!("builtins", "str")
            )
        )
    );

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        if obj.is_instance_of::<PyBytes>() {
            return Err(PyTypeError::new_err(
                "expected PermissionKind, str, or Sequence[PermissionKind | str], got bytes",
            ));
        }
        if let Ok(single) = obj.extract::<PyPermissionKind>() {
            return Ok(Self(vec![single.0]));
        }
        if obj.is_instance_of::<PyString>() {
            return Err(PyValueError::new_err(format!(
                "invalid PermissionKind string {:?}",
                obj.extract::<&str>().unwrap_or("<str>")
            )));
        }
        if let Ok(seq) = obj.extract::<Vec<PyPermissionKind>>() {
            if seq.is_empty() {
                return Err(PyValueError::new_err(
                    "permission kinds sequence must not be empty",
                ));
            }
            return Ok(Self(seq.into_iter().map(|k| k.0).collect()));
        }
        Err(PyTypeError::new_err(
            "expected PermissionKind, str, or Sequence[PermissionKind | str]",
        ))
    }
}

pub struct TreeStrategyInput(pub ProcessTreeRange);

impl<'a, 'py> FromPyObject<'a, 'py> for TreeStrategyInput {
    type Error = PyErr;

    const INPUT_TYPE: pyo3::inspect::PyStaticExpr = pyo3::type_hint_union!(
        pyo3::type_hint_identifier!("capture_system_or_process_audio", "ProcessTreeRange"),
        pyo3::type_hint_identifier!("capture_system_or_process_audio", "ProcessTreeStrategy"),
        pyo3::type_hint_identifier!("builtins", "str")
    );

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        if let Ok(range) = obj.extract::<PyProcessTreeRange>() {
            return Ok(Self(range.inner));
        }
        let strat = PyProcessTreeStrategy::parse_obj(&obj)?;
        Ok(Self(ProcessTreeRange {
            start_level: strat,
            fallback_limit: Some(strat),
        }))
    }
}

pub struct DestinationBuffer<'py>(pub Bound<'py, PyAny>);

impl<'a, 'py> FromPyObject<'a, 'py> for DestinationBuffer<'py> {
    type Error = PyErr;

    const INPUT_TYPE: pyo3::inspect::PyStaticExpr = pyo3::type_hint_union!(
        pyo3::type_hint_identifier!("builtins", "bytearray"),
        pyo3::type_hint_identifier!("builtins", "memoryview"),
        pyo3::type_hint_identifier!("typing", "Any")
    );

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        Ok(Self(obj.to_owned()))
    }
}

#[pyclass(
    name = "CaptureTarget",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, PartialEq)]
pub struct PyCaptureTarget {
    pub inner: CaptureTarget,
}

#[pymethods]
impl PyCaptureTarget {
    #[getter]
    fn kind(&self) -> PyTargetKind {
        PyTargetKind(self.inner.kind())
    }

    #[getter]
    fn id(&self) -> String {
        self.inner.id().to_string()
    }

    #[getter]
    fn logical_id(&self) -> String {
        self.inner.logical_id().to_string()
    }

    #[getter]
    fn pid(&self) -> Option<u32> {
        self.inner.pid()
    }

    #[getter]
    fn process_start_time(&self) -> Option<u64> {
        self.inner.process_start_time()
    }

    #[getter]
    fn app_id(&self) -> Option<String> {
        self.inner.app_id().map(ToString::to_string)
    }

    #[getter]
    fn device_id(&self) -> Option<String> {
        self.inner.device_id().map(ToString::to_string)
    }

    #[getter]
    fn name(&self) -> Option<String> {
        self.inner.name().map(ToString::to_string)
    }

    #[getter]
    fn executable_path(&self) -> Option<String> {
        self.inner.executable_path().map(ToString::to_string)
    }

    #[getter]
    fn active_audio(&self) -> Option<bool> {
        self.inner.active_audio()
    }

    #[getter]
    fn scope(&self) -> PyProcessScope {
        PyProcessScope(self.inner.scope())
    }

    #[getter]
    fn on_root_exit(&self) -> PyRootExitPolicy {
        PyRootExitPolicy(RootExitPolicyTag::from(self.inner.on_root_exit()))
    }

    #[getter]
    fn wait_timeout_ms(&self) -> Option<u64> {
        match self.inner.on_root_exit() {
            OnRootExit::Stop => None,
            OnRootExit::Wait { wait_timeout_ns } => Some(wait_timeout_ns / 1_000_000),
        }
    }

    #[getter]
    fn default_device_mode(&self) -> PyDefaultDeviceMode {
        PyDefaultDeviceMode(self.inner.default_device_mode())
    }

    #[getter]
    fn backend(&self) -> Option<PyBackendId> {
        self.inner.backend().map(PyBackendId)
    }

    #[getter]
    fn available_backends(&self) -> Vec<PyBackendId> {
        self.inner
            .available_backends()
            .iter()
            .copied()
            .map(PyBackendId)
            .collect()
    }

    #[getter]
    fn gain(&self) -> f32 {
        self.inner.gain()
    }

    #[pyo3(signature = (backend=None))]
    fn with_backend(&self, backend: Option<PyBackendId>) -> Self {
        Self {
            inner: self
                .inner
                .clone()
                .with_optional_backend(backend.map(|b| b.0)),
        }
    }

    fn with_gain(&self, gain: f32) -> PyResult<Self> {
        self.inner
            .clone()
            .with_gain(gain)
            .map(|inner| Self { inner })
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))
    }

    fn lower_to_process(&self) -> PyResult<Self> {
        self.inner
            .lower_to_process()
            .map(|inner| Self { inner })
            .map_err(to_py_err)
    }

    #[pyo3(signature = (include_identity=false))]
    fn diagnostic_summary(&self, include_identity: bool) -> String {
        self.inner
            .diagnostic_summary(DiagnosticOptions { include_identity })
    }
}

#[pyclass(
    name = "Backends",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PyBackends {
    pub inner: Backends,
}

#[pymethods]
impl PyBackends {
    #[new]
    #[pyo3(signature = (windows=None, linux=None, macos=None))]
    fn new(
        windows: Option<Vec<PyBackendId>>,
        linux: Option<Vec<PyBackendId>>,
        macos: Option<Vec<PyBackendId>>,
    ) -> PyResult<Self> {
        let map_list =
            |list: Option<Vec<PyBackendId>>| list.map(|v| v.into_iter().map(|b| b.0).collect());
        let inner = Backends::new(map_list(windows), map_list(linux), map_list(macos))
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    #[getter]
    fn windows(&self) -> Vec<PyBackendId> {
        self.inner
            .windows
            .iter()
            .copied()
            .map(PyBackendId)
            .collect()
    }

    #[getter]
    fn linux(&self) -> Vec<PyBackendId> {
        self.inner.linux.iter().copied().map(PyBackendId).collect()
    }

    #[getter]
    fn macos(&self) -> Vec<PyBackendId> {
        self.inner.macos.iter().copied().map(PyBackendId).collect()
    }
}

#[pyclass(
    name = "Format",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PyFormat {
    pub inner: Format,
}

#[pymethods]
impl PyFormat {
    #[new]
    #[pyo3(signature = (
        rate=48000,
        channels=2,
        sample_format=PySampleType(SampleType::F32),
        byte_order=PyByteOrder(ByteOrder::Little),
        layout=None
    ))]
    pub fn new(
        rate: u32,
        channels: u16,
        sample_format: PySampleType,
        byte_order: PyByteOrder,
        layout: Option<Vec<PyChannel>>,
    ) -> PyResult<Self> {
        let resolved_layout = match layout {
            Some(ch_list) => {
                let raw: Vec<Channel> = ch_list.into_iter().map(|c| c.0).collect();
                Layout::Known(Arc::from(raw))
            }
            None => match channels {
                1 => Layout::Known(Arc::from([Channel::Mono])),
                2 => Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
                _ => Layout::Unknown,
            },
        };
        let inner = Format::new(
            sample_format.0,
            byte_order.0,
            rate,
            channels,
            resolved_layout,
        )
        .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    #[getter]
    fn rate(&self) -> u32 {
        self.inner.rate()
    }

    #[getter]
    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    #[getter]
    fn sample_format(&self) -> PySampleType {
        PySampleType(self.inner.sample())
    }

    #[getter]
    fn byte_order(&self) -> PyByteOrder {
        PyByteOrder(self.inner.order())
    }

    #[getter]
    fn layout(&self) -> Option<Vec<PyChannel>> {
        match self.inner.layout() {
            Layout::Unknown => None,
            Layout::Known(labels) => Some(labels.iter().copied().map(PyChannel).collect()),
        }
    }

    #[getter]
    fn bytes_per_frame(&self) -> usize {
        self.inner.bytes_per_frame()
    }
}

#[pyclass(
    name = "OutputConverter",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PyOutputConverter {
    pub inner: OutputConverterConfig,
}

#[pymethods]
impl PyOutputConverter {
    #[new]
    #[pyo3(signature = (
        rate=48000,
        channels=2,
        sample_format=PySampleType(SampleType::F32),
        byte_order=PyByteOrder(ByteOrder::Little),
        dither=PyDitherMode(DitherMode::Tpdf),
        layout=None
    ))]
    pub fn new(
        rate: u32,
        channels: u16,
        sample_format: PySampleType,
        byte_order: PyByteOrder,
        dither: PyDitherMode,
        layout: Option<Vec<PyChannel>>,
    ) -> PyResult<Self> {
        let fmt = PyFormat::new(rate, channels, sample_format, byte_order, layout)?.inner;
        Ok(Self {
            inner: OutputConverterConfig::new(fmt, dither.0),
        })
    }

    #[getter]
    fn rate(&self) -> u32 {
        self.inner.target_format.rate()
    }

    #[getter]
    fn channels(&self) -> u16 {
        self.inner.target_format.channels()
    }

    #[getter]
    fn sample_format(&self) -> PySampleType {
        PySampleType(self.inner.target_format.sample())
    }

    #[getter]
    fn byte_order(&self) -> PyByteOrder {
        PyByteOrder(self.inner.target_format.order())
    }

    #[getter]
    fn dither(&self) -> PyDitherMode {
        PyDitherMode(self.inner.dither)
    }

    #[getter]
    fn format(&self) -> PyFormat {
        PyFormat {
            inner: self.inner.target_format.clone(),
        }
    }
}

#[pyclass(
    name = "ProcessTreeRange",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyProcessTreeRange {
    pub inner: ProcessTreeRange,
}

#[pymethods]
impl PyProcessTreeRange {
    #[new]
    #[pyo3(signature = (
        start_level=PyProcessTreeStrategy(ProcessTreeStrategy::NativeKernelCapture),
        fallback_limit=Some(PyProcessTreeStrategy(ProcessTreeStrategy::SoftwareStreamMix))
    ))]
    pub fn new(
        start_level: PyProcessTreeStrategy,
        fallback_limit: Option<PyProcessTreeStrategy>,
    ) -> PyResult<Self> {
        let inner = ProcessTreeRange {
            start_level: start_level.0,
            fallback_limit: fallback_limit.map(|f| f.0),
        };
        inner
            .validate()
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    #[getter]
    fn start_level(&self) -> PyProcessTreeStrategy {
        PyProcessTreeStrategy(self.inner.start_level)
    }

    #[getter]
    fn fallback_limit(&self) -> Option<PyProcessTreeStrategy> {
        self.inner.fallback_limit.map(PyProcessTreeStrategy)
    }

    fn allows(&self, level: PyProcessTreeStrategy) -> bool {
        self.inner.allows(level.0)
    }
}

#[pyclass(
    name = "MixerConfig",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyMixerConfig {
    pub inner: MixerConfig,
}

#[pymethods]
impl PyMixerConfig {
    #[new]
    #[pyo3(signature = (
        alignment_window_ms=20,
        max_input_buffer_ms=100,
        max_input_slots=8,
        output_queue_slots=8,
        clamp_f32=false,
        drift_threshold_ms=5,
        drift_hysteresis_ms=2,
        drift_hold_ms=500,
        drift_crossfade_frames=64,
        silence_on_idle=false,
        on_input_failure=PyOnInputFailure(OnInputFailure::Degrade)
    ))]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        alignment_window_ms: u64,
        max_input_buffer_ms: u64,
        max_input_slots: usize,
        output_queue_slots: usize,
        clamp_f32: bool,
        drift_threshold_ms: u64,
        drift_hysteresis_ms: u64,
        drift_hold_ms: u64,
        drift_crossfade_frames: usize,
        silence_on_idle: bool,
        on_input_failure: PyOnInputFailure,
    ) -> PyResult<Self> {
        let inner = MixerConfig {
            alignment_window_ns: alignment_window_ms.saturating_mul(1_000_000),
            max_input_buffer_ns: max_input_buffer_ms.saturating_mul(1_000_000),
            max_input_slots,
            output_queue_slots,
            clamp_f32,
            drift_threshold_ns: drift_threshold_ms.saturating_mul(1_000_000),
            drift_hysteresis_ns: drift_hysteresis_ms.saturating_mul(1_000_000),
            drift_hold_ns: drift_hold_ms.saturating_mul(1_000_000),
            drift_crossfade_frames,
            silence_on_idle,
            on_input_failure: on_input_failure.0,
        };
        inner
            .validate()
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    #[getter]
    fn alignment_window_ms(&self) -> u64 {
        self.inner.alignment_window_ns / 1_000_000
    }

    #[getter]
    fn max_input_buffer_ms(&self) -> u64 {
        self.inner.max_input_buffer_ns / 1_000_000
    }

    #[getter]
    fn max_input_slots(&self) -> usize {
        self.inner.max_input_slots
    }

    #[getter]
    fn output_queue_slots(&self) -> usize {
        self.inner.output_queue_slots
    }

    #[getter]
    fn clamp_f32(&self) -> bool {
        self.inner.clamp_f32
    }

    #[getter]
    fn drift_threshold_ms(&self) -> u64 {
        self.inner.drift_threshold_ns / 1_000_000
    }

    #[getter]
    fn drift_hysteresis_ms(&self) -> u64 {
        self.inner.drift_hysteresis_ns / 1_000_000
    }

    #[getter]
    fn drift_hold_ms(&self) -> u64 {
        self.inner.drift_hold_ns / 1_000_000
    }

    #[getter]
    fn drift_crossfade_frames(&self) -> usize {
        self.inner.drift_crossfade_frames
    }

    #[getter]
    fn silence_on_idle(&self) -> bool {
        self.inner.silence_on_idle
    }

    #[getter]
    fn on_input_failure(&self) -> PyOnInputFailure {
        PyOnInputFailure(self.inner.on_input_failure)
    }
}

#[pyclass(
    name = "AggregateManagerBudgets",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyAggregateManagerBudgets {
    pub inner: AggregateManagerBudgets,
}

#[pymethods]
impl PyAggregateManagerBudgets {
    #[new]
    #[pyo3(signature = (
        max_targets=16,
        max_active_streams=16,
        max_pending_opens=4,
        max_native_graph_links=64,
        max_discovery_queue=256,
        max_total_pcm_memory=16777216,
        cleanup_timeout_ms=2000
    ))]
    pub fn new(
        max_targets: usize,
        max_active_streams: usize,
        max_pending_opens: usize,
        max_native_graph_links: usize,
        max_discovery_queue: usize,
        max_total_pcm_memory: usize,
        cleanup_timeout_ms: u64,
    ) -> PyResult<Self> {
        let inner = AggregateManagerBudgets {
            max_targets,
            max_active_streams,
            max_pending_opens,
            max_native_graph_links,
            max_discovery_queue,
            max_total_pcm_memory,
            cleanup_timeout_ns: cleanup_timeout_ms.saturating_mul(1_000_000),
        };
        inner
            .validate()
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    #[getter]
    fn max_targets(&self) -> usize {
        self.inner.max_targets
    }

    #[getter]
    fn max_active_streams(&self) -> usize {
        self.inner.max_active_streams
    }

    #[getter]
    fn max_pending_opens(&self) -> usize {
        self.inner.max_pending_opens
    }

    #[getter]
    fn max_native_graph_links(&self) -> usize {
        self.inner.max_native_graph_links
    }

    #[getter]
    fn max_discovery_queue(&self) -> usize {
        self.inner.max_discovery_queue
    }

    #[getter]
    fn max_total_pcm_memory(&self) -> usize {
        self.inner.max_total_pcm_memory
    }

    #[getter]
    fn cleanup_timeout_ms(&self) -> u64 {
        self.inner.cleanup_timeout_ns / 1_000_000
    }
}

#[pyclass(
    name = "TargetCandidateAttempt",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PyTargetCandidateAttempt {
    pub inner: TargetCandidateAttempt,
}

#[pymethods]
impl PyTargetCandidateAttempt {
    #[getter]
    fn backend(&self) -> PyBackendId {
        PyBackendId(self.inner.backend)
    }

    #[getter]
    fn acquisition(&self) -> Option<PyTargetAcquisitionKind> {
        self.inner
            .acquisition
            .map(|a| PyTargetAcquisitionKind(TargetAcquisitionTag::split(a).0))
    }

    #[getter]
    fn process_tree_strategy(&self) -> Option<PyProcessTreeStrategy> {
        self.inner
            .acquisition
            .and_then(|a| TargetAcquisitionTag::split(a).1)
            .map(PyProcessTreeStrategy)
    }

    #[getter]
    fn accepted(&self) -> bool {
        self.inner.accepted
    }

    #[getter]
    fn rejection(&self) -> Option<PyPlanErrorCode> {
        self.inner
            .rejection
            .map(|r| PyPlanErrorCode(PlanErrorTag::from(r)))
    }
}

#[pyclass(
    name = "TargetPlan",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PyTargetPlan {
    pub inner: TargetPlan,
}

#[pymethods]
impl PyTargetPlan {
    #[getter]
    fn target_index(&self) -> usize {
        self.inner.target_index
    }

    #[getter]
    fn target_id(&self) -> String {
        self.inner.target_id.to_string()
    }

    #[getter]
    fn resolved_backend(&self) -> PyBackendId {
        PyBackendId(self.inner.resolved_backend)
    }

    #[getter]
    fn acquisition(&self) -> PyTargetAcquisitionKind {
        PyTargetAcquisitionKind(TargetAcquisitionTag::split(self.inner.acquisition).0)
    }

    #[getter]
    fn process_tree_strategy(&self) -> Option<PyProcessTreeStrategy> {
        TargetAcquisitionTag::split(self.inner.acquisition)
            .1
            .map(PyProcessTreeStrategy)
    }

    #[getter]
    fn attempts(&self) -> Vec<PyTargetCandidateAttempt> {
        self.inner
            .attempts
            .iter()
            .cloned()
            .map(|inner| PyTargetCandidateAttempt { inner })
            .collect()
    }
}

#[pyclass(
    name = "CapturePlanReport",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, PartialEq)]
pub struct PyCapturePlanReport {
    pub inner: CapturePlanReport,
}

#[pymethods]
impl PyCapturePlanReport {
    #[getter]
    fn os(&self) -> PyPlatformOs {
        PyPlatformOs(self.inner.os)
    }

    #[getter]
    fn selected_backend(&self) -> Option<PyBackendId> {
        self.inner.selected_backend.map(PyBackendId)
    }

    #[getter]
    fn target_plans(&self) -> Vec<PyTargetPlan> {
        self.inner
            .target_plans
            .iter()
            .cloned()
            .map(|inner| PyTargetPlan { inner })
            .collect()
    }

    #[getter]
    fn combine_mode(&self) -> PyCombineMode {
        PyCombineMode(self.inner.combine_mode)
    }

    #[getter]
    fn combine_resolution(&self) -> PyCombineResolutionKind {
        PyCombineResolutionKind(CombineResolutionTag::split(self.inner.combine_resolution).0)
    }

    #[getter]
    fn combine_backend(&self) -> Option<PyBackendId> {
        CombineResolutionTag::split(self.inner.combine_resolution)
            .1
            .map(PyBackendId)
    }

    #[getter]
    fn tree_strategy(&self) -> PyProcessTreeRange {
        PyProcessTreeRange {
            inner: self.inner.tree_strategy,
        }
    }

    #[getter]
    fn input_conversion(&self) -> PyInputConversionPolicy {
        PyInputConversionPolicy(self.inner.input_conversion)
    }

    #[getter]
    fn backends(&self) -> PyBackends {
        PyBackends {
            inner: self.inner.backends.clone(),
        }
    }

    #[getter]
    fn uses_software_mixer(&self) -> bool {
        self.inner.uses_software_mixer
    }

    #[getter]
    fn uses_output_converter(&self) -> bool {
        self.inner.uses_output_converter
    }

    #[getter]
    fn requires_discovery_subscription(&self) -> bool {
        self.inner.requires_discovery_subscription
    }

    #[getter]
    fn overlap_semantics(&self) -> PyOverlapSemantics {
        PyOverlapSemantics(self.inner.overlap_semantics)
    }

    #[getter]
    fn duplicate_target_ids(&self) -> Vec<String> {
        self.inner
            .duplicate_target_ids
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[getter]
    fn allow_reroute(&self) -> bool {
        self.inner.allow_reroute
    }

    #[getter]
    fn permission_policy(&self) -> PyPermissionPolicy {
        PyPermissionPolicy(self.inner.permission_policy)
    }

    #[getter]
    fn estimated_active_streams(&self) -> usize {
        self.inner.estimated_active_streams
    }

    #[getter]
    fn estimated_pcm_bytes(&self) -> usize {
        self.inner.estimated_pcm_bytes
    }
}

#[pyclass(
    name = "PermissionEntry",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyPermissionEntry {
    pub inner: PermissionEntry,
}

#[pymethods]
impl PyPermissionEntry {
    #[getter]
    fn kind(&self) -> PyPermissionKind {
        PyPermissionKind(self.inner.kind)
    }

    #[getter]
    fn state(&self) -> PyPermissionState {
        PyPermissionState(self.inner.state)
    }

    #[getter]
    fn required(&self) -> bool {
        self.inner.required
    }

    #[getter]
    fn can_prompt(&self) -> bool {
        self.inner.can_prompt
    }

    #[getter]
    fn requires_relaunch(&self) -> bool {
        self.inner.requires_relaunch
    }

    #[getter]
    fn is_granted(&self) -> bool {
        self.inner.is_granted()
    }
}

#[pyclass(
    name = "PermissionReport",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyPermissionReport {
    pub inner: PermissionReport,
}

#[pymethods]
impl PyPermissionReport {
    #[getter]
    fn os(&self) -> PyPlatformOs {
        PyPlatformOs(self.inner.os)
    }

    #[getter]
    fn is_granted(&self) -> bool {
        self.inner.is_granted()
    }

    #[getter]
    fn has_denial(&self) -> bool {
        self.inner.has_denial()
    }

    #[getter]
    fn requires_relaunch(&self) -> bool {
        self.inner.requires_relaunch()
    }

    #[getter]
    fn entries(&self) -> Vec<PyPermissionEntry> {
        self.inner
            .entries()
            .iter()
            .copied()
            .map(|inner| PyPermissionEntry { inner })
            .collect()
    }

    #[getter]
    fn required_kinds(&self) -> Vec<PyPermissionKind> {
        self.inner
            .required_kinds()
            .into_iter()
            .map(PyPermissionKind)
            .collect()
    }

    #[getter]
    fn microphone(&self) -> PyPermissionEntry {
        PyPermissionEntry {
            inner: self.inner.microphone(),
        }
    }

    #[getter]
    fn system_audio(&self) -> PyPermissionEntry {
        PyPermissionEntry {
            inner: self.inner.system_audio(),
        }
    }

    #[getter]
    fn screen_recording(&self) -> PyPermissionEntry {
        PyPermissionEntry {
            inner: self.inner.screen_recording(),
        }
    }

    fn entry(&self, kind: PyPermissionKind) -> PyPermissionEntry {
        PyPermissionEntry {
            inner: self.inner.entry(kind.0),
        }
    }

    fn state_for(&self, kind: PyPermissionKind) -> PyPermissionState {
        PyPermissionState(self.inner.state_for(kind.0))
    }

    fn __bool__(&self) -> bool {
        self.inner.is_granted()
    }
}

#[pyclass(
    name = "DiscoveryQuery",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, PartialEq)]
pub struct PyDiscoveryQuery {
    pub inner: DiscoveryQuery,
}

#[pymethods]
impl PyDiscoveryQuery {
    #[new]
    #[pyo3(signature = (
        backends=None,
        dedupe=PyDiscoveryDedupe(DiscoveryDedupe::ByLogicalTarget),
        order=PyDiscoveryOrder(DiscoveryOrder::KindThenBackend),
        active_only=false,
        include_non_audio=false,
        include_applications=true,
        include_processes=true,
        include_output_devices=true,
        include_inputs=true,
        os=None
    ))]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backends: Option<PyBackends>,
        dedupe: PyDiscoveryDedupe,
        order: PyDiscoveryOrder,
        active_only: bool,
        include_non_audio: bool,
        include_applications: bool,
        include_processes: bool,
        include_output_devices: bool,
        include_inputs: bool,
        os: Option<PyPlatformOs>,
    ) -> PyResult<Self> {
        let resolved_backends = backends.map_or_else(Backends::default, |b| b.inner);
        resolved_backends
            .validate()
            .map_err(|e| PyValueError::new_err(format!("{e:?}")))?;
        Ok(Self {
            inner: DiscoveryQuery {
                os: os.map_or_else(PlatformOs::current, |o| o.0),
                backends: resolved_backends,
                dedupe: dedupe.0,
                order: order.0,
                active_only,
                include_non_audio,
                include_applications,
                include_processes,
                include_output_devices,
                include_input_devices: include_inputs,
            },
        })
    }

    #[getter]
    fn os(&self) -> PyPlatformOs {
        PyPlatformOs(self.inner.os)
    }

    #[getter]
    fn backends(&self) -> PyBackends {
        PyBackends {
            inner: self.inner.backends.clone(),
        }
    }

    #[getter]
    fn dedupe(&self) -> PyDiscoveryDedupe {
        PyDiscoveryDedupe(self.inner.dedupe)
    }

    #[getter]
    fn order(&self) -> PyDiscoveryOrder {
        PyDiscoveryOrder(self.inner.order)
    }

    #[getter]
    fn active_only(&self) -> bool {
        self.inner.active_only
    }

    #[getter]
    fn include_non_audio(&self) -> bool {
        self.inner.include_non_audio
    }

    #[getter]
    fn include_applications(&self) -> bool {
        self.inner.include_applications
    }

    #[getter]
    fn include_processes(&self) -> bool {
        self.inner.include_processes
    }

    #[getter]
    fn include_output_devices(&self) -> bool {
        self.inner.include_output_devices
    }

    #[getter]
    fn include_inputs(&self) -> bool {
        self.inner.include_input_devices
    }
}

#[pyclass(
    name = "TargetSnapshot",
    module = "capture_system_or_process_audio",
    frozen,
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct PyTargetSnapshot {
    #[pyo3(get)]
    pub revision: u64,
    pub targets: Vec<PyCaptureTarget>,
}

impl From<TargetSnapshot> for PyTargetSnapshot {
    fn from(snap: TargetSnapshot) -> Self {
        Self {
            revision: snap.cursor.revision,
            targets: snap
                .targets
                .into_iter()
                .map(|inner| PyCaptureTarget { inner })
                .collect(),
        }
    }
}

#[pymethods]
impl PyTargetSnapshot {
    #[getter]
    fn targets(&self) -> Vec<PyCaptureTarget> {
        self.targets.clone()
    }
}

#[pyclass(
    name = "TargetCatalog",
    module = "capture_system_or_process_audio",
    frozen,
    from_py_object
)]
#[derive(Clone, Debug)]
pub struct PyTargetCatalog {
    pub inner: TargetCatalog,
}

#[pymethods]
impl PyTargetCatalog {
    #[getter]
    fn revision(&self) -> u64 {
        self.inner.cursor.revision
    }

    #[getter]
    fn targets(&self) -> Vec<PyCaptureTarget> {
        self.inner
            .targets
            .iter()
            .cloned()
            .map(|inner| PyCaptureTarget { inner })
            .collect()
    }

    #[getter]
    fn applications(&self) -> Vec<PyCaptureTarget> {
        self.inner
            .applications
            .iter()
            .cloned()
            .map(|inner| PyCaptureTarget { inner })
            .collect()
    }

    #[getter]
    fn processes(&self) -> Vec<PyCaptureTarget> {
        self.inner
            .processes
            .iter()
            .cloned()
            .map(|inner| PyCaptureTarget { inner })
            .collect()
    }

    #[getter]
    fn output_devices(&self) -> Vec<PyCaptureTarget> {
        self.inner
            .output_devices
            .iter()
            .cloned()
            .map(|inner| PyCaptureTarget { inner })
            .collect()
    }

    #[getter]
    fn input_devices(&self) -> Vec<PyCaptureTarget> {
        self.inner
            .input_devices
            .iter()
            .cloned()
            .map(|inner| PyCaptureTarget { inner })
            .collect()
    }

    #[getter]
    fn default_output_device_id(&self) -> Option<String> {
        self.inner
            .default_output_device_id
            .as_ref()
            .map(ToString::to_string)
    }

    #[getter]
    fn default_input_device_id(&self) -> Option<String> {
        self.inner
            .default_input_device_id
            .as_ref()
            .map(ToString::to_string)
    }
}

#[pyclass(
    name = "DiscoveryEvent",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, PartialEq)]
pub struct PyDiscoveryEvent {
    pub inner: DiscoveryEvent,
}

impl From<DiscoveryEvent> for PyDiscoveryEvent {
    fn from(inner: DiscoveryEvent) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl PyDiscoveryEvent {
    #[getter]
    fn revision(&self) -> u64 {
        self.inner.revision
    }

    #[getter]
    fn kind(&self) -> PyDiscoveryEventKind {
        PyDiscoveryEventKind(DiscoveryEventTag::from(&self.inner.kind))
    }

    #[getter]
    fn target(&self) -> Option<PyCaptureTarget> {
        match &self.inner.kind {
            DiscoveryEventKind::Added(t) | DiscoveryEventKind::StateChanged(t) => {
                Some(PyCaptureTarget { inner: t.clone() })
            }
            _ => None,
        }
    }

    #[getter]
    fn target_id(&self) -> Option<String> {
        match &self.inner.kind {
            DiscoveryEventKind::Added(t) | DiscoveryEventKind::StateChanged(t) => {
                Some(t.id().to_string())
            }
            DiscoveryEventKind::Removed(id) => Some(id.to_string()),
            _ => None,
        }
    }

    #[getter]
    fn device_id(&self) -> Option<String> {
        match &self.inner.kind {
            DiscoveryEventKind::DefaultDeviceChanged { device_id }
            | DiscoveryEventKind::DefaultInputChanged { device_id } => Some(device_id.to_string()),
            _ => None,
        }
    }

    #[getter]
    fn failure_reason(&self) -> Option<String> {
        match &self.inner.kind {
            DiscoveryEventKind::WatcherFailed { reason } => Some(reason.to_string()),
            _ => None,
        }
    }

    #[getter]
    fn visibility(&self) -> Option<PyVisibility> {
        match &self.inner.kind {
            DiscoveryEventKind::VisibilityChanged(v) => Some(PyVisibility(*v)),
            _ => None,
        }
    }

    #[getter]
    fn minimum_revision(&self) -> Option<u64> {
        match &self.inner.kind {
            DiscoveryEventKind::ResnapshotRequired { minimum_revision } => Some(*minimum_revision),
            _ => None,
        }
    }
}

#[pyclass(
    name = "Cancellation",
    module = "capture_system_or_process_audio",
    frozen,
    from_py_object
)]
#[derive(Clone, Default)]
pub struct PyCancellation {
    pub inner: Cancellation,
}

#[pymethods]
impl PyCancellation {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

#[repr(C)]
struct BufferMeta {
    format: CString,
    shape: [isize; 2],
    strides: [isize; 2],
}

#[pyclass(name = "PcmBlock", module = "capture_system_or_process_audio", frozen)]
pub struct PyPcmBlock {
    block: OwnedBlock,
    first: usize,
    frames: usize,
    meta: Box<BufferMeta>,
    materialized_copies: Arc<AtomicU64>,
}

impl PyPcmBlock {
    pub fn from_slice(slice: &Slice) -> Self {
        let format = slice.block.format();
        let channels = format.channels() as isize;
        let itemsize = match format.sample() {
            SampleType::I16 => 2isize,
            SampleType::I24 => 1isize,
            SampleType::I32 | SampleType::F32 => 4isize,
        };
        let fmt_code = match (format.sample(), format.order()) {
            (SampleType::F32, ByteOrder::Little) => "<f",
            (SampleType::F32, ByteOrder::Big) => ">f",
            (SampleType::I32, ByteOrder::Little) => "<i",
            (SampleType::I32, ByteOrder::Big) => ">i",
            (SampleType::I16, ByteOrder::Little) => "<h",
            (SampleType::I16, ByteOrder::Big) => ">h",
            (SampleType::I24, _) => "B",
        };
        let dim1 = if format.sample() == SampleType::I24 {
            channels * 3
        } else {
            channels
        };
        let frames_isize = slice.frames as isize;
        Self {
            block: slice.block.view(),
            first: slice.first,
            frames: slice.frames,
            meta: Box::new(BufferMeta {
                format: CString::new(fmt_code).unwrap(),
                shape: [frames_isize, dim1],
                strides: [dim1 * itemsize, itemsize],
            }),
            materialized_copies: Arc::new(AtomicU64::new(0)),
        }
    }

    #[inline]
    fn slice_bytes(&self) -> &[u8] {
        let stride = self.block.format().bytes_per_frame();
        &self.block.bytes()[self.first * stride..(self.first + self.frames) * stride]
    }
}

#[pymethods]
impl PyPcmBlock {
    #[getter]
    fn frames(&self) -> usize {
        self.frames
    }

    #[getter]
    fn channels(&self) -> u16 {
        self.block.format().channels()
    }

    #[getter]
    fn rate(&self) -> u32 {
        self.block.format().rate()
    }

    #[getter]
    fn sample_format(&self) -> PySampleType {
        PySampleType(self.block.format().sample())
    }

    #[getter]
    fn byte_order(&self) -> PyByteOrder {
        PyByteOrder(self.block.format().order())
    }

    #[getter]
    fn sequence(&self) -> u64 {
        self.block.header().sequence
    }

    #[getter]
    fn segment(&self) -> u64 {
        self.block.header().segment
    }

    #[getter]
    fn generation(&self) -> u64 {
        self.block.header().generation
    }

    #[getter]
    fn data_ptr(&self) -> usize {
        self.slice_bytes().as_ptr() as usize
    }

    #[getter]
    fn byte_len(&self) -> usize {
        self.slice_bytes().len()
    }

    #[getter]
    fn materialized_copies(&self) -> u64 {
        self.materialized_copies.load(SeqCst)
    }

    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        self.materialized_copies.fetch_add(1, SeqCst);
        PyBytes::new(py, self.slice_bytes())
    }

    #[pyo3(signature = (copy=false, unpack_i24=false))]
    fn to_numpy<'py>(
        slf: Bound<'py, Self>,
        py: Python<'py>,
        copy: bool,
        unpack_i24: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let this = slf.get();
        let sample = this.block.format().sample();
        let order = this.block.format().order();
        let frames = this.frames;
        let channels = usize::from(this.block.format().channels());

        if sample == SampleType::I24 && !unpack_i24 {
            return Err(PyValueError::new_err(
                "packed 24-bit PCM (i24) cannot be reinterpreted as a native NumPy scalar dtype without unpack_i24=True",
            ));
        }
        let np = py.import("numpy").map_err(|_| {
            PyImportError::new_err("numpy is not installed or available in this Python environment")
        })?;
        if sample == SampleType::I24 {
            let raw = this.slice_bytes();
            let mut out = Vec::with_capacity(frames * channels * 4);
            for chunk in raw.chunks_exact(3) {
                let v = match order {
                    ByteOrder::Little => {
                        let u = u32::from(chunk[0])
                            | (u32::from(chunk[1]) << 8)
                            | (u32::from(chunk[2]) << 16);
                        if (u & 0x0080_0000) != 0 {
                            (u | 0xFF00_0000) as i32
                        } else {
                            u as i32
                        }
                    }
                    ByteOrder::Big => {
                        let u = (u32::from(chunk[0]) << 16)
                            | (u32::from(chunk[1]) << 8)
                            | u32::from(chunk[2]);
                        if (u & 0x0080_0000) != 0 {
                            (u | 0xFF00_0000) as i32
                        } else {
                            u as i32
                        }
                    }
                };
                out.extend_from_slice(&v.to_le_bytes());
            }
            this.materialized_copies.fetch_add(1, SeqCst);
            let py_bytes = PyBytes::new(py, &out);
            let flat = np.call_method1("frombuffer", (py_bytes, "<i4"))?;
            let shaped = flat.call_method1("reshape", ((frames, channels),))?;
            return shaped.call_method0("copy");
        }

        let arr = np.call_method1("asarray", (&slf,))?;
        if copy {
            this.materialized_copies.fetch_add(1, SeqCst);
            arr.call_method0("copy")
        } else {
            Ok(arr)
        }
    }

    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        if view.is_null() {
            return Err(PyBufferError::new_err("null Py_buffer pointer"));
        }
        if (flags & ffi::PyBUF_WRITABLE) == ffi::PyBUF_WRITABLE {
            return Err(PyBufferError::new_err(
                "PcmBlock is a read-only zero-copy view",
            ));
        }
        let this = slf.get();
        let bytes = this.slice_bytes();
        let format = this.block.format();
        let itemsize = match format.sample() {
            SampleType::I16 => 2isize,
            SampleType::I24 => 1isize,
            SampleType::I32 | SampleType::F32 => 4isize,
        };
        // SAFETY: `view` is checked non-null; `this` (`PyPcmBlock`) is immutable (`frozen`) and kept
        // alive via `view.obj = slf.clone().into_any().into_ptr()` until Python releases the buffer.
        unsafe {
            (*view).buf = bytes.as_ptr() as *mut c_void;
            (*view).obj = slf.clone().into_any().into_ptr();
            (*view).len = bytes.len() as isize;
            (*view).readonly = 1;
            (*view).itemsize = itemsize;
            (*view).format = if (flags & ffi::PyBUF_FORMAT) == ffi::PyBUF_FORMAT {
                this.meta.format.as_ptr() as *mut c_char
            } else {
                std::ptr::null_mut()
            };
            if (flags & ffi::PyBUF_ND) == ffi::PyBUF_ND {
                (*view).ndim = 2;
                (*view).shape = this.meta.shape.as_ptr() as *mut isize;
                (*view).strides = if (flags & ffi::PyBUF_STRIDES) == ffi::PyBUF_STRIDES {
                    this.meta.strides.as_ptr() as *mut isize
                } else {
                    std::ptr::null_mut()
                };
            } else {
                (*view).ndim = 1;
                (*view).shape = std::ptr::null_mut();
                (*view).strides = std::ptr::null_mut();
            }
            (*view).suboffsets = std::ptr::null_mut();
            (*view).internal = std::ptr::null_mut();
        }
        Ok(())
    }

    unsafe fn __releasebuffer__(&self, _view: *mut ffi::Py_buffer) {}
}

#[pyclass(
    name = "MixedBlockMetadata",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyMixedBlockMetadata {
    pub inner: MixedBlockMetadata,
}

#[pymethods]
impl PyMixedBlockMetadata {
    #[getter]
    fn membership_version(&self) -> u64 {
        self.inner.membership_version
    }

    #[getter]
    fn contributing_mask(&self) -> u64 {
        self.inner.contributing_mask
    }

    #[getter]
    fn late_inputs(&self) -> u64 {
        self.inner.late_inputs
    }

    #[getter]
    fn failed_inputs(&self) -> u64 {
        self.inner.failed_inputs
    }

    #[getter]
    fn nonfinite_inputs(&self) -> u64 {
        self.inner.nonfinite_inputs
    }

    #[getter]
    fn synthesized_silence(&self) -> bool {
        self.inner.synthesized_silence
    }

    #[getter]
    fn clipped_samples(&self) -> u64 {
        self.inner.clipped_samples
    }

    #[getter]
    fn nonfinite_samples(&self) -> u64 {
        self.inner.nonfinite_samples
    }

    #[getter]
    fn drift_slips(&self) -> u64 {
        self.inner.drift_slips
    }

    #[getter]
    fn skew_estimate_ns(&self) -> Option<u64> {
        self.inner.skew_estimate_ns
    }

    #[getter]
    fn skew_uncertainty_ns(&self) -> Option<u64> {
        self.inner.skew_uncertainty_ns
    }
}

#[pyclass(
    name = "InputStatusHandle",
    module = "capture_system_or_process_audio",
    frozen,
    eq,
    from_py_object
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PyInputStatusHandle {
    pub target_index: usize,
    pub target_id: String,
    pub backend: BackendId,
    pub participation_state: ParticipationStateTag,
    pub failure_code: Option<u32>,
    pub native_format: Format,
    pub mixer_format: Format,
    pub is_resampled: bool,
    pub is_identity_bypass: bool,
    pub queued_packets: usize,
    pub dropped_frames: u64,
    pub clipped_samples: u64,
    pub nonfinite_samples: u64,
    pub drift_slips: u64,
}

impl From<&InputStatusHandle> for PyInputStatusHandle {
    fn from(h: &InputStatusHandle) -> Self {
        let snap = h.snapshot();
        let (state_tag, fail_code) = snap
            .as_ref()
            .map_or((ParticipationStateTag::Starting, None), |s| {
                ParticipationStateTag::split(s.participation_state)
            });
        Self {
            target_index: h.target_index(),
            target_id: h.target_id().to_string(),
            backend: h.backend(),
            participation_state: state_tag,
            failure_code: fail_code,
            native_format: (**h.native_format()).clone(),
            mixer_format: (**h.mixer_format()).clone(),
            is_resampled: h.is_resampled(),
            is_identity_bypass: h.is_identity_bypass(),
            queued_packets: snap.as_ref().map_or(0, |s| s.queued_packets),
            dropped_frames: snap.as_ref().map_or(0, |s| s.dropped_frames),
            clipped_samples: snap.as_ref().map_or(0, |s| s.clipped_samples),
            nonfinite_samples: snap.as_ref().map_or(0, |s| s.nonfinite_samples),
            drift_slips: snap.as_ref().map_or(0, |s| s.drift_slips),
        }
    }
}

#[pymethods]
impl PyInputStatusHandle {
    #[getter]
    fn target_index(&self) -> usize {
        self.target_index
    }

    #[getter]
    fn target_id(&self) -> String {
        self.target_id.clone()
    }

    #[getter]
    fn backend(&self) -> PyBackendId {
        PyBackendId(self.backend)
    }

    #[getter]
    fn participation_state(&self) -> PyParticipationState {
        PyParticipationState(self.participation_state)
    }

    #[getter]
    fn failure_code(&self) -> Option<u32> {
        self.failure_code
    }

    #[getter]
    fn native_format(&self) -> PyFormat {
        PyFormat {
            inner: self.native_format.clone(),
        }
    }

    #[getter]
    fn mixer_format(&self) -> PyFormat {
        PyFormat {
            inner: self.mixer_format.clone(),
        }
    }

    #[getter]
    fn is_resampled(&self) -> bool {
        self.is_resampled
    }

    #[getter]
    fn is_identity_bypass(&self) -> bool {
        self.is_identity_bypass
    }

    #[getter]
    fn queued_packets(&self) -> usize {
        self.queued_packets
    }

    #[getter]
    fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    #[getter]
    fn clipped_samples(&self) -> u64 {
        self.clipped_samples
    }

    #[getter]
    fn nonfinite_samples(&self) -> u64 {
        self.nonfinite_samples
    }

    #[getter]
    fn drift_slips(&self) -> u64 {
        self.drift_slips
    }
}

#[pyclass(
    name = "ReadOutcome",
    module = "capture_system_or_process_audio",
    frozen
)]
pub struct PyReadOutcome {
    pub frames: usize,
    pub reason: PartialReasonTag,
    pub failure: Option<Failure>,
    pub session: u64,
    pub segment: u64,
    pub boundary_kind: Option<BoundaryKind>,
    pub copy_bytes: u64,
    pub copy_destinations: u64,
    pub mixed_metadata: Option<PyMixedBlockMetadata>,
    pub blocks: Vec<Py<PyPcmBlock>>,
}

#[pymethods]
impl PyReadOutcome {
    #[getter]
    fn frames(&self) -> usize {
        self.frames
    }

    #[getter]
    fn reason(&self) -> PyPartialReason {
        PyPartialReason(self.reason)
    }

    #[getter]
    fn failure(&self) -> Option<PyFailureKind> {
        self.failure.map(PyFailureKind)
    }

    #[getter]
    fn session(&self) -> u64 {
        self.session
    }

    #[getter]
    fn segment(&self) -> u64 {
        self.segment
    }

    #[getter]
    fn boundary_kind(&self) -> Option<PyBoundaryKind> {
        self.boundary_kind.map(PyBoundaryKind)
    }

    #[getter]
    fn copy_bytes(&self) -> u64 {
        self.copy_bytes
    }

    #[getter]
    fn copy_destinations(&self) -> u64 {
        self.copy_destinations
    }

    #[getter]
    fn mixed_metadata(&self) -> Option<PyMixedBlockMetadata> {
        self.mixed_metadata
    }

    #[getter]
    fn blocks<'py>(&self, py: Python<'py>) -> Vec<Py<PyPcmBlock>> {
        self.blocks.iter().map(|b| b.clone_ref(py)).collect()
    }
}

pub fn build_py_outcome(
    py: Python<'_>,
    session: u64,
    segment: u64,
    boundary_kind: Option<BoundaryKind>,
    mixed_metadata: Option<MixedBlockMetadata>,
    result: ReadResult,
) -> PyResult<PyReadOutcome> {
    let mut blocks = Vec::with_capacity(result.chunks.len());
    for slice in &result.chunks {
        blocks.push(Py::new(py, PyPcmBlock::from_slice(slice))?);
    }
    let (reason_tag, failure_opt) = PartialReasonTag::split(result.reason);
    let resolved_bkind = boundary_kind.or({
        if result.reason == PartialReason::Boundary {
            Some(BoundaryKind::DeviceChanged)
        } else {
            None
        }
    });
    Ok(PyReadOutcome {
        frames: result.frames,
        reason: reason_tag,
        failure: failure_opt,
        session,
        segment,
        boundary_kind: resolved_bkind,
        copy_bytes: result.copies.bytes,
        copy_destinations: result.copies.destinations,
        mixed_metadata: mixed_metadata.map(|inner| PyMixedBlockMetadata { inner }),
        blocks,
    })
}
