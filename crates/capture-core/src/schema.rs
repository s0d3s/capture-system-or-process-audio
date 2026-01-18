use crate::CapacityError;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleType {
    I16,
    I24,
    I32,
    F32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DitherMode {
    #[default]
    Tpdf,
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OnInputFailure {
    #[default]
    Degrade,
    Fail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipationState {
    Starting,
    Active,
    Idle,
    WaitingRelaunch,
    Late,
    Exited,
    Failed(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipEntry {
    pub input_index: usize,
    pub target_id: Arc<str>,
    pub state: ParticipationState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipDescriptor {
    pub version: u64,
    pub entries: Arc<[MembershipEntry]>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MixedBlockMetadata {
    pub membership_version: u64,
    pub contributing_mask: u64,
    pub late_inputs: u64,
    pub failed_inputs: u64,
    pub nonfinite_inputs: u64,
    pub synthesized_silence: bool,
    pub clipped_samples: u64,
    pub nonfinite_samples: u64,
    pub drift_slips: u64,
    pub skew_estimate_ns: Option<u64>,
    pub skew_uncertainty_ns: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputConverterConfig {
    pub target_format: Format,
    pub dither: DitherMode,
    pub seed: u64,
}

impl OutputConverterConfig {
    pub fn new(target_format: Format, dither: DitherMode) -> Self {
        Self {
            target_format,
            dither,
            seed: 0xC59A_2026_0003_0001,
        }
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AggregateManagerBudgets {
    pub max_targets: usize,
    pub max_active_streams: usize,
    pub max_pending_opens: usize,
    pub max_native_graph_links: usize,
    pub max_discovery_queue: usize,
    pub max_total_pcm_memory: usize,
    pub cleanup_timeout_ns: u64,
}

impl Default for AggregateManagerBudgets {
    fn default() -> Self {
        Self {
            max_targets: 16,
            max_active_streams: 16,
            max_pending_opens: 4,
            max_native_graph_links: 64,
            max_discovery_queue: 256,
            max_total_pcm_memory: 16 * 1024 * 1024,
            cleanup_timeout_ns: 2_000_000_000,
        }
    }
}

impl AggregateManagerBudgets {
    pub fn validate(&self) -> Result<(), PlanError> {
        if !(1..=64).contains(&self.max_targets)
            || !(1..=64).contains(&self.max_active_streams)
            || !(1..=16).contains(&self.max_pending_opens)
            || !(1..=256).contains(&self.max_native_graph_links)
            || !(32..=4096).contains(&self.max_discovery_queue)
            || !(1024 * 1024..=128 * 1024 * 1024).contains(&self.max_total_pcm_memory)
            || !(100_000_000..=30_000_000_000).contains(&self.cleanup_timeout_ns)
        {
            return Err(PlanError::Capacity(CapacityError::ExceedsByteCeiling));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixerConfig {
    pub alignment_window_ns: u64,
    pub max_input_buffer_ns: u64,
    pub max_input_slots: usize,
    pub output_queue_slots: usize,
    pub clamp_f32: bool,
    pub drift_threshold_ns: u64,
    pub drift_hysteresis_ns: u64,
    pub drift_hold_ns: u64,
    pub drift_crossfade_frames: usize,
    pub silence_on_idle: bool,
    pub on_input_failure: OnInputFailure,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            alignment_window_ns: 20_000_000,
            max_input_buffer_ns: 100_000_000,
            max_input_slots: 8,
            output_queue_slots: 8,
            clamp_f32: false,
            drift_threshold_ns: 5_000_000,
            drift_hysteresis_ns: 2_000_000,
            drift_hold_ns: 500_000_000,
            drift_crossfade_frames: 64,
            silence_on_idle: false,
            on_input_failure: OnInputFailure::Degrade,
        }
    }
}

impl MixerConfig {
    pub fn validate(&self) -> Result<(), PlanError> {
        if !(5_000_000..=250_000_000).contains(&self.alignment_window_ns)
            || !(20_000_000..=1_000_000_000).contains(&self.max_input_buffer_ns)
            || self.max_input_slots == 0
            || self.output_queue_slots == 0
            || !(1_000_000..=50_000_000).contains(&self.drift_threshold_ns)
            || self.drift_hysteresis_ns >= self.drift_threshold_ns
            || self.drift_crossfade_frames > 256
        {
            return Err(PlanError::InvalidTarget);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteOrder {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Channel {
    Mono,
    FrontLeft,
    FrontRight,
    Center,
    LowFrequency,
    RearLeft,
    RearRight,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Layout {
    Unknown,
    Known(Arc<[Channel]>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Format {
    sample: SampleType,
    order: ByteOrder,
    rate: u32,
    channels: u16,
    layout: Layout,
}

impl Format {
    pub fn new(
        sample: SampleType,
        order: ByteOrder,
        rate: u32,
        channels: u16,
        layout: Layout,
    ) -> Result<Self, Reason> {
        if rate == 0 || channels == 0 {
            return Err(Reason::InvalidFormat);
        }
        if let Layout::Known(labels) = &layout
            && (labels.len() != usize::from(channels)
                || labels
                    .iter()
                    .enumerate()
                    .any(|(index, label)| labels[..index].contains(label)))
        {
            return Err(Reason::InvalidFormat);
        }
        Ok(Self {
            sample,
            order,
            rate,
            channels,
            layout,
        })
    }
    pub fn sample(&self) -> SampleType {
        self.sample
    }
    pub fn order(&self) -> ByteOrder {
        self.order
    }
    pub fn rate(&self) -> u32 {
        self.rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn bytes_per_frame(&self) -> usize {
        usize::from(self.channels)
            * match self.sample {
                SampleType::I16 => 2,
                SampleType::I24 => 3,
                SampleType::I32 | SampleType::F32 => 4,
            }
    }
    pub fn byte_count(&self, frames: usize) -> Result<usize, Reason> {
        frames
            .checked_mul(self.bytes_per_frame())
            .ok_or(Reason::Capacity(CapacityError::Overflow))
    }
    pub fn frames_in(&self, bytes: usize) -> Result<usize, Reason> {
        if !bytes.is_multiple_of(self.bytes_per_frame()) {
            return Err(Reason::IncompleteFrame);
        }
        Ok(bytes / self.bytes_per_frame())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DefaultDeviceMode {
    #[default]
    Follow,
    Pin,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessScope {
    #[default]
    Tree,
    Exact,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OnRootExit {
    #[default]
    Stop,
    Wait {
        wait_timeout_ns: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TargetKind {
    DefaultOutput,
    DefaultInput,
    Application,
    Process,
    Device,
    InputDevice,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OnFormatChange {
    #[default]
    Adapt,
    Fail,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputConversionPolicy {
    #[default]
    AutoWhenMixing,
    StrictNoResample,
}

/// Per-target process-tree acquisition strategy for `Process` and `Application` targets.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProcessTreeStrategy {
    NativeKernelCapture = 1,
    NativeGraphMix = 2,
    SoftwareStreamMix = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessTreeRange {
    pub start_level: ProcessTreeStrategy,
    pub fallback_limit: Option<ProcessTreeStrategy>,
}

impl Default for ProcessTreeRange {
    fn default() -> Self {
        Self {
            start_level: ProcessTreeStrategy::NativeKernelCapture,
            fallback_limit: Some(ProcessTreeStrategy::SoftwareStreamMix),
        }
    }
}

impl ProcessTreeRange {
    pub fn validate(&self) -> Result<(), PlanError> {
        if let Some(limit) = self.fallback_limit
            && limit < self.start_level
        {
            return Err(PlanError::ReversedStrategyRange);
        }
        Ok(())
    }

    pub fn allows(&self, level: ProcessTreeStrategy) -> bool {
        match self.fallback_limit {
            None => level == self.start_level,
            Some(limit) => level >= self.start_level && level <= limit,
        }
    }

    pub fn ordered_levels(&self) -> Result<Vec<ProcessTreeStrategy>, PlanError> {
        self.validate()?;
        let end = self.fallback_limit.unwrap_or(self.start_level);
        Ok([
            ProcessTreeStrategy::NativeKernelCapture,
            ProcessTreeStrategy::NativeGraphMix,
            ProcessTreeStrategy::SoftwareStreamMix,
        ]
        .into_iter()
        .filter(|level| *level >= self.start_level && *level <= end)
        .collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetAcquisition {
    DirectEndpoint,
    ProcessTree(ProcessTreeStrategy),
}

/// Session-level target combination policy owned by `Manager`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CombineMode {
    #[default]
    Auto,
    NativeOnly,
    CspaMixer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CombineResolution {
    PassThrough(BackendId),
    SharedNativeGraphMix(BackendId),
    CspaMixer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendId {
    Wasapi,
    PipeWire,
    PulseAudio,
    CoreAudio,
    ScreenCaptureKit,
}

impl BackendId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wasapi => "wasapi",
            Self::PipeWire => "pipewire",
            Self::PulseAudio => "pulseaudio",
            Self::CoreAudio => "coreaudio",
            Self::ScreenCaptureKit => "screencapturekit",
        }
    }

    pub fn parse(name: &str) -> Result<Self, PlanError> {
        match name {
            "wasapi" => Ok(Self::Wasapi),
            "pipewire" => Ok(Self::PipeWire),
            "pulseaudio" => Ok(Self::PulseAudio),
            "coreaudio" => Ok(Self::CoreAudio),
            "screencapturekit" => Ok(Self::ScreenCaptureKit),
            _ => Err(PlanError::InvalidBackend),
        }
    }

    pub fn is_valid_for_os(self, os: PlatformOs) -> bool {
        matches!(
            (os, self),
            (PlatformOs::Windows, Self::Wasapi)
                | (PlatformOs::Linux, Self::PipeWire | Self::PulseAudio)
                | (PlatformOs::MacOs, Self::CoreAudio | Self::ScreenCaptureKit)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformOs {
    Windows,
    Linux,
    MacOs,
}

impl PlatformOs {
    #[inline]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// OS recording-consent domain.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PermissionKind {
    Microphone,
    SystemAudio,
    ScreenRecording,
}

impl PermissionKind {
    pub const ALL: [Self; 3] = [Self::Microphone, Self::SystemAudio, Self::ScreenRecording];

    #[inline]
    pub const fn index(self) -> usize {
        match self {
            Self::Microphone => 0,
            Self::SystemAudio => 1,
            Self::ScreenRecording => 2,
        }
    }

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::SystemAudio => "system_audio",
            Self::ScreenRecording => "screen_recording",
        }
    }
}

/// Recording permission status. Unprobed OS domains are never reported as `Granted`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionState {
    Granted,
    NotDetermined,
    Denied,
    Restricted,
    Unknown,
}

impl PermissionState {
    #[inline]
    pub const fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }

    #[inline]
    pub const fn is_denied_or_restricted(self) -> bool {
        matches!(self, Self::Denied | Self::Restricted)
    }

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::NotDetermined => "not_determined",
            Self::Denied => "denied",
            Self::Restricted => "restricted",
            Self::Unknown => "unknown",
        }
    }
}

/// Recording permission prompt policy during `open()`.
/// Defaults to `AutoRequest`; `CheckOnly` disables prompting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PermissionPolicy {
    #[default]
    AutoRequest,
    CheckOnly,
}

impl PermissionPolicy {
    #[inline]
    pub const fn allows_prompt(self) -> bool {
        matches!(self, Self::AutoRequest)
    }

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AutoRequest => "auto_request",
            Self::CheckOnly => "check_only",
        }
    }
}

/// Per-domain recording permission status entry inside `PermissionReport`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PermissionEntry {
    pub kind: PermissionKind,
    pub state: PermissionState,
    pub required: bool,
    pub can_prompt: bool,
    pub requires_relaunch: bool,
}

impl PermissionEntry {
    #[inline]
    pub const fn is_granted(self) -> bool {
        self.state.is_granted()
    }
}

/// Zero-heap-allocation permission report with entries ordered as
/// `[Microphone, SystemAudio, ScreenRecording]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PermissionReport {
    pub os: PlatformOs,
    pub entries: [PermissionEntry; 3],
}

impl PermissionReport {
    pub const fn new(
        os: PlatformOs,
        microphone: PermissionEntry,
        system_audio: PermissionEntry,
        screen_recording: PermissionEntry,
    ) -> Self {
        Self {
            os,
            entries: [microphone, system_audio, screen_recording],
        }
    }

    #[inline]
    pub const fn entries(&self) -> &[PermissionEntry; 3] {
        &self.entries
    }

    #[inline]
    pub const fn microphone(&self) -> PermissionEntry {
        self.entries[0]
    }

    #[inline]
    pub const fn system_audio(&self) -> PermissionEntry {
        self.entries[1]
    }

    #[inline]
    pub const fn screen_recording(&self) -> PermissionEntry {
        self.entries[2]
    }

    #[inline]
    pub const fn entry(&self, kind: PermissionKind) -> PermissionEntry {
        self.entries[kind.index()]
    }

    #[inline]
    pub const fn state_for(&self, kind: PermissionKind) -> PermissionState {
        self.entries[kind.index()].state
    }

    /// Returns `true` iff every entry marked `required == true` is explicitly `PermissionState::Granted`.
    pub fn is_granted(&self) -> bool {
        self.entries
            .iter()
            .filter(|e| e.required)
            .all(|e| e.state == PermissionState::Granted)
    }

    /// Returns `true` if any `required == true` entry is `Denied` or `Restricted`.
    pub fn has_denial(&self) -> bool {
        self.entries
            .iter()
            .filter(|e| e.required)
            .any(|e| e.state.is_denied_or_restricted())
    }

    /// Returns `true` if any `required == true` entry requires restarting the application for consent to take effect.
    pub fn requires_relaunch(&self) -> bool {
        self.entries
            .iter()
            .filter(|e| e.required)
            .any(|e| e.requires_relaunch)
    }

    pub fn required_kinds(&self) -> Vec<PermissionKind> {
        self.entries
            .iter()
            .filter(|e| e.required)
            .map(|e| e.kind)
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Backends {
    pub windows: Vec<BackendId>,
    pub linux: Vec<BackendId>,
    pub macos: Vec<BackendId>,
}

impl Default for Backends {
    fn default() -> Self {
        Self {
            windows: vec![BackendId::Wasapi],
            linux: vec![BackendId::PipeWire, BackendId::PulseAudio],
            macos: vec![BackendId::CoreAudio, BackendId::ScreenCaptureKit],
        }
    }
}

impl Backends {
    pub fn new(
        windows: Option<Vec<BackendId>>,
        linux: Option<Vec<BackendId>>,
        macos: Option<Vec<BackendId>>,
    ) -> Result<Self, PlanError> {
        let defaults = Self::default();
        let backends = Self {
            windows: windows.unwrap_or(defaults.windows),
            linux: linux.unwrap_or(defaults.linux),
            macos: macos.unwrap_or(defaults.macos),
        };
        backends.validate()?;
        Ok(backends)
    }

    pub fn default_primary_for_os(os: PlatformOs) -> BackendId {
        match os {
            PlatformOs::Windows => BackendId::Wasapi,
            PlatformOs::Linux => BackendId::PipeWire,
            PlatformOs::MacOs => BackendId::CoreAudio,
        }
    }

    pub fn primary_for_os(&self, os: PlatformOs) -> BackendId {
        self.for_os(os)
            .first()
            .copied()
            .unwrap_or_else(|| Self::default_primary_for_os(os))
    }

    pub fn validate(&self) -> Result<(), PlanError> {
        if self.windows.is_empty()
            || self.linux.is_empty()
            || self.macos.is_empty()
            || self.windows.iter().any(|id| *id != BackendId::Wasapi)
            || self
                .linux
                .iter()
                .any(|id| !matches!(id, BackendId::PipeWire | BackendId::PulseAudio))
            || self
                .macos
                .iter()
                .any(|id| !matches!(id, BackendId::CoreAudio | BackendId::ScreenCaptureKit))
        {
            return Err(PlanError::InvalidBackend);
        }
        Ok(())
    }

    pub fn for_os(&self, os: PlatformOs) -> &[BackendId] {
        match os {
            PlatformOs::Windows => &self.windows,
            PlatformOs::Linux => &self.linux,
            PlatformOs::MacOs => &self.macos,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureTarget {
    pub(crate) kind: TargetKind,
    pub(crate) id: Arc<str>,
    pub(crate) pid: Option<u32>,
    pub(crate) process_start_time: Option<u64>,
    pub(crate) app_id: Option<Arc<str>>,
    pub(crate) device_id: Option<Arc<str>>,
    pub(crate) name: Option<Arc<str>>,
    pub(crate) executable_path: Option<Arc<str>>,
    pub(crate) active_audio: Option<bool>,
    pub(crate) scope: ProcessScope,
    pub(crate) on_root_exit: OnRootExit,
    pub(crate) default_device_mode: DefaultDeviceMode,
    pub(crate) backend: Option<BackendId>,
    pub(crate) available_backends: Arc<[BackendId]>,
    pub(crate) gain: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
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
    Capacity(CapacityError),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PlanError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticOptions {
    pub include_identity: bool,
}

pub fn extract_backend_id_prefix(id: &str) -> (Option<BackendId>, &str) {
    for (prefix, backend) in [
        ("wasapi:", BackendId::Wasapi),
        ("pipewire:", BackendId::PipeWire),
        ("pulseaudio:", BackendId::PulseAudio),
        ("coreaudio:", BackendId::CoreAudio),
        ("screencapturekit:", BackendId::ScreenCaptureKit),
    ] {
        if let Some(rest) = id.strip_prefix(prefix) {
            return (Some(backend), rest);
        }
    }
    (None, id)
}

pub fn strip_backend_id_prefix(id: &str) -> &str {
    extract_backend_id_prefix(id).1
}

impl CaptureTarget {
    #[inline]
    pub fn kind(&self) -> TargetKind {
        self.kind
    }

    #[inline]
    pub fn id(&self) -> &Arc<str> {
        &self.id
    }

    #[inline]
    pub fn logical_id(&self) -> &str {
        strip_backend_id_prefix(&self.id)
    }

    #[inline]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    #[inline]
    pub fn process_start_time(&self) -> Option<u64> {
        self.process_start_time
    }

    #[inline]
    pub fn app_id(&self) -> Option<&Arc<str>> {
        self.app_id.as_ref()
    }

    #[inline]
    pub fn device_id(&self) -> Option<&Arc<str>> {
        self.device_id.as_ref()
    }

    #[inline]
    pub fn clean_device_id(&self) -> Option<&str> {
        self.device_id.as_deref().map(|raw| {
            let stripped = strip_backend_id_prefix(raw);
            stripped
                .strip_prefix("device:")
                .or_else(|| stripped.strip_prefix("input_device:"))
                .unwrap_or(stripped)
        })
    }

    #[inline]
    pub fn name(&self) -> Option<&Arc<str>> {
        self.name.as_ref()
    }

    #[inline]
    pub fn executable_path(&self) -> Option<&Arc<str>> {
        self.executable_path.as_ref()
    }

    #[inline]
    pub fn active_audio(&self) -> Option<bool> {
        self.active_audio
    }

    #[inline]
    pub fn scope(&self) -> ProcessScope {
        self.scope
    }

    #[inline]
    pub fn on_root_exit(&self) -> OnRootExit {
        self.on_root_exit
    }

    #[inline]
    pub fn default_device_mode(&self) -> DefaultDeviceMode {
        self.default_device_mode
    }

    #[inline]
    pub fn backend(&self) -> Option<BackendId> {
        self.backend
    }

    #[inline]
    pub fn available_backends(&self) -> &Arc<[BackendId]> {
        &self.available_backends
    }

    #[inline]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn diagnostic_summary(&self, options: DiagnosticOptions) -> String {
        if options.include_identity {
            format!(
                "kind={:?} id={} backend={:?} scope={:?} on_root_exit={:?} default_device_mode={:?} gain={}",
                self.kind,
                self.id,
                self.backend,
                self.scope,
                self.on_root_exit,
                self.default_device_mode,
                self.gain
            )
        } else {
            format!(
                "kind={:?} id=<redacted> backend={:?} scope={:?} on_root_exit={:?} default_device_mode={:?} gain={}",
                self.kind,
                self.backend,
                self.scope,
                self.on_root_exit,
                self.default_device_mode,
                self.gain
            )
        }
    }

    pub fn process(
        identity: ProcessIdentity,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<Self, PlanError> {
        if identity.pid == 0 {
            return Err(PlanError::InvalidTarget);
        }
        if matches!(on_root_exit, OnRootExit::Wait { wait_timeout_ns: 0 }) {
            return Err(PlanError::InvalidTarget);
        }
        Ok(Self {
            kind: TargetKind::Process,
            id: format!("process:{}:{}", identity.pid, identity.generation).into(),
            pid: Some(identity.pid),
            process_start_time: Some(identity.generation),
            app_id: None,
            device_id: None,
            name: None,
            executable_path: None,
            active_audio: None,
            scope,
            on_root_exit,
            default_device_mode: DefaultDeviceMode::Follow,
            backend: None,
            available_backends: Arc::from([]),
            gain: 1.0,
        })
    }

    pub fn application(
        identity: ProcessIdentity,
        app_id: Option<Arc<str>>,
        name: Option<Arc<str>>,
        executable_path: Option<Arc<str>>,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<Self, PlanError> {
        let mut target = Self::process(identity, scope, on_root_exit)?;
        target.kind = TargetKind::Application;
        target.id = format!("application:{}:{}", identity.pid, identity.generation).into();
        target.app_id = app_id;
        target.name = name;
        target.executable_path = executable_path;
        Ok(target)
    }

    pub fn device(device_id: impl Into<Arc<str>>) -> Result<Self, PlanError> {
        let raw_id: Arc<str> = device_id.into();
        let clean = strip_backend_id_prefix(&raw_id);
        let clean = clean.strip_prefix("device:").unwrap_or(clean);
        if clean.is_empty() || clean.contains('\0') {
            return Err(PlanError::InvalidTarget);
        }
        let clean_arc: Arc<str> = clean.into();
        Ok(Self {
            kind: TargetKind::Device,
            id: format!("device:{clean_arc}").into(),
            pid: None,
            process_start_time: None,
            app_id: None,
            device_id: Some(clean_arc),
            name: None,
            executable_path: None,
            active_audio: None,
            scope: ProcessScope::Tree,
            on_root_exit: OnRootExit::Stop,
            default_device_mode: DefaultDeviceMode::Pin,
            backend: None,
            available_backends: Arc::from([]),
            gain: 1.0,
        })
    }

    pub fn input_device(device_id: impl Into<Arc<str>>) -> Result<Self, PlanError> {
        let raw_id: Arc<str> = device_id.into();
        let clean = strip_backend_id_prefix(&raw_id);
        let clean = clean.strip_prefix("input_device:").unwrap_or(clean);
        if clean.is_empty() || clean.contains('\0') {
            return Err(PlanError::InvalidTarget);
        }
        let clean_arc: Arc<str> = clean.into();
        Ok(Self {
            kind: TargetKind::InputDevice,
            id: format!("input_device:{clean_arc}").into(),
            pid: None,
            process_start_time: None,
            app_id: None,
            device_id: Some(clean_arc),
            name: None,
            executable_path: None,
            active_audio: None,
            scope: ProcessScope::Tree,
            on_root_exit: OnRootExit::Stop,
            default_device_mode: DefaultDeviceMode::Pin,
            backend: None,
            available_backends: Arc::from([]),
            gain: 1.0,
        })
    }

    pub fn default_output(default_device_mode: DefaultDeviceMode) -> Self {
        Self {
            kind: TargetKind::DefaultOutput,
            id: match default_device_mode {
                DefaultDeviceMode::Follow => "default_output:follow".into(),
                DefaultDeviceMode::Pin => "default_output:pin".into(),
            },
            pid: None,
            process_start_time: None,
            app_id: None,
            device_id: None,
            name: None,
            executable_path: None,
            active_audio: None,
            scope: ProcessScope::Tree,
            on_root_exit: OnRootExit::Stop,
            default_device_mode,
            backend: None,
            available_backends: Arc::from([]),
            gain: 1.0,
        }
    }

    pub fn microphone(default_device_mode: DefaultDeviceMode) -> Self {
        Self {
            kind: TargetKind::DefaultInput,
            id: match default_device_mode {
                DefaultDeviceMode::Follow => "default_input:follow".into(),
                DefaultDeviceMode::Pin => "default_input:pin".into(),
            },
            pid: None,
            process_start_time: None,
            app_id: None,
            device_id: None,
            name: None,
            executable_path: None,
            active_audio: None,
            scope: ProcessScope::Tree,
            on_root_exit: OnRootExit::Stop,
            default_device_mode,
            backend: None,
            available_backends: Arc::from([]),
            gain: 1.0,
        }
    }

    pub fn default_input(default_device_mode: DefaultDeviceMode) -> Self {
        Self::microphone(default_device_mode)
    }

    pub fn with_backend(self, backend: BackendId) -> Self {
        self.with_optional_backend(Some(backend))
    }

    pub fn with_optional_backend(mut self, backend: Option<BackendId>) -> Self {
        let base_id = strip_backend_id_prefix(&self.id).to_owned();
        self.backend = backend;
        self.id = match backend {
            Some(b) => format!("{}:{base_id}", b.as_str()).into(),
            None => base_id.into(),
        };
        if let Some(b) = backend
            && self.available_backends.is_empty()
        {
            self.available_backends = Arc::from([b]);
        }
        self
    }

    pub fn with_available_backends(mut self, backends: impl Into<Arc<[BackendId]>>) -> Self {
        self.available_backends = backends.into();
        self
    }

    pub fn with_scope(mut self, scope: ProcessScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_on_root_exit(mut self, on_root_exit: OnRootExit) -> Result<Self, PlanError> {
        if matches!(on_root_exit, OnRootExit::Wait { wait_timeout_ns: 0 }) {
            return Err(PlanError::InvalidTarget);
        }
        self.on_root_exit = on_root_exit;
        Ok(self)
    }

    pub fn with_default_device_mode(mut self, mode: DefaultDeviceMode) -> Self {
        self.default_device_mode = mode;
        let base_id = match self.kind {
            TargetKind::DefaultOutput => Some(match mode {
                DefaultDeviceMode::Follow => "default_output:follow",
                DefaultDeviceMode::Pin => "default_output:pin",
            }),
            TargetKind::DefaultInput => Some(match mode {
                DefaultDeviceMode::Follow => "default_input:follow",
                DefaultDeviceMode::Pin => "default_input:pin",
            }),
            _ => None,
        };
        if let Some(base) = base_id {
            self.id = match self.backend {
                Some(b) => format!("{}:{base}", b.as_str()).into(),
                None => base.into(),
            };
        }
        self
    }

    pub fn with_name(mut self, name: Option<Arc<str>>) -> Self {
        self.name = name;
        self
    }

    pub fn with_active_audio(mut self, active_audio: Option<bool>) -> Self {
        self.active_audio = active_audio;
        self
    }

    pub fn with_process_identity(mut self, identity: ProcessIdentity) -> Result<Self, PlanError> {
        if identity.pid == 0 || !matches!(self.kind, TargetKind::Process | TargetKind::Application)
        {
            return Err(PlanError::InvalidTarget);
        }
        let kind_prefix = match self.kind {
            TargetKind::Process => "process",
            TargetKind::Application => "application",
            _ => unreachable!(),
        };
        let base = format!("{kind_prefix}:{}:{}", identity.pid, identity.generation);
        self.pid = Some(identity.pid);
        self.process_start_time = Some(identity.generation);
        self.id = match self.backend {
            Some(b) => format!("{}:{base}", b.as_str()).into(),
            None => base.into(),
        };
        Ok(self)
    }

    /// Resolve `DefaultOutput(Pin)` or `DefaultInput(Pin)` to a concrete `Device` / `InputDevice`
    /// while preserving `gain`, `backend`, and `available_backends` invariants.
    pub fn resolve_pinned_endpoint(
        &self,
        default_output_id: Option<&str>,
        default_input_id: Option<&str>,
    ) -> Self {
        if self.default_device_mode != DefaultDeviceMode::Pin {
            return self.clone();
        }
        let replacement = match self.kind {
            TargetKind::DefaultOutput => default_output_id.and_then(|id| Self::device(id).ok()),
            TargetKind::DefaultInput => default_input_id.and_then(|id| Self::input_device(id).ok()),
            _ => None,
        };
        let Some(mut resolved) = replacement else {
            return self.clone();
        };
        resolved.gain = self.gain;
        resolved.available_backends = self.available_backends.clone();
        resolved.with_optional_backend(self.backend)
    }

    pub fn with_metadata(
        mut self,
        app_id: Option<Arc<str>>,
        name: Option<Arc<str>>,
        executable_path: Option<Arc<str>>,
        active_audio: Option<bool>,
    ) -> Self {
        self.app_id = app_id;
        self.name = name;
        self.executable_path = executable_path;
        self.active_audio = active_audio;
        self
    }

    pub fn with_gain(mut self, gain: f32) -> Result<Self, PlanError> {
        if !gain.is_finite() || gain < 0.0 {
            return Err(PlanError::InvalidTarget);
        }
        self.gain = gain;
        Ok(self)
    }

    pub fn process_identity(&self) -> Option<ProcessIdentity> {
        Some(ProcessIdentity {
            pid: self.pid?,
            generation: self.process_start_time?,
        })
    }

    pub fn lower_to_process(&self) -> Result<Self, PlanError> {
        match self.kind {
            TargetKind::Process => {
                self.validate()?;
                Ok(self.clone())
            }
            TargetKind::Application => {
                self.validate()?;
                let identity = self.process_identity().ok_or(PlanError::InvalidTarget)?;
                let base_id = format!("process:{}:{}", identity.pid, identity.generation);
                let id = match self.backend {
                    Some(b) => format!("{}:{base_id}", b.as_str()).into(),
                    None => base_id.into(),
                };
                Ok(Self {
                    kind: TargetKind::Process,
                    id,
                    ..self.clone()
                })
            }
            _ => Err(PlanError::InvalidTarget),
        }
    }

    pub fn match_identity_key(&self) -> Option<u64> {
        let text = self
            .app_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.executable_path
                    .as_deref()
                    .filter(|value| !value.is_empty())
            })?;
        Some(stable_match_key(text))
    }

    pub fn validate(&self) -> Result<(), PlanError> {
        if !self.gain.is_finite() || self.gain < 0.0 || self.id.is_empty() {
            return Err(PlanError::InvalidTarget);
        }
        let (id_backend, base_id) = extract_backend_id_prefix(&self.id);
        if id_backend != self.backend || base_id.is_empty() {
            return Err(PlanError::InvalidTarget);
        }
        if matches!(self.on_root_exit, OnRootExit::Wait { wait_timeout_ns: 0 }) {
            return Err(PlanError::InvalidTarget);
        }
        match self.kind {
            TargetKind::Process | TargetKind::Application => {
                if self.pid.is_none_or(|pid| pid == 0)
                    || self.process_start_time.is_none()
                    || self.device_id.is_some()
                {
                    return Err(PlanError::InvalidTarget);
                }
            }
            TargetKind::Device | TargetKind::InputDevice => {
                if self.pid.is_some()
                    || self.process_start_time.is_some()
                    || self
                        .device_id
                        .as_ref()
                        .is_none_or(|id| id.is_empty() || id.contains('\0'))
                {
                    return Err(PlanError::InvalidTarget);
                }
            }
            TargetKind::DefaultOutput | TargetKind::DefaultInput => {
                if self.pid.is_some()
                    || self.process_start_time.is_some()
                    || self.device_id.is_some()
                {
                    return Err(PlanError::InvalidTarget);
                }
            }
        }
        Ok(())
    }
}

pub fn stable_match_key(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    if hash == 0 { 1 } else { hash }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provenance {
    Native,
    Derived,
    HostArrival,
    Synthetic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub ticks: u64,
    pub ticks_per_second: u64,
    pub clock_domain: u64,
    pub provenance: Provenance,
    pub uncertainty_ns: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Loss {
    pub frames: Option<u64>,
    pub rate_hz: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub sequence: u64,
    pub segment: u64,
    pub generation: u64,
    pub frames: usize,
    pub sample_position: Option<u64>,
    pub timestamp: Option<Stamp>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    Ready,
    Unknown,
    PermissionRequired,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reason {
    AmbiguousApplication,
    StaleProcess,
    InvalidFormat,
    UnsupportedFormat,
    UnsupportedChannelMapping,
    FormatChanged,
    IncompleteFrame,
    Capacity(CapacityError),
    NoCandidate,
    Unknown,
    PermissionRequired,
    Unavailable,
    Unsupported,
    SelectedOpenFailed,
}
