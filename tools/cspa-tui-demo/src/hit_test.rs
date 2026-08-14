use crate::dsp::VisMode;
use capture_system_or_process_audio::PermissionKind;
use ratatui::layout::{Position, Rect};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalTab {
    Applications,
    Processes,
    OutputDevices,
    Microphones,
}

impl ModalTab {
    pub fn next(self) -> Self {
        match self {
            Self::Applications => Self::Processes,
            Self::Processes => Self::OutputDevices,
            Self::OutputDevices => Self::Microphones,
            Self::Microphones => Self::Applications,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Applications => "Apps (Tree)",
            Self::Processes => "Processes",
            Self::OutputDevices => "Output Devs",
            Self::Microphones => "Microphones",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClickAction {
    OpenAddSourceModal,
    ToggleStagedSource(usize),
    SelectStagedSource(usize),
    CycleStagedBackend(usize),
    CycleStagedGain(usize),
    CycleStagedScope(usize),
    CycleStagedExitPolicy(usize),
    CycleStagedDeviceMode(usize),
    DuplicateStagedSource(usize),
    RemoveStagedSource(usize),
    EnableAllStaged,
    DisableAllStaged,
    CycleCombineMode,
    CycleTreeStrategy,
    ToggleInputConversion,
    CycleSampleRate,
    CycleChannels,
    CycleSampleFormat,
    CycleDither,
    ToggleFormatConverter,
    ToggleSilenceOnIdle,
    CycleOverflowPolicy,
    ToggleSyntheticSignal,
    ToggleLiveMonitor,
    SelectVisMode(VisMode),
    ToggleRecordStop,
    SelectTake(usize),
    PlayPauseSelectedTake,
    StopPlayback,
    SeekPlaybackBar { bar_x: u16, bar_width: u16 },
    SaveSelectedTakeWav,
    DeleteSelectedTake,
    CloseModal,
    SwitchModalTab(ModalTab),
    FocusModalSearch,
    ClearModalSearch,
    ToggleModalAudioOnly,
    ToggleModalPlayingOnly,
    CycleModalBackendFilter,
    ToggleModalDedupe,
    ToggleModalOrder,
    RefreshModalDiscovery,
    ToggleModalCandidate(usize),
    ConfirmAddModalCandidates,
    OpenPermissionsModal,
    ClosePermissionsModal,
    TogglePermissionPolicy,
    ToggleOpenSettingsFallback,
    RefreshPermissions,
    RequestRequiredPermissions,
    RequestDomainPermission(PermissionKind),
    OpenDomainSettings(PermissionKind),
}

