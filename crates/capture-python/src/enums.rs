use cspa_facade::{
    BackendId, BoundaryKind, ByteOrder, Channel, CombineMode, CombineResolution, DefaultDeviceMode,
    DiscoveryDedupe, DiscoveryEventKind, DiscoveryOrder, DitherMode, Failure,
    InputConversionPolicy, OnFormatChange, OnInputFailure, OnRootExit, Overflow,
    OverlapCombinationSemantics, PartialReason, ParticipationState, PermissionKind,
    PermissionPolicy, PermissionState, PlanError, PlatformOs, ProcessScope, ProcessTreeStrategy,
    SampleType, Stop, TargetKind, TimingRequirement,
    core::{manager::Visibility, schema::TargetAcquisition},
};
use pyo3::{
    Borrowed, Bound, FromPyObject, IntoPyObject, Py, PyAny, PyErr, PyResult, Python,
    exceptions::{PyTypeError, PyValueError},
    prelude::PyAnyMethods,
    sync::PyOnceLock,
    type_hint_union,
    types::{PyModule, PyModuleMethods, PyString, PyStringMethods, PyTypeMethods},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumVariantSpec {
    pub member: &'static str,
    pub value: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumClassSpec {
    pub name: &'static str,
    pub variants: &'static [EnumVariantSpec],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootExitPolicyTag {
    Stop,
    Wait,
}

impl From<OnRootExit> for RootExitPolicyTag {
    fn from(value: OnRootExit) -> Self {
        match value {
            OnRootExit::Stop => Self::Stop,
            OnRootExit::Wait { .. } => Self::Wait,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CombineResolutionTag {
    PassThrough,
    SharedNativeGraphMix,
    CspaMixer,
}

impl CombineResolutionTag {
    pub fn split(value: CombineResolution) -> (Self, Option<BackendId>) {
        match value {
            CombineResolution::PassThrough(b) => (Self::PassThrough, Some(b)),
            CombineResolution::SharedNativeGraphMix(b) => (Self::SharedNativeGraphMix, Some(b)),
            CombineResolution::CspaMixer => (Self::CspaMixer, None),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetAcquisitionTag {
    DirectEndpoint,
    ProcessTree,
}

impl TargetAcquisitionTag {
    pub fn split(value: TargetAcquisition) -> (Self, Option<ProcessTreeStrategy>) {
        match value {
            TargetAcquisition::DirectEndpoint => (Self::DirectEndpoint, None),
            TargetAcquisition::ProcessTree(s) => (Self::ProcessTree, Some(s)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanErrorTag {
    InvalidTarget,
    UnsupportedScope,
    ReversedStrategyRange,
    InvalidBackend,
    BackendUnavailable,
    PermissionDenied,
    NoSuchProcess,
    PidReused,
    PreciseTimingUnqualified,
    UnsupportedCapturePlan,
    Capacity,
}

impl From<PlanError> for PlanErrorTag {
    fn from(value: PlanError) -> Self {
        match value {
            PlanError::InvalidTarget => Self::InvalidTarget,
            PlanError::UnsupportedScope => Self::UnsupportedScope,
            PlanError::ReversedStrategyRange => Self::ReversedStrategyRange,
            PlanError::InvalidBackend => Self::InvalidBackend,
            PlanError::BackendUnavailable => Self::BackendUnavailable,
            PlanError::PermissionDenied => Self::PermissionDenied,
            PlanError::NoSuchProcess => Self::NoSuchProcess,
            PlanError::PidReused => Self::PidReused,
            PlanError::PreciseTimingUnqualified => Self::PreciseTimingUnqualified,
            PlanError::UnsupportedCapturePlan => Self::UnsupportedCapturePlan,
            PlanError::Capacity(_) => Self::Capacity,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryEventTag {
    Added,
    Removed,
    StateChanged,
    DefaultDeviceChanged,
    DefaultInputChanged,
    WatcherFailed,
    VisibilityChanged,
    ResnapshotRequired,
}

impl From<&DiscoveryEventKind> for DiscoveryEventTag {
    fn from(value: &DiscoveryEventKind) -> Self {
        match value {
            DiscoveryEventKind::Added(_) => Self::Added,
            DiscoveryEventKind::Removed(_) => Self::Removed,
            DiscoveryEventKind::StateChanged(_) => Self::StateChanged,
            DiscoveryEventKind::DefaultDeviceChanged { .. } => Self::DefaultDeviceChanged,
            DiscoveryEventKind::DefaultInputChanged { .. } => Self::DefaultInputChanged,
            DiscoveryEventKind::WatcherFailed { .. } => Self::WatcherFailed,
            DiscoveryEventKind::VisibilityChanged(_) => Self::VisibilityChanged,
            DiscoveryEventKind::ResnapshotRequired { .. } => Self::ResnapshotRequired,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartialReasonTag {
    Complete,
    WouldBlock,
    Boundary,
    Cancelled,
    Timeout,
    Eof,
    Failure,
    Ready,
}

impl PartialReasonTag {
    pub fn split(value: PartialReason) -> (Self, Option<Failure>) {
        match value {
            PartialReason::Complete => (Self::Complete, None),
            PartialReason::WouldBlock => (Self::WouldBlock, None),
            PartialReason::Boundary => (Self::Boundary, None),
            PartialReason::Cancelled => (Self::Cancelled, None),
            PartialReason::Timeout => (Self::Timeout, None),
            PartialReason::Eof => (Self::Eof, None),
            PartialReason::Failure(f) => (Self::Failure, Some(f)),
            PartialReason::Ready => (Self::Ready, None),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipationStateTag {
    Starting,
    Active,
    Idle,
    WaitingRelaunch,
    Late,
    Exited,
    Failed,
}

impl ParticipationStateTag {
    pub fn split(value: ParticipationState) -> (Self, Option<u32>) {
        match value {
            ParticipationState::Starting => (Self::Starting, None),
            ParticipationState::Active => (Self::Active, None),
            ParticipationState::Idle => (Self::Idle, None),
            ParticipationState::WaitingRelaunch => (Self::WaitingRelaunch, None),
            ParticipationState::Late => (Self::Late, None),
            ParticipationState::Exited => (Self::Exited, None),
            ParticipationState::Failed(code) => (Self::Failed, Some(code)),
        }
    }
}

struct CachedPyEnum {
    cls: Py<PyAny>,
    members: Vec<Py<PyAny>>,
}

impl CachedPyEnum {
    fn create(
        py: Python<'_>,
        enum_ctor: &Bound<'_, PyAny>,
        spec: &EnumClassSpec,
    ) -> PyResult<Self> {
        let pairs: Vec<(&'static str, &'static str)> =
            spec.variants.iter().map(|v| (v.member, v.value)).collect();
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("module", "capture_system_or_process_audio")?;
        let cls_bound = enum_ctor.call((spec.name, pairs), Some(&kwargs))?;
        let mut members = Vec::with_capacity(spec.variants.len());
        for v in spec.variants {
            members.push(cls_bound.getattr(v.member)?.unbind());
        }
        Ok(Self {
            cls: cls_bound.unbind(),
            members,
        })
    }
}

macro_rules! define_cspa_enums {
    (
        $(
            ($field:ident, $wrapper:ident, $rust_ty:ty, $py_name:literal) {
                $( $variant:path => ($member:literal, $wire:literal) ),+ $(,)?
            }
        );+ $(;)?
    ) => {
        pub struct EnumRegistry {
            stdlib_enum_base: Py<PyAny>,
            $( $field: CachedPyEnum, )+
        }

        static ENUM_REGISTRY: PyOnceLock<EnumRegistry> = PyOnceLock::new();

        pub fn registry(py: Python<'_>) -> PyResult<&'static EnumRegistry> {
            ENUM_REGISTRY.get_or_try_init(py, || {
                let enum_mod = py.import("enum")?;
                let enum_base = enum_mod.getattr("Enum")?;
                Ok(EnumRegistry {
                    stdlib_enum_base: enum_base.clone().unbind(),
                    $(
                        $field: CachedPyEnum::create(py, &enum_base, &$wrapper::SPEC)?,
                    )+
                })
            })
        }

        pub fn register_all_on_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
            let py = m.py();
            let reg = registry(py)?;
            $(
                m.add($py_name, reg.$field.cls.bind(py))?;
            )+
            Ok(())
        }

        pub const ALL_ENUM_SPECS: &[EnumClassSpec] = &[
            $( $wrapper::SPEC, )+
        ];

        $(
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct $wrapper(pub $rust_ty);

            impl $wrapper {
                pub const SPEC: EnumClassSpec = EnumClassSpec {
                    name: $py_name,
                    variants: &[
                        $( EnumVariantSpec { member: $member, value: $wire }, )+
                    ],
                };

                pub const ALL: &'static [$rust_ty] = &[
                    $( $variant, )+
                ];

                #[inline]
                pub const fn variant_index(value: $rust_ty) -> usize {
                    let mut idx = 0usize;
                    $(
                        if matches!(value, $variant) {
                            return idx;
                        }
                        idx += 1;
                    )+
                    let _ = idx;
                    // Exhaustive match check without catch-all `_` so adding a Rust variant fails compilation:
                    match value {
                        $( $variant => 0, )+
                    }
                }

                #[inline]
                pub const fn as_str(value: $rust_ty) -> &'static str {
                    match value {
                        $( $variant => $wire, )+
                    }
                }

                pub fn from_wire_str(raw: &str) -> PyResult<$rust_ty> {
                    match raw {
                        $( $wire => Ok($variant), )+
                        _ => Err(PyValueError::new_err(format!(
                            "invalid {} value {:?}; expected one of {:?}",
                            $py_name,
                            raw,
                            &[ $( $wire ),+ ]
                        ))),
                    }
                }

                pub fn parse_obj(obj: &Bound<'_, PyAny>) -> PyResult<$rust_ty> {
                    let py = obj.py();
                    let reg = registry(py)?;
                    let cached = &reg.$field;
                    for (idx, member_py) in cached.members.iter().enumerate() {
                        if obj.is(member_py) {
                            return Ok(Self::ALL[idx]);
                        }
                    }
                    if obj.is_instance(cached.cls.bind(py))? {
                        let val_obj = obj.getattr("value")?;
                        let s = val_obj.cast::<PyString>().map_err(|_| {
                            PyTypeError::new_err(format!("invalid {} member value", $py_name))
                        })?;
                        return Self::from_wire_str(s.to_str()?);
                    }
                    if obj.is_instance(reg.stdlib_enum_base.bind(py))? {
                        let ty_name = obj
                            .get_type()
                            .qualname()
                            .map(|q| q.to_string())
                            .unwrap_or_else(|_| "<Enum>".to_string());
                        return Err(PyTypeError::new_err(format!(
                            "expected {} or str, got foreign enum {}",
                            $py_name, ty_name
                        )));
                    }
                    if let Ok(py_str) = obj.cast::<PyString>() {
                        return Self::from_wire_str(py_str.to_str()?);
                    }
                    let ty_name = obj
                        .get_type()
                        .name()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|_| "<object>".to_string());
                    Err(PyTypeError::new_err(format!(
                        "expected {} or str, got {}",
                        $py_name, ty_name
                    )))
                }

                pub fn into_member<'py>(value: $rust_ty, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
                    let reg = registry(py)?;
                    let idx = Self::variant_index(value);
                    Ok(reg.$field.members[idx].bind(py).clone())
                }
            }

            impl From<$rust_ty> for $wrapper {
                #[inline]
                fn from(value: $rust_ty) -> Self {
                    Self(value)
                }
            }

            impl<'a, 'py> FromPyObject<'a, 'py> for $wrapper {
                type Error = PyErr;

                const INPUT_TYPE: pyo3::inspect::PyStaticExpr = pyo3::type_hint_union!(
                    pyo3::type_hint_identifier!("capture_system_or_process_audio", $py_name),
                    pyo3::type_hint_identifier!("builtins", "str")
                );

                fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
                    Self::parse_obj(&obj).map(Self)
                }
            }

            impl<'py> IntoPyObject<'py> for $wrapper {
                type Target = PyAny;
                type Output = Bound<'py, Self::Target>;
                type Error = PyErr;

                const OUTPUT_TYPE: pyo3::inspect::PyStaticExpr =
                    pyo3::type_hint_identifier!("capture_system_or_process_audio", $py_name);

                fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
                    Self::into_member(self.0, py)
                }
            }

            impl<'py> IntoPyObject<'py> for &$wrapper {
                type Target = PyAny;
                type Output = Bound<'py, Self::Target>;
                type Error = PyErr;

                const OUTPUT_TYPE: pyo3::inspect::PyStaticExpr =
                    pyo3::type_hint_identifier!("capture_system_or_process_audio", $py_name);

                fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
                    $wrapper::into_member(self.0, py)
                }
            }
        )+
    };
}

define_cspa_enums! {
    (target_kind, PyTargetKind, TargetKind, "TargetKind") {
        TargetKind::Process => ("PROCESS", "process"),
        TargetKind::Application => ("APPLICATION", "application"),
        TargetKind::Device => ("DEVICE", "device"),
        TargetKind::DefaultOutput => ("DEFAULT_OUTPUT", "default_output"),
        TargetKind::InputDevice => ("INPUT_DEVICE", "input_device"),
        TargetKind::DefaultInput => ("DEFAULT_INPUT", "default_input"),
    };
    (backend_id, PyBackendId, BackendId, "BackendId") {
        BackendId::Wasapi => ("WASAPI", "wasapi"),
        BackendId::PipeWire => ("PIPEWIRE", "pipewire"),
        BackendId::PulseAudio => ("PULSEAUDIO", "pulseaudio"),
        BackendId::CoreAudio => ("COREAUDIO", "coreaudio"),
        BackendId::ScreenCaptureKit => ("SCREENCAPTUREKIT", "screencapturekit"),
    };
    (platform_os, PyPlatformOs, PlatformOs, "PlatformOs") {
        PlatformOs::Windows => ("WINDOWS", "windows"),
        PlatformOs::Linux => ("LINUX", "linux"),
        PlatformOs::MacOs => ("MACOS", "macos"),
    };
    (process_scope, PyProcessScope, ProcessScope, "ProcessScope") {
        ProcessScope::Tree => ("TREE", "tree"),
        ProcessScope::Exact => ("EXACT", "exact"),
    };
    (root_exit_policy, PyRootExitPolicy, RootExitPolicyTag, "RootExitPolicy") {
        RootExitPolicyTag::Stop => ("STOP", "stop"),
        RootExitPolicyTag::Wait => ("WAIT", "wait"),
    };
    (default_device_mode, PyDefaultDeviceMode, DefaultDeviceMode, "DefaultDeviceMode") {
        DefaultDeviceMode::Follow => ("FOLLOW", "follow"),
        DefaultDeviceMode::Pin => ("PIN", "pin"),
    };
    (process_tree_strategy, PyProcessTreeStrategy, ProcessTreeStrategy, "ProcessTreeStrategy") {
        ProcessTreeStrategy::NativeKernelCapture => ("NATIVE_KERNEL_CAPTURE", "native_kernel_capture"),
        ProcessTreeStrategy::NativeGraphMix => ("NATIVE_GRAPH_MIX", "native_graph_mix"),
        ProcessTreeStrategy::SoftwareStreamMix => ("SOFTWARE_STREAM_MIX", "software_stream_mix"),
    };
    (combine_mode, PyCombineMode, CombineMode, "CombineMode") {
        CombineMode::Auto => ("AUTO", "auto"),
        CombineMode::NativeOnly => ("NATIVE_ONLY", "native_only"),
        CombineMode::CspaMixer => ("CSPA_MIXER", "cspa_mixer"),
    };
    (combine_resolution_kind, PyCombineResolutionKind, CombineResolutionTag, "CombineResolutionKind") {
        CombineResolutionTag::PassThrough => ("PASS_THROUGH", "pass_through"),
        CombineResolutionTag::SharedNativeGraphMix => ("SHARED_NATIVE_GRAPH_MIX", "shared_native_graph_mix"),
        CombineResolutionTag::CspaMixer => ("CSPA_MIXER", "cspa_mixer"),
    };
    (target_acquisition_kind, PyTargetAcquisitionKind, TargetAcquisitionTag, "TargetAcquisitionKind") {
        TargetAcquisitionTag::DirectEndpoint => ("DIRECT_ENDPOINT", "direct_endpoint"),
        TargetAcquisitionTag::ProcessTree => ("PROCESS_TREE", "process_tree"),
    };
    (input_conversion_policy, PyInputConversionPolicy, InputConversionPolicy, "InputConversionPolicy") {
        InputConversionPolicy::AutoWhenMixing => ("AUTO_WHEN_MIXING", "auto_when_mixing"),
        InputConversionPolicy::StrictNoResample => ("STRICT_NO_RESAMPLE", "strict_no_resample"),
    };
    (overlap_semantics, PyOverlapSemantics, OverlapCombinationSemantics, "OverlapSemantics") {
        OverlapCombinationSemantics::SingleTarget => ("SINGLE_TARGET", "single_target"),
        OverlapCombinationSemantics::SoftwareSummation => ("SOFTWARE_SUMMATION", "software_summation"),
        OverlapCombinationSemantics::NativeSetUnion => ("NATIVE_SET_UNION", "native_set_union"),
    };
    (plan_error_code, PyPlanErrorCode, PlanErrorTag, "PlanErrorCode") {
        PlanErrorTag::InvalidTarget => ("INVALID_TARGET", "invalid_target"),
        PlanErrorTag::UnsupportedScope => ("UNSUPPORTED_SCOPE", "unsupported_scope"),
        PlanErrorTag::ReversedStrategyRange => ("REVERSED_STRATEGY_RANGE", "reversed_strategy_range"),
        PlanErrorTag::InvalidBackend => ("INVALID_BACKEND", "invalid_backend"),
        PlanErrorTag::BackendUnavailable => ("BACKEND_UNAVAILABLE", "backend_unavailable"),
        PlanErrorTag::PermissionDenied => ("PERMISSION_DENIED", "permission_denied"),
        PlanErrorTag::NoSuchProcess => ("NO_SUCH_PROCESS", "no_such_process"),
        PlanErrorTag::PidReused => ("PID_REUSED", "pid_reused"),
        PlanErrorTag::PreciseTimingUnqualified => ("PRECISE_TIMING_UNQUALIFIED", "precise_timing_unqualified"),
        PlanErrorTag::UnsupportedCapturePlan => ("UNSUPPORTED_CAPTURE_PLAN", "unsupported_capture_plan"),
        PlanErrorTag::Capacity => ("CAPACITY", "capacity"),
    };
    (discovery_dedupe, PyDiscoveryDedupe, DiscoveryDedupe, "DiscoveryDedupe") {
        DiscoveryDedupe::ByLogicalTarget => ("BY_LOGICAL_TARGET", "by_logical_target"),
        DiscoveryDedupe::PerBackend => ("PER_BACKEND", "per_backend"),
    };
    (discovery_order, PyDiscoveryOrder, DiscoveryOrder, "DiscoveryOrder") {
        DiscoveryOrder::KindThenBackend => ("KIND_THEN_BACKEND", "kind_then_backend"),
        DiscoveryOrder::BackendThenKind => ("BACKEND_THEN_KIND", "backend_then_kind"),
    };
    (discovery_event_kind, PyDiscoveryEventKind, DiscoveryEventTag, "DiscoveryEventKind") {
        DiscoveryEventTag::Added => ("ADDED", "added"),
        DiscoveryEventTag::Removed => ("REMOVED", "removed"),
        DiscoveryEventTag::StateChanged => ("STATE_CHANGED", "state_changed"),
        DiscoveryEventTag::DefaultDeviceChanged => ("DEFAULT_DEVICE_CHANGED", "default_device_changed"),
        DiscoveryEventTag::DefaultInputChanged => ("DEFAULT_INPUT_CHANGED", "default_input_changed"),
        DiscoveryEventTag::WatcherFailed => ("WATCHER_FAILED", "watcher_failed"),
        DiscoveryEventTag::VisibilityChanged => ("VISIBILITY_CHANGED", "visibility_changed"),
        DiscoveryEventTag::ResnapshotRequired => ("RESNAPSHOT_REQUIRED", "resnapshot_required"),
    };
    (visibility, PyVisibility, Visibility, "Visibility") {
        Visibility::Complete => ("COMPLETE", "complete"),
        Visibility::Limited => ("LIMITED", "limited"),
    };
    (sample_type, PySampleType, SampleType, "SampleType") {
        SampleType::I16 => ("I16", "i16"),
        SampleType::I24 => ("I24", "i24"),
        SampleType::I32 => ("I32", "i32"),
        SampleType::F32 => ("F32", "f32"),
    };
    (byte_order, PyByteOrder, ByteOrder, "ByteOrder") {
        ByteOrder::Little => ("LITTLE", "little"),
        ByteOrder::Big => ("BIG", "big"),
    };
    (channel, PyChannel, Channel, "Channel") {
        Channel::Mono => ("MONO", "mono"),
        Channel::FrontLeft => ("FRONT_LEFT", "front_left"),
        Channel::FrontRight => ("FRONT_RIGHT", "front_right"),
        Channel::Center => ("CENTER", "center"),
        Channel::LowFrequency => ("LOW_FREQUENCY", "low_frequency"),
        Channel::RearLeft => ("REAR_LEFT", "rear_left"),
        Channel::RearRight => ("REAR_RIGHT", "rear_right"),
    };
    (dither_mode, PyDitherMode, DitherMode, "DitherMode") {
        DitherMode::Tpdf => ("TPDF", "tpdf"),
        DitherMode::None => ("NONE", "none"),
    };
    (on_format_change, PyOnFormatChange, OnFormatChange, "OnFormatChange") {
        OnFormatChange::Adapt => ("ADAPT", "adapt"),
        OnFormatChange::Fail => ("FAIL", "fail"),
    };
    (on_input_failure, PyOnInputFailure, OnInputFailure, "OnInputFailure") {
        OnInputFailure::Degrade => ("DEGRADE", "degrade"),
        OnInputFailure::Fail => ("FAIL", "fail"),
    };
    (overflow, PyOverflow, Overflow, "Overflow") {
        Overflow::DropOldest => ("DROP_OLDEST", "drop_oldest"),
        Overflow::DropNewest => ("DROP_NEWEST", "drop_newest"),
        Overflow::Fail => ("FAIL", "fail"),
    };
    (boundary_kind, PyBoundaryKind, BoundaryKind, "BoundaryKind") {
        BoundaryKind::SessionChanged => ("SESSION_CHANGED", "session_changed"),
        BoundaryKind::ConfigurationChanged => ("CONFIGURATION_CHANGED", "configuration_changed"),
        BoundaryKind::DeviceChanged => ("DEVICE_CHANGED", "device_changed"),
        BoundaryKind::FormatChanged => ("FORMAT_CHANGED", "format_changed"),
        BoundaryKind::Gap => ("GAP", "gap"),
    };
    (partial_reason, PyPartialReason, PartialReasonTag, "PartialReason") {
        PartialReasonTag::Complete => ("COMPLETE", "complete"),
        PartialReasonTag::WouldBlock => ("WOULD_BLOCK", "would_block"),
        PartialReasonTag::Boundary => ("BOUNDARY", "boundary"),
        PartialReasonTag::Cancelled => ("CANCELLED", "cancelled"),
        PartialReasonTag::Timeout => ("TIMEOUT", "timeout"),
        PartialReasonTag::Eof => ("EOF", "eof"),
        PartialReasonTag::Failure => ("FAILURE", "failure"),
        PartialReasonTag::Ready => ("READY", "ready"),
    };
    (failure_kind, PyFailureKind, Failure, "FailureKind") {
        Failure::QueueOverflow => ("QUEUE_OVERFLOW", "queue_overflow"),
        Failure::PoolExhaustion => ("POOL_EXHAUSTION", "pool_exhaustion"),
        Failure::PacketTooLarge => ("PACKET_TOO_LARGE", "packet_too_large"),
        Failure::InvalidPacket => ("INVALID_PACKET", "invalid_packet"),
        Failure::TicketExhausted => ("TICKET_EXHAUSTED", "ticket_exhausted"),
        Failure::ControlExhausted => ("CONTROL_EXHAUSTED", "control_exhausted"),
        Failure::Backend => ("BACKEND", "backend"),
        Failure::Callback => ("CALLBACK", "callback"),
        Failure::CallbackPanic => ("CALLBACK_PANIC", "callback_panic"),
    };
    (participation_state, PyParticipationState, ParticipationStateTag, "ParticipationState") {
        ParticipationStateTag::Starting => ("STARTING", "starting"),
        ParticipationStateTag::Active => ("ACTIVE", "active"),
        ParticipationStateTag::Idle => ("IDLE", "idle"),
        ParticipationStateTag::WaitingRelaunch => ("WAITING_RELAUNCH", "waiting_relaunch"),
        ParticipationStateTag::Late => ("LATE", "late"),
        ParticipationStateTag::Exited => ("EXITED", "exited"),
        ParticipationStateTag::Failed => ("FAILED", "failed"),
    };
    (stop_mode, PyStopMode, Stop, "StopMode") {
        Stop::Drain => ("DRAIN", "drain"),
        Stop::Abort => ("ABORT", "abort"),
    };
    (timing_requirement, PyTimingRequirement, TimingRequirement, "TimingRequirement") {
        TimingRequirement::Raw => ("RAW", "raw"),
        TimingRequirement::Precise => ("PRECISE", "precise"),
    };
    (permission_kind, PyPermissionKind, PermissionKind, "PermissionKind") {
        PermissionKind::Microphone => ("MICROPHONE", "microphone"),
        PermissionKind::SystemAudio => ("SYSTEM_AUDIO", "system_audio"),
        PermissionKind::ScreenRecording => ("SCREEN_RECORDING", "screen_recording"),
    };
    (permission_state, PyPermissionState, PermissionState, "PermissionState") {
        PermissionState::Granted => ("GRANTED", "granted"),
        PermissionState::NotDetermined => ("NOT_DETERMINED", "not_determined"),
        PermissionState::Denied => ("DENIED", "denied"),
        PermissionState::Restricted => ("RESTRICTED", "restricted"),
        PermissionState::Unknown => ("UNKNOWN", "unknown"),
    };
    (permission_policy, PyPermissionPolicy, PermissionPolicy, "PermissionPolicy") {
        PermissionPolicy::AutoRequest => ("AUTO_REQUEST", "auto_request"),
        PermissionPolicy::CheckOnly => ("CHECK_ONLY", "check_only"),
    };
}
