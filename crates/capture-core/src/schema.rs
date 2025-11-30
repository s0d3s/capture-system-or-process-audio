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

