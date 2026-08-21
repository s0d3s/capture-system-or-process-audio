use crate::backend::{
    InputTelemetryRow, PermissionJobKind, PermissionJobOutcome, PermissionRunner,
    available_os_backends, default_stereo_f32_format, domain_supports_open_settings,
    inspect_permissions_for_optional_request, inspect_request_plan, query_catalog,
    target_required_permission_kind,
};
use crate::capture_worker::{
    CaptureWorkerHandle, LiveCaptureTelemetry, MemoryTake, WorkerCommand, WorkerEvent,
};
use crate::dsp::{SpectroscopeAnalyzer, VisMode};
use crate::hit_test::{ClickAction, ModalTab};
use crate::playback::{PlaybackEngine, PlaybackStatus};
use crate::wav::save_wav_file;
use capture_system_or_process_audio::{
    BackendId, Backends, ByteOrder, CapturePlanReport, CaptureRequest, CaptureTarget, Channel,
    CombineMode, DefaultDeviceMode, Discovery, DiscoveryDedupe, DiscoveryEvent, DiscoveryEventKind,
    DiscoveryOrder, DiscoveryQuery, DiscoverySubscription, DitherMode, Format,
    InputConversionPolicy, Layout, MixedBlockMetadata, MixerConfig, OnRootExit,
    OutputConverterConfig, Overflow, ParticipationState, PermissionEntry, PermissionKind,
    PermissionPolicy, PermissionReport, PermissionState, PlatformOs, ProcessScope,
    ProcessTreeRange, ProcessTreeStrategy, SampleType, TargetKind, planner,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub fn backend_short_badge(backend: Option<BackendId>) -> &'static str {
    match backend {
        None => "AUTO",
        Some(BackendId::Wasapi) => "WASAPI",
        Some(BackendId::PipeWire) => "PW",
        Some(BackendId::PulseAudio) => "PA",
        Some(BackendId::CoreAudio) => "CA",
        Some(BackendId::ScreenCaptureKit) => "SCK",
    }
}

#[derive(Clone, Debug)]
pub struct StagedSource {
    pub enabled: bool,
    pub target: CaptureTarget,
}

impl StagedSource {
    pub fn display_name(&self) -> String {
        match self.target.kind() {
            TargetKind::DefaultOutput => "Default System Output".to_owned(),
            TargetKind::DefaultInput => "Default Microphone".to_owned(),
            TargetKind::Device => self
                .target
                .name()
                .map(Arc::as_ref)
                .or_else(|| self.target.clean_device_id())
                .unwrap_or("Output Device")
                .to_owned(),
            TargetKind::InputDevice => self
                .target
                .name()
                .map(Arc::as_ref)
                .or_else(|| self.target.clean_device_id())
                .unwrap_or("Input Microphone")
                .to_owned(),
            TargetKind::Application => {
                let name = self
                    .target
                    .name()
                    .map(Arc::as_ref)
                    .or_else(|| self.target.app_id().map(Arc::as_ref))
                    .unwrap_or("Application");
                if let Some(pid) = self.target.pid() {
                    format!("{name} (PID {pid})")
                } else {
                    name.to_owned()
                }
            }
            TargetKind::Process => {
                let name = self.target.name().map_or("Process", Arc::as_ref);
                if let Some(pid) = self.target.pid() {
                    format!("{name} [PID {pid}]")
                } else {
                    name.to_owned()
                }
            }
        }
    }

    pub fn kind_badge(&self) -> &'static str {
        match self.target.kind() {
            TargetKind::DefaultOutput => "SYS",
            TargetKind::DefaultInput => "MIC",
            TargetKind::Device => "DEV",
            TargetKind::InputDevice => "IN",
            TargetKind::Application => "APP",
            TargetKind::Process => "PID",
        }
    }

    pub fn backend_badge(&self) -> &'static str {
        backend_short_badge(self.target.backend())
    }
}

#[derive(Clone, Debug)]
pub struct ModalCandidate {
    pub selected: bool,
    pub target: CaptureTarget,
}

pub struct AddSourceModalState {
    pub open: bool,
    pub tab: ModalTab,
    pub search_query: String,
    pub search_focused: bool,
    pub audio_only: bool,
    pub playing_only: bool,
    pub backend_filter: Option<BackendId>,
    pub dedupe: DiscoveryDedupe,
    pub order: DiscoveryOrder,
    pub candidates: Vec<ModalCandidate>,
    pub scroll_offset: usize,
    pub cursor_idx: usize,
    pub hovered_candidate: Option<(usize, u16, u16)>,
    pub discovery_revision: u64,
    pub last_event_note: String,
    pub subscription: Option<DiscoverySubscription>,
}

impl Default for AddSourceModalState {
    fn default() -> Self {
        Self {
            open: false,
            tab: ModalTab::Applications,
            search_query: String::new(),
            search_focused: false,
            audio_only: true,
            playing_only: false,
            backend_filter: None,
            dedupe: DiscoveryDedupe::ByLogicalTarget,
            order: DiscoveryOrder::KindThenBackend,
            candidates: Vec::new(),
            scroll_offset: 0,
            cursor_idx: 0,
            hovered_candidate: None,
            discovery_revision: 0,
            last_event_note: "Ready".to_owned(),
            subscription: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PermissionsModalState {
    pub open: bool,
    pub last_action_note: String,
}

impl Default for PermissionsModalState {
    fn default() -> Self {
        Self {
            open: false,
            last_action_note:
                "Click [Ask OS] / [Probe] / [Settgs] or press 1/2/3 to manage consent".to_owned(),
        }
    }
}

pub struct App {
    pub should_quit: bool,
    pub staged_sources: Vec<StagedSource>,
    pub selected_source_idx: usize,
    pub modal: AddSourceModalState,

    pub combine_mode: CombineMode,
    pub tree_strategy: ProcessTreeRange,
    pub input_conversion: InputConversionPolicy,
    pub permission_policy: PermissionPolicy,
    pub open_settings_if_denied: bool,
    pub use_converter: bool,
    pub converter_rate: u32,
    pub converter_channels: u16,
    pub converter_sample: SampleType,
    pub converter_dither: DitherMode,
    pub silence_on_idle: bool,
    pub overflow_policy: Overflow,
    pub synthetic_mode: bool,
    pub live_monitor_enabled: bool,

    pub current_plan: Result<CapturePlanReport, String>,
    pub permission_report: Result<PermissionReport, String>,
    permission_report_overridden: bool,
    pub permissions_modal: PermissionsModalState,
    pub permission_runner: PermissionRunner,

    pub discovery: Discovery,
    pub worker: CaptureWorkerHandle,
    pub is_recording: bool,
    pub active_take_id: usize,
    pub next_take_id: usize,
    pub live_frames: usize,
    pub live_bytes: usize,
    pub live_segments: usize,
    pub live_format: Format,
    pub live_metadata: MixedBlockMetadata,
    pub live_inputs: Vec<InputTelemetryRow>,

    pub vis_mode: VisMode,
    pub spectroscope: SpectroscopeAnalyzer,

    pub takes: Vec<Arc<MemoryTake>>,
    pub selected_take_idx: usize,
    pub playback: PlaybackEngine,
    pub playback_status: PlaybackStatus,

    pub status_message: String,
    pub error_banner: Option<String>,
    last_discovery_poll: Instant,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let default_fmt = default_stereo_f32_format();

        let initial_staged = vec![StagedSource {
            enabled: true,
            target: CaptureTarget::default_output(DefaultDeviceMode::Follow),
        }];

        let mut app = Self {
            should_quit: false,
            staged_sources: initial_staged,
            selected_source_idx: 0,
            modal: AddSourceModalState::default(),
            combine_mode: CombineMode::Auto,
            tree_strategy: ProcessTreeRange::default(),
            input_conversion: InputConversionPolicy::AutoWhenMixing,
            permission_policy: PermissionPolicy::AutoRequest,
            open_settings_if_denied: true,
            use_converter: false,
            converter_rate: 48_000,
            converter_channels: 2,
            converter_sample: SampleType::F32,
            converter_dither: DitherMode::Tpdf,
            silence_on_idle: true,
            overflow_policy: Overflow::DropOldest,
            synthetic_mode: false,
            live_monitor_enabled: true,
            current_plan: Err("Initializing...".to_owned()),
            permission_report: Err("Initializing...".to_owned()),
            permission_report_overridden: false,
            permissions_modal: PermissionsModalState::default(),
            permission_runner: PermissionRunner::new(),
            discovery: Discovery::new(),
            worker: CaptureWorkerHandle::spawn(),
            is_recording: false,
            active_take_id: 0,
            next_take_id: 1,
            live_frames: 0,
            live_bytes: 0,
            live_segments: 1,
            live_format: default_fmt,
            live_metadata: MixedBlockMetadata::default(),
            live_inputs: Vec::new(),
            vis_mode: VisMode::FftSpectrum,
            spectroscope: SpectroscopeAnalyzer::new(48_000),
            takes: Vec::new(),
            selected_take_idx: 0,
            playback: PlaybackEngine::new(),
            playback_status: PlaybackStatus::default(),
            status_message:
                "Ready - Click [+ Add Source] (A) to pick apps/mics/devices, or [Record] (R) to start"
                    .to_owned(),
            error_banner: None,
            last_discovery_poll: Instant::now(),
        };
        app.recompute_plan();
        app.sync_worker_monitor();
        app
    }

    pub fn sync_worker_monitor(&self) {
        if self.is_recording {
            return;
        }
        let effective_enabled = self.live_monitor_enabled
            && (!self.playback_status.active || self.playback_status.paused);
        let summary = self.checked_targets_summary();
        let req = self.build_capture_request();
        self.worker.send(WorkerCommand::SyncMonitor {
            enabled: effective_enabled,
            targets_summary: summary,
            request: req,
            overflow: self.overflow_policy,
            synthetic: self.synthetic_mode,
        });
    }

    pub fn checked_targets(&self) -> Vec<CaptureTarget> {
        self.staged_sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.target.clone())
            .collect()
    }

    pub fn checked_targets_summary(&self) -> String {
        let enabled: Vec<String> = self
            .staged_sources
            .iter()
            .filter(|s| s.enabled)
            .map(StagedSource::display_name)
            .collect();
        if enabled.is_empty() {
            "No Active Sources (Parked)".to_owned()
        } else if enabled.len() == 1 {
            enabled[0].clone()
        } else {
            format!("{} sources ({})", enabled.len(), enabled.join(" + "))
        }
    }

    pub fn build_capture_request(&self) -> Option<CaptureRequest> {
        let targets = self.checked_targets();
        if targets.is_empty() {
            return None;
        }
        let mixer_cfg = MixerConfig {
            silence_on_idle: self.silence_on_idle,
            ..MixerConfig::default()
        };
        let mut req = CaptureRequest::new(targets)
            .with_combine_mode(self.combine_mode)
            .with_tree_strategy(self.tree_strategy)
            .with_input_conversion(self.input_conversion)
            .with_permission_policy(self.permission_policy)
            .with_mixer_config(mixer_cfg);

        let layout = match self.converter_channels {
            1 => Layout::Known(Arc::from([Channel::Mono])),
            2 => Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            _ => Layout::Unknown,
        };
        if self.use_converter
            && let Ok(target_fmt) = Format::new(
                self.converter_sample,
                ByteOrder::Little,
                self.converter_rate,
                self.converter_channels,
                layout,
            )
        {
            req = req.with_output_converter(Some(OutputConverterConfig::new(
                target_fmt,
                self.converter_dither,
            )));
        }
        Some(req)
    }

    pub fn build_modal_backends(&self) -> Backends {
        let Some(selected) = self.modal.backend_filter else {
            return Backends::default();
        };
        match PlatformOs::current() {
            PlatformOs::Windows => Backends::new(Some(vec![selected]), None, None),
            PlatformOs::Linux => Backends::new(None, Some(vec![selected]), None),
            PlatformOs::MacOs => Backends::new(None, None, Some(vec![selected])),
        }
        .unwrap_or_default()
    }

    pub fn build_modal_subscription_query(&self) -> DiscoveryQuery {
        DiscoveryQuery {
            os: PlatformOs::current(),
            backends: self.build_modal_backends(),
            dedupe: self.modal.dedupe,
            order: self.modal.order,
            active_only: false,
            include_non_audio: true,
            include_applications: true,
            include_processes: true,
            include_output_devices: true,
            include_input_devices: true,
        }
    }

    pub fn build_modal_tab_query(&self) -> DiscoveryQuery {
        let include_non_audio = !self.modal.audio_only;
        let active_only = self.modal.playing_only;
        DiscoveryQuery {
            os: PlatformOs::current(),
            backends: self.build_modal_backends(),
            dedupe: self.modal.dedupe,
            order: self.modal.order,
            active_only,
            include_non_audio,
            include_applications: self.modal.tab == ModalTab::Applications,
            include_processes: self.modal.tab == ModalTab::Processes,
            include_output_devices: self.modal.tab == ModalTab::OutputDevices,
            include_input_devices: self.modal.tab == ModalTab::Microphones,
        }
    }

    pub fn recompute_plan(&mut self) {
        if let Some(req) = self.build_capture_request() {
            self.current_plan = inspect_request_plan(&req);
        } else {
            self.current_plan = Err("No sources checked (check at least 1 source)".to_owned());
        }
        self.refresh_permissions();
    }

    pub fn effective_permission_os(&self) -> PlatformOs {
        self.permission_report
            .as_ref()
            .map_or_else(|_| PlatformOs::current(), |r| r.os)
    }

    pub fn merge_permission_report_with_staged(
        &self,
        report: PermissionReport,
    ) -> PermissionReport {
        let checked = self.checked_targets();
        let required_set: BTreeSet<PermissionKind> = if checked.is_empty() {
            BTreeSet::new()
        } else {
            planner::required_permission_kinds_for_os_with_filter(
                &checked,
                &Backends::default(),
                self.tree_strategy,
                report.os,
                |_, _| true,
            )
            .unwrap_or_default()
        };
        let map_entry = |mut e: PermissionEntry| {
            e.required = required_set.contains(&e.kind);
            e
        };
        PermissionReport::new(
            report.os,
            map_entry(report.microphone()),
            map_entry(report.system_audio()),
            map_entry(report.screen_recording()),
        )
    }

    pub fn refresh_permissions(&mut self) {
        if self.permission_report_overridden
            && let Ok(existing) = self.permission_report
        {
            self.permission_report = Ok(self.merge_permission_report_with_staged(existing));
            return;
        }
        let req = self.build_capture_request();
        let backends = req
            .as_ref()
            .map_or_else(Backends::default, |r| r.backends().clone());
        self.permission_report = inspect_permissions_for_optional_request(req.as_ref(), &backends);
    }

    pub fn set_permission_report_for_test(&mut self, report: PermissionReport) {
        self.permission_report_overridden = true;
        self.permission_report = Ok(self.merge_permission_report_with_staged(report));
    }

    pub fn target_permission_entry(&self, target: &CaptureTarget) -> Option<PermissionEntry> {
        let report = self.permission_report.as_ref().ok()?;
        let kind = target_required_permission_kind(
            target,
            self.tree_strategy,
            &Backends::default(),
            report.os,
        )?;
        Some(report.entry(kind))
    }

    pub fn modal_tab_primary_permission_kind(&self) -> PermissionKind {
        let os = self.effective_permission_os();
        match self.modal.tab {
            ModalTab::Microphones => PermissionKind::Microphone,
            ModalTab::Applications if os == PlatformOs::MacOs => {
                if let Ok(rep) = &self.permission_report
                    && rep.screen_recording().is_granted()
                    && !rep.system_audio().is_granted()
                {
                    PermissionKind::SystemAudio
                } else {
                    PermissionKind::ScreenRecording
                }
            }
            ModalTab::Applications | ModalTab::Processes | ModalTab::OutputDevices => {
                PermissionKind::SystemAudio
            }
        }
    }

    pub fn trigger_domain_permission_action(&mut self, kind: PermissionKind) {
        let os = self.effective_permission_os();
        if let Ok(rep) = &self.permission_report {
            let entry = rep.entry(kind);
            if entry.can_prompt
                || (os == PlatformOs::MacOs
                    && kind == PermissionKind::SystemAudio
                    && entry.state == PermissionState::Unknown)
            {
                self.handle_click_action(ClickAction::RequestDomainPermission(kind));
                return;
            }
            if entry.state.is_denied_or_restricted() && domain_supports_open_settings(os, kind) {
                self.handle_click_action(ClickAction::OpenDomainSettings(kind));
                return;
            }
            let msg = format!(
                "{} is {} (no interactive OS prompt available on {:?})",
                kind.as_str(),
                entry.state.as_str(),
                os
            );
            self.permissions_modal.last_action_note = msg.clone();
            self.status_message = msg;
        }
    }

    fn apply_permission_job_outcome(&mut self, outcome: PermissionJobOutcome) {
        match outcome.result {
            Ok(raw_report) => {
                let merged = self.merge_permission_report_with_staged(raw_report);
                let summary = if merged.requires_relaunch() {
                    format!(
                        "{}: consent updated -> relaunch required for ScreenCaptureKit",
                        outcome.label
                    )
                } else if merged.has_denial() {
                    format!(
                        "{}: one or more required domains denied/restricted",
                        outcome.label
                    )
                } else if merged.is_granted() {
                    format!("{}: all required permissions granted", outcome.label)
                } else {
                    format!(
                        "{}: Mic={} Sys={} Scr={}",
                        outcome.label,
                        merged.microphone().state.as_str(),
                        merged.system_audio().state.as_str(),
                        merged.screen_recording().state.as_str()
                    )
                };
                self.permission_report = Ok(merged);
                self.permissions_modal.last_action_note = summary.clone();
                self.status_message = summary;

                if self.modal.open {
                    let _ = self.discovery.refresh_once();
                    self.refresh_modal_candidates();
                }

                if merged.is_granted() && !merged.requires_relaunch() {
                    if self
                        .error_banner
                        .as_ref()
                        .is_some_and(|e| e.to_ascii_lowercase().contains("permission"))
                    {
                        self.error_banner = None;
                    }
                    if !self.is_recording && self.live_monitor_enabled {
                        self.sync_worker_monitor();
                    }
                }
            }
            Err(err) => {
                let note = format!("{} failed: {err}", outcome.label);
                self.permissions_modal.last_action_note = note.clone();
                self.error_banner = Some(note);
            }
        }
    }

    pub fn on_tick(&mut self) {
        let mut received_audio = false;

        while let Some(evt) = self.worker.try_recv() {
            match evt {
                WorkerEvent::Telemetry(tel) => {
                    self.apply_live_telemetry(tel, &mut received_audio);
                }
                WorkerEvent::SessionOpened => {
                    self.refresh_permissions();
                }
                WorkerEvent::HotSwapped {
                    segment_index,
                    frame_offset,
                    targets_summary,
                } => {
                    self.live_segments = segment_index;
                    self.status_message = format!(
                        "Hot-swapped -> Segment #{segment_index} at frame {frame_offset}: {targets_summary}"
                    );
                    self.error_banner = None;
                }
                WorkerEvent::TakeCompleted(take) => {
                    self.is_recording = false;
                    if !self.live_monitor_enabled {
                        self.live_inputs.clear();
                    }
                    let dur = take.duration_secs();
                    let id = take.id;
                    let segs = take.segments.len();
                    self.takes.insert(0, take);
                    self.selected_take_idx = 0;
                    self.status_message = format!(
                        "Take #{id} stored in RAM ({dur:.2}s, {segs} segment(s)). Click [Play] or [Save WAV]."
                    );
                }
                WorkerEvent::StatusMessage(msg) => {
                    self.status_message = msg;
                }
                WorkerEvent::Error(err) => {
                    if err.to_ascii_lowercase().contains("permission") {
                        self.refresh_permissions();
                    }
                    self.error_banner = Some(err);
                }
            }
        }

        while let Some(outcome) = self.permission_runner.try_recv() {
            self.apply_permission_job_outcome(outcome);
        }

        let was_playing = self.playback_status.active && !self.playback_status.paused;
        self.playback_status = self.playback.status();
        let now_playing = self.playback_status.active && !self.playback_status.paused;
        if was_playing && !now_playing && !self.is_recording && self.live_monitor_enabled {
            self.sync_worker_monitor();
        }
        if let Some(play_err) = self.playback.take_error() {
            self.error_banner = Some(play_err);
        }
        if self.playback_status.active && !self.playback_status.paused {
            let mut play_vis = Vec::new();
            self.playback.drain_visual_samples(&mut play_vis);
            if !play_vis.is_empty() {
                self.spectroscope
                    .set_sample_rate(self.playback_status.sample_rate);
                self.spectroscope.push_stereo_samples(&play_vis);
                received_audio = true;
            }
        }

        if !received_audio {
            self.spectroscope.decay_idle();
        }

        if self.modal.open && self.last_discovery_poll.elapsed().as_millis() >= 350 {
            self.poll_modal_discovery_events();
            self.last_discovery_poll = Instant::now();
        }
    }

    fn apply_live_telemetry(&mut self, tel: LiveCaptureTelemetry, received_audio: &mut bool) {
        self.live_frames = tel.total_frames;
        self.live_bytes = tel.total_bytes;
        self.live_segments = tel.segment_count;
        self.live_format = tel.format.clone();
        self.live_metadata = tel.metadata;
        self.live_inputs = tel.input_rows;
        if !tel.stereo_samples.is_empty() {
            self.spectroscope.set_sample_rate(tel.format.rate());
            self.spectroscope.push_stereo_samples(&tel.stereo_samples);
            *received_audio = true;
        }
    }

    fn on_sources_or_config_modified(&mut self) {
        self.recompute_plan();
        if self.is_recording {
            let summary = self.checked_targets_summary();
            let req = self.build_capture_request();
            self.worker.send(WorkerCommand::HotSwap {
                targets_summary: summary,
                request: req,
                overflow: self.overflow_policy,
                synthetic: self.synthetic_mode,
            });
        } else {
            self.sync_worker_monitor();
        }
    }

    fn resubscribe_modal_if_open(&mut self) {
        if self.modal.open {
            let sub_query = self.build_modal_subscription_query();
            self.modal.subscription = self.discovery.subscribe_query(sub_query, 256).ok();
        }
    }

    pub fn handle_click_action(&mut self, action: ClickAction) {
        match action {
            ClickAction::OpenAddSourceModal => self.open_add_source_modal(),
            ClickAction::CloseModal => {
                self.modal.open = false;
                self.modal.subscription = None;
                self.modal.search_focused = false;
                self.modal.hovered_candidate = None;
            }
            ClickAction::SwitchModalTab(tab) => {
                self.modal.tab = tab;
                self.modal.cursor_idx = 0;
                self.modal.scroll_offset = 0;
                self.modal.hovered_candidate = None;
                self.refresh_modal_candidates();
            }
            ClickAction::FocusModalSearch => {
                self.modal.search_focused = true;
            }
            ClickAction::ClearModalSearch => {
                self.modal.search_query.clear();
                self.refresh_modal_candidates();
            }
            ClickAction::ToggleModalAudioOnly => {
                self.modal.audio_only = !self.modal.audio_only;
                self.refresh_modal_candidates();
            }
            ClickAction::ToggleModalPlayingOnly => {
                self.modal.playing_only = !self.modal.playing_only;
                if self.modal.playing_only {
                    self.modal.audio_only = true;
                }
                self.refresh_modal_candidates();
            }
            ClickAction::CycleModalBackendFilter => {
                let os_backends = available_os_backends();
                let mut opts: Vec<Option<BackendId>> = vec![None];
                for b in os_backends {
                    opts.push(Some(b));
                }
                let pos = opts
                    .iter()
                    .position(|x| *x == self.modal.backend_filter)
                    .unwrap_or(0);
                self.modal.backend_filter = opts[(pos + 1) % opts.len()];
                self.resubscribe_modal_if_open();
                self.refresh_modal_candidates();
            }
            ClickAction::ToggleModalDedupe => {
                self.modal.dedupe = match self.modal.dedupe {
                    DiscoveryDedupe::ByLogicalTarget => DiscoveryDedupe::PerBackend,
                    DiscoveryDedupe::PerBackend => DiscoveryDedupe::ByLogicalTarget,
                };
                self.resubscribe_modal_if_open();
                self.refresh_modal_candidates();
            }
            ClickAction::ToggleModalOrder => {
                self.modal.order = match self.modal.order {
                    DiscoveryOrder::KindThenBackend => DiscoveryOrder::BackendThenKind,
                    DiscoveryOrder::BackendThenKind => DiscoveryOrder::KindThenBackend,
                };
                self.resubscribe_modal_if_open();
                self.refresh_modal_candidates();
            }
            ClickAction::RefreshModalDiscovery => {
                let _ = self.discovery.refresh_once();
                self.refresh_modal_candidates();
            }
            ClickAction::ToggleModalCandidate(idx) => {
                if let Some(cand) = self.modal.candidates.get_mut(idx) {
                    cand.selected = !cand.selected;
                    self.modal.cursor_idx = idx;
                }
            }
            ClickAction::ConfirmAddModalCandidates => {
                self.confirm_add_modal_selection();
            }

            ClickAction::ToggleStagedSource(idx) => {
                if let Some(src) = self.staged_sources.get_mut(idx) {
                    src.enabled = !src.enabled;
                    self.selected_source_idx = idx;
                    self.on_sources_or_config_modified();
                }
            }
            ClickAction::SelectStagedSource(idx) => {
                if idx < self.staged_sources.len() {
                    self.selected_source_idx = idx;
                }
            }
            ClickAction::CycleStagedBackend(idx) => {
                if let Some(src) = self.staged_sources.get_mut(idx) {
                    let pool: Vec<BackendId> = if !src.target.available_backends().is_empty() {
                        src.target.available_backends().to_vec()
                    } else {
                        available_os_backends()
                    };
                    let mut choices: Vec<Option<BackendId>> = vec![None];
                    for b in pool {
                        if !choices.contains(&Some(b)) {
                            choices.push(Some(b));
                        }
                    }
                    let cur = src.target.backend();
                    let next_idx = choices
                        .iter()
                        .position(|c| *c == cur)
                        .map_or(0, |p| (p + 1) % choices.len());
                    src.target = src.target.clone().with_optional_backend(choices[next_idx]);
                    self.selected_source_idx = idx;
                    self.on_sources_or_config_modified();
                }
            }
            ClickAction::CycleStagedGain(idx) => {
                if let Some(src) = self.staged_sources.get_mut(idx) {
                    let cur = src.target.gain();
                    let steps = [1.0f32, 0.5, 1.5, 0.25];
                    let pos = steps
                        .iter()
                        .position(|&g| (g - cur).abs() < 1e-4)
                        .unwrap_or(0);
                    let next_gain = steps[(pos + 1) % steps.len()];
                    if let Ok(updated) = src.target.clone().with_gain(next_gain) {
                        src.target = updated;
                        self.selected_source_idx = idx;
                        self.on_sources_or_config_modified();
                    }
                }
            }
            ClickAction::CycleStagedScope(idx) => {
                if let Some(src) = self.staged_sources.get_mut(idx)
                    && matches!(
                        src.target.kind(),
                        TargetKind::Application | TargetKind::Process
                    )
                {
                    let next_scope = match src.target.scope() {
                        ProcessScope::Tree => ProcessScope::Exact,
                        ProcessScope::Exact => ProcessScope::Tree,
                    };
                    src.target = src.target.clone().with_scope(next_scope);
                    self.selected_source_idx = idx;
                    self.on_sources_or_config_modified();
                }
            }
            ClickAction::CycleStagedExitPolicy(idx) => {
                if let Some(src) = self.staged_sources.get_mut(idx)
                    && matches!(
                        src.target.kind(),
                        TargetKind::Application | TargetKind::Process
                    )
                {
                    let next_exit = match src.target.on_root_exit() {
                        OnRootExit::Stop => OnRootExit::Wait {
                            wait_timeout_ns: 5_000_000_000,
                        },
                        OnRootExit::Wait { .. } => OnRootExit::Stop,
                    };
                    if let Ok(updated) = src.target.clone().with_on_root_exit(next_exit) {
                        src.target = updated;
                        self.selected_source_idx = idx;
                        self.on_sources_or_config_modified();
                    }
                }
            }
            ClickAction::CycleStagedDeviceMode(idx) => {
                if let Some(src) = self.staged_sources.get_mut(idx)
                    && matches!(
                        src.target.kind(),
                        TargetKind::DefaultOutput | TargetKind::DefaultInput
                    )
                {
                    let next_mode = match src.target.default_device_mode() {
                        DefaultDeviceMode::Follow => DefaultDeviceMode::Pin,
                        DefaultDeviceMode::Pin => DefaultDeviceMode::Follow,
                    };
                    src.target = src.target.clone().with_default_device_mode(next_mode);
                    self.selected_source_idx = idx;
                    self.on_sources_or_config_modified();
                }
            }
            ClickAction::DuplicateStagedSource(idx) => {
                if let Some(existing) = self.staged_sources.get(idx).cloned() {
                    self.staged_sources.insert(idx + 1, existing);
                    self.selected_source_idx = idx + 1;
                    self.status_message =
                        "Duplicated source (demonstrates caller-owned overlap 2x summation)"
                            .to_owned();
                    self.on_sources_or_config_modified();
                }
            }
            ClickAction::RemoveStagedSource(idx) => {
                if idx < self.staged_sources.len() {
                    self.staged_sources.remove(idx);
                    if self.selected_source_idx >= self.staged_sources.len()
                        && !self.staged_sources.is_empty()
                    {
                        self.selected_source_idx = self.staged_sources.len() - 1;
                    }
                    self.on_sources_or_config_modified();
                }
            }
            ClickAction::EnableAllStaged => {
                for s in &mut self.staged_sources {
                    s.enabled = true;
                }
                self.on_sources_or_config_modified();
            }
            ClickAction::DisableAllStaged => {
                for s in &mut self.staged_sources {
                    s.enabled = false;
                }
                self.on_sources_or_config_modified();
            }

            ClickAction::CycleCombineMode => {
                self.combine_mode = match self.combine_mode {
                    CombineMode::Auto => CombineMode::NativeOnly,
                    CombineMode::NativeOnly => CombineMode::CspaMixer,
                    CombineMode::CspaMixer => CombineMode::Auto,
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::CycleTreeStrategy => {
                self.tree_strategy = match (
                    self.tree_strategy.start_level,
                    self.tree_strategy.fallback_limit,
                ) {
                    (
                        ProcessTreeStrategy::NativeKernelCapture,
                        Some(ProcessTreeStrategy::SoftwareStreamMix),
                    ) => ProcessTreeRange {
                        start_level: ProcessTreeStrategy::NativeKernelCapture,
                        fallback_limit: Some(ProcessTreeStrategy::NativeGraphMix),
                    },
                    (
                        ProcessTreeStrategy::NativeKernelCapture,
                        Some(ProcessTreeStrategy::NativeGraphMix),
                    ) => ProcessTreeRange {
                        start_level: ProcessTreeStrategy::NativeKernelCapture,
                        fallback_limit: None,
                    },
                    (ProcessTreeStrategy::NativeKernelCapture, None) => ProcessTreeRange {
                        start_level: ProcessTreeStrategy::SoftwareStreamMix,
                        fallback_limit: Some(ProcessTreeStrategy::SoftwareStreamMix),
                    },
                    _ => ProcessTreeRange::default(),
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::ToggleInputConversion => {
                self.input_conversion = match self.input_conversion {
                    InputConversionPolicy::AutoWhenMixing => {
                        InputConversionPolicy::StrictNoResample
                    }
                    InputConversionPolicy::StrictNoResample => {
                        InputConversionPolicy::AutoWhenMixing
                    }
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::ToggleFormatConverter => {
                self.use_converter = !self.use_converter;
                self.on_sources_or_config_modified();
            }
            ClickAction::CycleSampleRate => {
                self.use_converter = true;
                self.converter_rate = match self.converter_rate {
                    48_000 => 44_100,
                    44_100 => 16_000,
                    _ => 48_000,
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::CycleChannels => {
                self.use_converter = true;
                self.converter_channels = if self.converter_channels == 2 { 1 } else { 2 };
                self.on_sources_or_config_modified();
            }
            ClickAction::CycleSampleFormat => {
                self.use_converter = true;
                self.converter_sample = match self.converter_sample {
                    SampleType::F32 => SampleType::I16,
                    SampleType::I16 => SampleType::I24,
                    SampleType::I24 => SampleType::I32,
                    SampleType::I32 => SampleType::F32,
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::CycleDither => {
                self.use_converter = true;
                self.converter_dither = match self.converter_dither {
                    DitherMode::Tpdf => DitherMode::None,
                    DitherMode::None => DitherMode::Tpdf,
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::ToggleSilenceOnIdle => {
                self.silence_on_idle = !self.silence_on_idle;
                self.on_sources_or_config_modified();
            }
            ClickAction::CycleOverflowPolicy => {
                self.overflow_policy = match self.overflow_policy {
                    Overflow::DropOldest => Overflow::DropNewest,
                    Overflow::DropNewest => Overflow::Fail,
                    Overflow::Fail => Overflow::DropOldest,
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::ToggleSyntheticSignal => {
                self.synthetic_mode = !self.synthetic_mode;
                self.status_message = if self.synthetic_mode {
                    "Synth Test Signal ON (generates 440/1320/3520 Hz harmonic test tone)"
                        .to_owned()
                } else {
                    "Native OS Audio Capture ON".to_owned()
                };
                self.on_sources_or_config_modified();
            }
            ClickAction::ToggleLiveMonitor => {
                self.live_monitor_enabled = !self.live_monitor_enabled;
                if !self.live_monitor_enabled && !self.is_recording {
                    self.live_inputs.clear();
                    self.live_frames = 0;
                    self.live_bytes = 0;
                }
                self.status_message = if self.live_monitor_enabled {
                    "Live Spectroscope Monitor: ON (visualizes checked sources when idle)"
                        .to_owned()
                } else {
                    "Live Spectroscope Monitor: OFF (visualizes only during Record/Play)".to_owned()
                };
                self.sync_worker_monitor();
            }

            ClickAction::SelectVisMode(mode) => {
                self.vis_mode = mode;
            }

            ClickAction::ToggleRecordStop => {
                if self.is_recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            ClickAction::SelectTake(idx) => {
                if idx < self.takes.len() {
                    self.selected_take_idx = idx;
                }
            }
            ClickAction::PlayPauseSelectedTake => {
                self.play_or_pause_selected_take();
            }
            ClickAction::StopPlayback => {
                self.playback.stop();
                self.playback_status.active = false;
                self.sync_worker_monitor();
                self.status_message = "Playback stopped".to_owned();
            }
            ClickAction::SeekPlaybackBar { .. } => {}
            ClickAction::SaveSelectedTakeWav => {
                self.save_selected_take_to_disk();
            }
            ClickAction::DeleteSelectedTake => {
                if self.selected_take_idx < self.takes.len() {
                    let removed = self.takes.remove(self.selected_take_idx);
                    if self.playback_status.take_id == Some(removed.id) {
                        self.playback.stop();
                    }
                    if self.selected_take_idx >= self.takes.len() && !self.takes.is_empty() {
                        self.selected_take_idx = self.takes.len() - 1;
                    }
                    self.status_message = format!("Deleted Take #{} from RAM", removed.id);
                }
            }

            ClickAction::OpenPermissionsModal => {
                self.modal.open = false;
                self.modal.subscription = None;
                self.modal.search_focused = false;
                self.modal.hovered_candidate = None;
                self.permissions_modal.open = true;
                self.refresh_permissions();
            }
            ClickAction::ClosePermissionsModal => {
                self.permissions_modal.open = false;
            }
            ClickAction::TogglePermissionPolicy => {
                self.permission_policy = match self.permission_policy {
                    PermissionPolicy::AutoRequest => PermissionPolicy::CheckOnly,
                    PermissionPolicy::CheckOnly => PermissionPolicy::AutoRequest,
                };
                let note = format!(
                    "PermissionPolicy on open() -> {}",
                    self.permission_policy.as_str()
                );
                self.permissions_modal.last_action_note = note.clone();
                self.status_message = note;
                self.on_sources_or_config_modified();
            }
            ClickAction::ToggleOpenSettingsFallback => {
                self.open_settings_if_denied = !self.open_settings_if_denied;
                let note = format!(
                    "Open OS Privacy Settings on denial -> {}",
                    if self.open_settings_if_denied {
                        "ON"
                    } else {
                        "OFF"
                    }
                );
                self.permissions_modal.last_action_note = note.clone();
                self.status_message = note;
            }
            ClickAction::RefreshPermissions => {
                self.refresh_permissions();
                let note = "Re-checked OS recording permission states".to_owned();
                self.permissions_modal.last_action_note = note.clone();
                self.status_message = note;
            }
            ClickAction::RequestRequiredPermissions => {
                if self.permission_runner.is_in_flight() {
                    self.status_message =
                        "Permission request already in flight (awaiting OS response)...".to_owned();
                    return;
                }
                let job = if let Some(req) = self.build_capture_request() {
                    PermissionJobKind::ForRequest {
                        request: req,
                        open_settings_if_denied: self.open_settings_if_denied,
                    }
                } else {
                    PermissionJobKind::ForKinds {
                        kinds: vec![PermissionKind::Microphone, PermissionKind::SystemAudio],
                        backends: Backends::default(),
                        open_settings_if_denied: self.open_settings_if_denied,
                    }
                };
                if self
                    .permission_runner
                    .submit("Request required permissions".to_owned(), job)
                {
                    let note = "Requesting required OS recording permissions...".to_owned();
                    self.permissions_modal.last_action_note = note.clone();
                    self.status_message = note;
                }
            }
            ClickAction::RequestDomainPermission(kind) => {
                if self.permission_runner.is_in_flight() {
                    self.status_message =
                        "Permission request already in flight (awaiting OS response)...".to_owned();
                    return;
                }
                let job = PermissionJobKind::ForKinds {
                    kinds: vec![kind],
                    backends: Backends::default(),
                    open_settings_if_denied: self.open_settings_if_denied,
                };
                let label = format!("Request {}", kind.as_str());
                if self.permission_runner.submit(label, job) {
                    let note = format!("Requesting OS consent for {}...", kind.as_str());
                    self.permissions_modal.last_action_note = note.clone();
                    self.status_message = note;
                }
            }
            ClickAction::OpenDomainSettings(kind) => {
                if self.permission_runner.is_in_flight() {
                    return;
                }
                let job = PermissionJobKind::ForKinds {
                    kinds: vec![kind],
                    backends: Backends::default(),
                    open_settings_if_denied: true,
                };
                let label = format!("Open privacy settings ({})", kind.as_str());
                if self.permission_runner.submit(label, job) {
                    let note = format!("Opening OS privacy settings for {}...", kind.as_str());
                    self.permissions_modal.last_action_note = note.clone();
                    self.status_message = note;
                }
            }
        }
    }

    pub fn handle_mouse_event(&mut self, mouse: MouseEvent, hit_map: &crate::hit_test::HitMap) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(action) = hit_map.resolve(mouse.column, mouse.row) {
                    if let ClickAction::SeekPlaybackBar { bar_x, bar_width } = action {
                        let rel = mouse.column.saturating_sub(bar_x) as f32;
                        let ratio = (rel / (bar_width.max(1) as f32)).clamp(0.0, 1.0);
                        if !self.is_recording
                            && !self.playback_status.active
                            && let Some(take) = self.takes.get(self.selected_take_idx).cloned()
                        {
                            self.worker.send(WorkerCommand::SyncMonitor {
                                enabled: false,
                                targets_summary: self.checked_targets_summary(),
                                request: None,
                                overflow: self.overflow_policy,
                                synthetic: self.synthetic_mode,
                            });
                            self.playback.play(take);
                        }
                        self.playback.seek_ratio(ratio);
                        self.status_message = format!("Seeked playback to {:.0}%", ratio * 100.0);
                    } else {
                        self.handle_click_action(action);
                    }
                }
            }
            MouseEventKind::Moved => {
                if self.modal.open {
                    if let Some(ClickAction::ToggleModalCandidate(idx)) =
                        hit_map.resolve(mouse.column, mouse.row)
                        && idx < self.modal.candidates.len()
                    {
                        self.modal.hovered_candidate = Some((idx, mouse.column, mouse.row));
                    } else {
                        self.modal.hovered_candidate = None;
                    }
                } else {
                    self.modal.hovered_candidate = None;
                }
            }
            MouseEventKind::ScrollDown => {
                if self.modal.open {
                    self.modal.hovered_candidate = None;
                    if self.modal.cursor_idx + 1 < self.modal.candidates.len() {
                        self.modal.cursor_idx += 1;
                    }
                } else if self.selected_source_idx + 1 < self.staged_sources.len() {
                    self.selected_source_idx += 1;
                }
            }
            MouseEventKind::ScrollUp => {
                if self.modal.open {
                    self.modal.hovered_candidate = None;
                    self.modal.cursor_idx = self.modal.cursor_idx.saturating_sub(1);
                } else {
                    self.selected_source_idx = self.selected_source_idx.saturating_sub(1);
                }
            }
            _ => {}
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        if self.permissions_modal.open {
            self.handle_permissions_modal_key(key);
            return;
        }

        if self.modal.open {
            self.handle_modal_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Char('a') | KeyCode::Char('A') => self.open_add_source_modal(),
            KeyCode::Char('k') | KeyCode::Char('K') => {
                self.handle_click_action(ClickAction::OpenPermissionsModal);
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if self.is_recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if self.is_recording {
                    self.stop_recording();
                } else {
                    self.playback.stop();
                }
            }
            KeyCode::Char('p') | KeyCode::Char('P') => self.play_or_pause_selected_take(),
            KeyCode::Char('w') | KeyCode::Char('W') => self.save_selected_take_to_disk(),
            KeyCode::Char('v') | KeyCode::Char('V') => {
                self.vis_mode = self.vis_mode.next();
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                self.handle_click_action(ClickAction::ToggleSyntheticSignal);
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.handle_click_action(ClickAction::ToggleLiveMonitor);
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.handle_click_action(ClickAction::CycleStagedBackend(self.selected_source_idx));
            }
            KeyCode::Char('g') | KeyCode::Char('G') => {
                self.handle_click_action(ClickAction::CycleStagedGain(self.selected_source_idx));
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.handle_click_action(ClickAction::CycleCombineMode);
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.handle_click_action(ClickAction::DuplicateStagedSource(
                    self.selected_source_idx,
                ));
            }
            KeyCode::Delete | KeyCode::Backspace => {
                self.handle_click_action(ClickAction::RemoveStagedSource(self.selected_source_idx));
            }
            KeyCode::Char(' ') => {
                self.handle_click_action(ClickAction::ToggleStagedSource(self.selected_source_idx));
            }
            KeyCode::Up => {
                self.selected_source_idx = self.selected_source_idx.saturating_sub(1);
            }
            KeyCode::Down if self.selected_source_idx + 1 < self.staged_sources.len() => {
                self.selected_source_idx += 1;
            }
            KeyCode::Left => {
                let ratio = (self.playback_status.progress_ratio() - 0.05).clamp(0.0, 1.0);
                self.playback.seek_ratio(ratio);
            }
            KeyCode::Right => {
                let ratio = (self.playback_status.progress_ratio() + 0.05).clamp(0.0, 1.0);
                self.playback.seek_ratio(ratio);
            }
            KeyCode::Char('[') => {
                self.selected_take_idx = self.selected_take_idx.saturating_sub(1);
            }
            KeyCode::Char(']') if self.selected_take_idx + 1 < self.takes.len() => {
                self.selected_take_idx += 1;
            }
            _ => {}
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        if self.modal.search_focused {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.modal.search_focused = false;
                }
                KeyCode::Backspace => {
                    self.modal.search_query.pop();
                    self.refresh_modal_candidates();
                }
                KeyCode::Char(ch) => {
                    self.modal.search_query.push(ch);
                    self.refresh_modal_candidates();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc => {
                self.modal.open = false;
                self.modal.subscription = None;
                self.modal.hovered_candidate = None;
            }
            KeyCode::Tab => {
                self.modal.tab = self.modal.tab.next();
                self.modal.cursor_idx = 0;
                self.modal.scroll_offset = 0;
                self.modal.hovered_candidate = None;
                self.refresh_modal_candidates();
            }
            KeyCode::Char('/') => {
                self.modal.search_focused = true;
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                self.handle_click_action(ClickAction::ToggleModalAudioOnly);
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.handle_click_action(ClickAction::ToggleModalPlayingOnly);
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.handle_click_action(ClickAction::CycleModalBackendFilter);
            }
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.handle_click_action(ClickAction::ToggleModalDedupe);
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                self.handle_click_action(ClickAction::ToggleModalOrder);
            }
            KeyCode::Char('g') | KeyCode::Char('G') => {
                let kind = self.modal_tab_primary_permission_kind();
                self.trigger_domain_permission_action(kind);
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                self.handle_click_action(ClickAction::OpenPermissionsModal);
            }
            KeyCode::Up => {
                self.modal.cursor_idx = self.modal.cursor_idx.saturating_sub(1);
            }
            KeyCode::Down if self.modal.cursor_idx + 1 < self.modal.candidates.len() => {
                self.modal.cursor_idx += 1;
            }
            KeyCode::Char(' ') => {
                let idx = self.modal.cursor_idx;
                self.handle_click_action(ClickAction::ToggleModalCandidate(idx));
            }
            KeyCode::Enter => {
                if self.modal.candidates.iter().all(|c| !c.selected)
                    && let Some(cand) = self.modal.candidates.get_mut(self.modal.cursor_idx)
                {
                    cand.selected = true;
                }
                self.confirm_add_modal_selection();
            }
            _ => {}
        }
    }

    fn handle_permissions_modal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('k') | KeyCode::Char('K') => {
                self.handle_click_action(ClickAction::ClosePermissionsModal);
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.should_quit = true;
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.handle_click_action(ClickAction::TogglePermissionPolicy);
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                self.handle_click_action(ClickAction::ToggleOpenSettingsFallback);
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.handle_click_action(ClickAction::RefreshPermissions);
            }
            KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                self.handle_click_action(ClickAction::RequestRequiredPermissions);
            }
            KeyCode::Char('1') => {
                self.trigger_domain_permission_action(PermissionKind::Microphone);
            }
            KeyCode::Char('2') => {
                self.trigger_domain_permission_action(PermissionKind::SystemAudio);
            }
            KeyCode::Char('3') => {
                self.trigger_domain_permission_action(PermissionKind::ScreenRecording);
            }
            _ => {}
        }
    }

    pub fn open_add_source_modal(&mut self) {
        self.permissions_modal.open = false;
        self.modal.open = true;
        self.modal.search_focused = false;
        self.modal.hovered_candidate = None;
        self.refresh_permissions();
        let sub_query = self.build_modal_subscription_query();
        self.modal.subscription = self.discovery.subscribe_query(sub_query, 256).ok();
        self.refresh_modal_candidates();
    }

    pub fn refresh_modal_candidates(&mut self) {
        let previous_cursor_id = self
            .modal
            .candidates
            .get(self.modal.cursor_idx)
            .map(|c| c.target.id().clone());
        let previous_hovered = self.modal.hovered_candidate.and_then(|(h_idx, col, row)| {
            self.modal
                .candidates
                .get(h_idx)
                .map(|c| (c.target.id().clone(), col, row))
        });

        let previously_selected: BTreeSet<String> = self
            .modal
            .candidates
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.target.id().to_string())
            .collect();

        let use_cached = self.modal.subscription.is_some();
        let tab_query = self.build_modal_tab_query();
        let active_backends = tab_query.backends.for_os(tab_query.os).to_vec();

        let mut raw_list = Vec::new();
        match self.modal.tab {
            ModalTab::OutputDevices => {
                if self.modal.dedupe == DiscoveryDedupe::PerBackend {
                    for &b in &active_backends {
                        let cand = CaptureTarget::default_output(DefaultDeviceMode::Follow)
                            .with_backend(b);
                        if CaptureRequest::single(cand.clone()).inspect().is_ok() {
                            raw_list.push(cand);
                        }
                    }
                } else {
                    let mut def_out = CaptureTarget::default_output(DefaultDeviceMode::Follow);
                    if let Some(b) = self.modal.backend_filter {
                        def_out = def_out.with_backend(b);
                    } else {
                        let supported: Vec<BackendId> = active_backends
                            .iter()
                            .copied()
                            .filter(|&b| {
                                CaptureRequest::single(def_out.clone().with_backend(b))
                                    .inspect()
                                    .is_ok()
                            })
                            .collect();
                        def_out = def_out.with_available_backends(supported);
                    }
                    if CaptureRequest::single(def_out.clone()).inspect().is_ok() {
                        raw_list.push(def_out);
                    }
                }
            }
            ModalTab::Microphones => {
                if self.modal.dedupe == DiscoveryDedupe::PerBackend {
                    for &b in &active_backends {
                        let cand =
                            CaptureTarget::microphone(DefaultDeviceMode::Follow).with_backend(b);
                        if CaptureRequest::single(cand.clone()).inspect().is_ok() {
                            raw_list.push(cand);
                        }
                    }
                } else {
                    let mut def_mic = CaptureTarget::microphone(DefaultDeviceMode::Follow);
                    if let Some(b) = self.modal.backend_filter {
                        def_mic = def_mic.with_backend(b);
                    } else {
                        let supported: Vec<BackendId> = active_backends
                            .iter()
                            .copied()
                            .filter(|&b| {
                                CaptureRequest::single(def_mic.clone().with_backend(b))
                                    .inspect()
                                    .is_ok()
                            })
                            .collect();
                        def_mic = def_mic.with_available_backends(supported);
                    }
                    if CaptureRequest::single(def_mic.clone()).inspect().is_ok() {
                        raw_list.push(def_mic);
                    }
                }
            }
            ModalTab::Applications | ModalTab::Processes => {}
        }

        if let Ok(mut catalog) = query_catalog(&self.discovery, &tab_query, use_cached) {
            raw_list.append(&mut catalog.targets);
        }

        let query_lower = self.modal.search_query.trim().to_ascii_lowercase();
        let mut filtered: Vec<ModalCandidate> = raw_list
            .into_iter()
            .filter(|t| {
                if query_lower.is_empty() {
                    return true;
                }
                let name_match = t
                    .name()
                    .is_some_and(|n| n.to_ascii_lowercase().contains(&query_lower));
                let app_match = t
                    .app_id()
                    .is_some_and(|a| a.to_ascii_lowercase().contains(&query_lower));
                let pid_match = t
                    .pid()
                    .is_some_and(|p| p.to_string().contains(&query_lower));
                let id_match = t.id().to_ascii_lowercase().contains(&query_lower);
                name_match || app_match || pid_match || id_match
            })
            .map(|target| {
                let selected = previously_selected.contains(target.id().as_ref());
                ModalCandidate { selected, target }
            })
            .collect();

        let order_mode = self.modal.order;
        filtered.sort_by(|a, b| {
            let kind_rank = |t: &CaptureTarget| match t.kind() {
                TargetKind::DefaultOutput | TargetKind::DefaultInput => 0u8,
                TargetKind::Device | TargetKind::InputDevice => 1u8,
                TargetKind::Application | TargetKind::Process => match t.active_audio() {
                    Some(true) => 0u8,
                    Some(false) => 1u8,
                    None => 2u8,
                },
            };
            let backend_rank = |t: &CaptureTarget| {
                t.backend()
                    .or_else(|| t.available_backends().first().copied())
                    .and_then(|b| active_backends.iter().position(|&cand| cand == b))
                    .unwrap_or(usize::MAX)
            };
            match order_mode {
                DiscoveryOrder::KindThenBackend => kind_rank(&a.target)
                    .cmp(&kind_rank(&b.target))
                    .then_with(|| a.target.name().cmp(&b.target.name()))
                    .then_with(|| a.target.pid().cmp(&b.target.pid()))
                    .then_with(|| backend_rank(&a.target).cmp(&backend_rank(&b.target))),
                DiscoveryOrder::BackendThenKind => backend_rank(&a.target)
                    .cmp(&backend_rank(&b.target))
                    .then_with(|| kind_rank(&a.target).cmp(&kind_rank(&b.target)))
                    .then_with(|| a.target.name().cmp(&b.target.name()))
                    .then_with(|| a.target.pid().cmp(&b.target.pid())),
            }
        });

        self.modal.candidates = filtered;
        if let Some(ref cur_id) = previous_cursor_id
            && let Some(new_idx) = self
                .modal
                .candidates
                .iter()
                .position(|c| c.target.id() == cur_id)
        {
            self.modal.cursor_idx = new_idx;
        } else if self.modal.cursor_idx >= self.modal.candidates.len() {
            self.modal.cursor_idx = self.modal.candidates.len().saturating_sub(1);
        }

        if let Some((hov_id, col, row)) = previous_hovered {
            self.modal.hovered_candidate = self
                .modal
                .candidates
                .iter()
                .position(|c| c.target.id() == &hov_id)
                .map(|new_idx| (new_idx, col, row));
        } else {
            self.modal.hovered_candidate = None;
        }
    }

    pub fn apply_discovery_event(&mut self, evt: &DiscoveryEvent) {
        self.modal.discovery_revision = evt.revision;
        if let DiscoveryEventKind::StateChanged(t) = &evt.kind {
            for staged in &mut self.staged_sources {
                if staged.target.id() == t.id() {
                    staged.target = staged
                        .target
                        .clone()
                        .with_name(t.name().cloned())
                        .with_active_audio(t.active_audio());
                }
            }
            for cand in &mut self.modal.candidates {
                if cand.target.id() == t.id() {
                    cand.target = cand
                        .target
                        .clone()
                        .with_name(t.name().cloned())
                        .with_active_audio(t.active_audio());
                }
            }
        }
        self.modal.last_event_note = match &evt.kind {
            DiscoveryEventKind::Added(t) => {
                format!(
                    "rev #{}: +Added {}",
                    evt.revision,
                    t.name().map_or_else(|| t.id().as_ref(), Arc::as_ref)
                )
            }
            DiscoveryEventKind::Removed(id) => {
                format!("rev #{}: -Removed {id}", evt.revision)
            }
            DiscoveryEventKind::StateChanged(t) => match t.kind() {
                TargetKind::Device | TargetKind::InputDevice => format!(
                    "rev #{}: Device renamed -> {}",
                    evt.revision,
                    t.name().map_or_else(|| t.id().as_ref(), Arc::as_ref)
                ),
                _ => {
                    let st = if t.active_audio() == Some(true) {
                        "PLAYING"
                    } else {
                        "IDLE"
                    };
                    format!(
                        "rev #{}: {} -> {st}",
                        evt.revision,
                        t.name().map_or_else(|| t.id().as_ref(), Arc::as_ref)
                    )
                }
            },
            DiscoveryEventKind::DefaultDeviceChanged { device_id } => {
                format!("rev #{}: Default output -> {device_id}", evt.revision)
            }
            DiscoveryEventKind::DefaultInputChanged { device_id } => {
                format!("rev #{}: Default mic -> {device_id}", evt.revision)
            }
            DiscoveryEventKind::ResnapshotRequired { minimum_revision } => {
                format!("rev #{}: Resnapshot (>= {minimum_revision})", evt.revision)
            }
            _ => format!("rev #{}", evt.revision),
        };
    }

    fn poll_modal_discovery_events(&mut self) {
        let mut events = Vec::new();
        if let Some(sub) = &self.modal.subscription {
            while let Some(evt) = sub.poll_event() {
                events.push(evt);
            }
        }
        if !events.is_empty() {
            let mut needs_structural_refresh = self.modal.playing_only;
            for evt in &events {
                if !matches!(evt.kind, DiscoveryEventKind::StateChanged(_)) {
                    needs_structural_refresh = true;
                }
                self.apply_discovery_event(evt);
            }
            if needs_structural_refresh {
                self.refresh_modal_candidates();
            }
        }
    }

    fn confirm_add_modal_selection(&mut self) {
        let mut added = 0usize;
        for cand in &self.modal.candidates {
            if cand.selected {
                self.staged_sources.push(StagedSource {
                    enabled: true,
                    target: cand.target.clone(),
                });
                added += 1;
            }
        }
        self.modal.open = false;
        self.modal.subscription = None;
        self.modal.search_focused = false;
        self.modal.hovered_candidate = None;
        if added > 0 {
            self.status_message = format!("Added {added} source(s) to staged mixing board");
            self.on_sources_or_config_modified();
        }
    }

    pub fn start_recording(&mut self) {
        self.playback.stop();
        self.error_banner = None;
        let take_id = self.next_take_id;
        self.next_take_id += 1;
        self.active_take_id = take_id;
        self.is_recording = true;
        self.live_frames = 0;
        self.live_bytes = 0;
        self.live_segments = 1;

        let summary = self.checked_targets_summary();
        let title = format!("Take #{take_id}");
        let req = self.build_capture_request();
        self.worker.send(WorkerCommand::StartRecording {
            take_id,
            title,
            targets_summary: summary,
            request: req,
            overflow: self.overflow_policy,
            synthetic: self.synthetic_mode,
        });
    }

    pub fn stop_recording(&mut self) {
        if self.is_recording {
            self.worker.send(WorkerCommand::StopRecording);
        }
    }

    pub fn play_or_pause_selected_take(&mut self) {
        if self.is_recording {
            self.error_banner =
                Some("Stop active recording before starting take playback".to_owned());
            return;
        }
        let Some(take) = self.takes.get(self.selected_take_idx).cloned() else {
            self.error_banner = Some("No recorded takes in memory yet".to_owned());
            return;
        };
        if self.playback_status.active && self.playback_status.take_id == Some(take.id) {
            self.playback.toggle_pause();
            self.status_message = if self.playback_status.paused {
                format!("Resumed playback of Take #{}", take.id)
            } else {
                format!("Paused Take #{}", take.id)
            };
        } else {
            self.error_banner = None;
            self.worker.send(WorkerCommand::SyncMonitor {
                enabled: false,
                targets_summary: self.checked_targets_summary(),
                request: None,
                overflow: self.overflow_policy,
                synthetic: self.synthetic_mode,
            });
            self.status_message = format!(
                "Playing Take #{} ({:.2}s, {} Hz, {}ch {:?}) from RAM",
                take.id,
                take.duration_secs(),
                take.format.rate(),
                take.format.channels(),
                take.format.sample()
            );
            self.playback.play(take);
        }
    }

    pub fn save_selected_take_to_disk(&mut self) {
        let Some(take_arc) = self.takes.get_mut(self.selected_take_idx) else {
            self.error_banner = Some("No recorded take selected to save".to_owned());
            return;
        };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let file_name = format!(
            "take_{:02}_{}_{}hz_{}ch.wav",
            take_arc.id,
            ts,
            take_arc.format.rate(),
            take_arc.format.channels()
        );
        let path = PathBuf::from("recordings").join(file_name);
        match save_wav_file(&path, &take_arc.format, &take_arc.raw_pcm_bytes) {
            Ok(()) => {
                let mut cloned = (**take_arc).clone();
                cloned.saved_path = Some(path.clone());
                *take_arc = Arc::new(cloned);
                self.error_banner = None;
                self.status_message = format!("Saved WAV to {}", path.display());
            }
            Err(err) => {
                self.error_banner = Some(format!("Failed to save WAV: {err}"));
            }
        }
    }
}

pub fn format_participation_badge(state: ParticipationState) -> &'static str {
    match state {
        ParticipationState::Starting => "STARTING",
        ParticipationState::Active => "ACTIVE",
        ParticipationState::Idle => "IDLE",
        ParticipationState::WaitingRelaunch => "WAIT_RELAUNCH",
        ParticipationState::Late => "LATE",
        ParticipationState::Exited => "EXITED",
        ParticipationState::Failed(_) => "FAILED",
    }
}
