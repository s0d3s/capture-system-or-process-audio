use capture_system_or_process_audio::{
    BackendId, Backends, ByteOrder, CapabilityError, CaptureManager, CapturePlanReport,
    CaptureRequest, CaptureTarget, Channel, Discovery, DiscoveryQuery, Format,
    InputDiagnosticSnapshot, Layout, MixedBlockMetadata, Overflow, PartialReason,
    ParticipationState, PermissionKind, PermissionReport, PlatformOs, ProcessTreeRange, SampleType,
    Stop, TargetCatalog, TargetKind, TimingRequirement, check_permission_kinds,
    check_permissions_with_backends, check_request_permissions, planner,
    request_for_capture_request, request_permission_kinds,
};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PASSTHROUGH_ACTIVE_HOLD: Duration = Duration::from_millis(500);
const DEFAULT_PERMISSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub fn default_stereo_f32_format() -> Format {
    Format::new(
        SampleType::F32,
        ByteOrder::Little,
        48_000,
        2,
        Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
    )
    .expect("valid canonical 48 kHz stereo F32 format")
}

pub fn available_os_backends() -> Vec<BackendId> {
    Backends::default().for_os(PlatformOs::current()).to_vec()
}

pub fn query_catalog(
    discovery: &Discovery,
    query: &DiscoveryQuery,
    use_cached: bool,
) -> Result<TargetCatalog, String> {
    if use_cached {
        discovery
            .service()
            .discover_query(query)
            .map_err(|e| format!("{e:?}"))
    } else {
        discovery.discover_query(query).map_err(|e| e.to_string())
    }
}

pub fn inspect_request_plan(request: &CaptureRequest) -> Result<CapturePlanReport, String> {
    request.inspect().map_err(|e| format!("{e:?}"))
}

pub fn inspect_request_permissions(request: &CaptureRequest) -> Result<PermissionReport, String> {
    check_request_permissions(request).map_err(|e| e.to_string())
}

pub fn inspect_permissions_for_optional_request(
    request: Option<&CaptureRequest>,
    backends: &Backends,
) -> Result<PermissionReport, String> {
    if let Some(req) = request {
        if let Ok(report) = check_request_permissions(req) {
            return Ok(report);
        }
        if let Ok(report) = check_permissions_with_backends(req.targets(), req.backends()) {
            return Ok(report);
        }
    }
    check_permission_kinds(&[], backends).map_err(|e| e.to_string())
}

pub fn target_required_permission_kind(
    target: &CaptureTarget,
    tree_range: ProcessTreeRange,
    backends: &Backends,
    os: PlatformOs,
) -> Option<PermissionKind> {
    planner::required_permission_kinds_for_os_with_filter(
        std::slice::from_ref(target),
        backends,
        tree_range,
        os,
        |_, _| true,
    )
    .ok()
    .and_then(|set| set.into_iter().next())
}

pub const fn domain_supports_open_settings(os: PlatformOs, kind: PermissionKind) -> bool {
    match os {
        PlatformOs::MacOs => true,
        PlatformOs::Windows => matches!(kind, PermissionKind::Microphone),
        PlatformOs::Linux => false,
    }
}

#[derive(Clone, Debug)]
pub enum PermissionJobKind {
    ForRequest {
        request: CaptureRequest,
        open_settings_if_denied: bool,
    },
    ForKinds {
        kinds: Vec<PermissionKind>,
        backends: Backends,
        open_settings_if_denied: bool,
    },
}

#[derive(Clone, Debug)]
pub struct PermissionJobOutcome {
    pub label: String,
    pub result: Result<PermissionReport, String>,
}

enum PermissionWorkerCmd {
    Run {
        label: String,
        job: Box<PermissionJobKind>,
    },
    Shutdown,
}

pub struct PermissionRunner {
    in_flight: Arc<AtomicBool>,
    cmd_tx: Option<Sender<PermissionWorkerCmd>>,
    evt_rx: Option<Receiver<PermissionJobOutcome>>,
    join: Option<JoinHandle<()>>,
    injected_outcomes: VecDeque<PermissionJobOutcome>,
}

impl Default for PermissionRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionRunner {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(AtomicBool::new(false)),
            cmd_tx: None,
            evt_rx: None,
            join: None,
            injected_outcomes: VecDeque::new(),
        }
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::Acquire)
    }

    fn ensure_worker(&mut self) {
        if self.cmd_tx.is_some() {
            return;
        }
        let (cmd_tx, cmd_rx) = mpsc::channel::<PermissionWorkerCmd>();
        let (evt_tx, evt_rx) = mpsc::channel::<PermissionJobOutcome>();
        let in_flight = Arc::clone(&self.in_flight);
        let join = thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    PermissionWorkerCmd::Run { label, job } => {
                        let result = match *job {
                            PermissionJobKind::ForRequest {
                                request,
                                open_settings_if_denied,
                            } => request_for_capture_request(
                                &request,
                                DEFAULT_PERMISSION_REQUEST_TIMEOUT,
                                open_settings_if_denied,
                            )
                            .map_err(|e| e.to_string()),
                            PermissionJobKind::ForKinds {
                                kinds,
                                backends,
                                open_settings_if_denied,
                            } => request_permission_kinds(
                                &kinds,
                                &backends,
                                DEFAULT_PERMISSION_REQUEST_TIMEOUT,
                                open_settings_if_denied,
                            )
                            .map_err(|e| e.to_string()),
                        };
                        in_flight.store(false, Ordering::Release);
                        let _ = evt_tx.send(PermissionJobOutcome { label, result });
                    }
                    PermissionWorkerCmd::Shutdown => break,
                }
            }
            in_flight.store(false, Ordering::Release);
        });
        self.cmd_tx = Some(cmd_tx);
        self.evt_rx = Some(evt_rx);
        self.join = Some(join);
    }

    pub fn submit(&mut self, label: String, job: PermissionJobKind) -> bool {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.ensure_worker();
        if let Some(tx) = &self.cmd_tx
            && tx
                .send(PermissionWorkerCmd::Run {
                    label,
                    job: Box::new(job),
                })
                .is_ok()
        {
            true
        } else {
            self.in_flight.store(false, Ordering::Release);
            false
        }
    }

    pub fn inject_outcome(&mut self, outcome: PermissionJobOutcome) {
        self.injected_outcomes.push_back(outcome);
    }

    pub fn try_recv(&mut self) -> Option<PermissionJobOutcome> {
        if let Some(outcome) = self.injected_outcomes.pop_front() {
            return Some(outcome);
        }
        self.evt_rx.as_ref()?.try_recv().ok()
    }
}

impl Drop for PermissionRunner {
    fn drop(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(PermissionWorkerCmd::Shutdown);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputTelemetryRow {
    pub snapshot: InputDiagnosticSnapshot,
    pub backend: Option<BackendId>,
    pub native_rate_hz: u32,
    pub native_channels: u16,
    pub mixer_rate_hz: u32,
    pub mixer_channels: u16,
    pub is_resampled: bool,
    pub is_identity_bypass: bool,
}

impl InputTelemetryRow {
    pub fn normalizer_badge(&self) -> String {
        if self.is_identity_bypass
            && !self.is_resampled
            && self.native_channels == self.mixer_channels
        {
            format!(
                "{}k->{}k BYPASS",
                self.native_rate_hz / 1000,
                self.mixer_rate_hz / 1000
            )
        } else {
            format!(
                "{}k {}ch->{}k {}ch SYNC",
                self.native_rate_hz / 1000,
                self.native_channels,
                self.mixer_rate_hz / 1000,
                self.mixer_channels
            )
        }
    }
}

pub struct ReadChunkOutcome {
    pub frames: usize,
    pub pcm_bytes: Vec<u8>,
    pub format: Format,
    pub metadata: MixedBlockMetadata,
    pub input_rows: Vec<InputTelemetryRow>,
    pub eof: bool,
}

pub enum PlatformCaptureSession {
    Live {
        manager: Box<CaptureManager>,
        request: Box<CaptureRequest>,
        format: Format,
        last_active_at: Option<Instant>,
    },
    Synthetic {
        format: Format,
        targets: Vec<CaptureTarget>,
        targets_count: usize,
        phase: f32,
        sequence: u64,
        stopped: bool,
    },
}

fn resolve_manager_output_format(manager: &CaptureManager, fallback: &Format) -> Format {
    manager
        .output_format()
        .or_else(|| manager.working_format())
        .map_or_else(|| fallback.clone(), |fmt| (**fmt).clone())
}

impl PlatformCaptureSession {
    pub fn open_native(request: CaptureRequest, overflow: Overflow) -> Result<Self, String> {
        let fallback_fmt = request
            .output_converter()
            .map_or_else(default_stereo_f32_format, |c| c.target_format.clone());
        let manager =
            CaptureManager::open_with_options(request.clone(), overflow, TimingRequirement::Raw)
                .map_err(|e| e.to_string())?;
        let actual_fmt = resolve_manager_output_format(&manager, &fallback_fmt);
        Ok(Self::Live {
            manager: Box::new(manager),
            request: Box::new(request),
            format: actual_fmt,
            last_active_at: None,
        })
    }

    pub fn open_synthetic(request: &CaptureRequest) -> Result<Self, String> {
        let format = request
            .output_converter()
            .map_or_else(default_stereo_f32_format, |c| c.target_format.clone());
        let targets = request.targets().to_vec();
        let targets_count = targets.len().max(1);
        Ok(Self::Synthetic {
            format,
            targets,
            targets_count,
            phase: 0.0,
            sequence: 0,
            stopped: false,
        })
    }

    pub fn format(&self) -> &Format {
        match self {
            Self::Live { format, .. } | Self::Synthetic { format, .. } => format,
        }
    }

    pub fn targets(&self) -> &[CaptureTarget] {
        match self {
            Self::Live { manager, .. } => manager.targets(),
            Self::Synthetic { targets, .. } => targets,
        }
    }

    /// Updates targets in place when possible; configuration or format changes may reopen the session.
    pub fn hot_swap(&mut self, request: CaptureRequest, overflow: Overflow) -> Result<(), String> {
        let desired_fallback = request
            .output_converter()
            .map_or_else(default_stereo_f32_format, |c| c.target_format.clone());

        match self {
            Self::Synthetic {
                format,
                targets,
                targets_count,
                sequence,
                ..
            } => {
                *format = desired_fallback;
                *targets = request.targets().to_vec();
                *targets_count = targets.len().max(1);
                *sequence = sequence.saturating_add(1);
                Ok(())
            }
            Self::Live {
                manager,
                request: prev_request,
                format,
                last_active_at,
            } => {
                let config_unchanged = prev_request.combine_mode() == request.combine_mode()
                    && prev_request.tree_strategy() == request.tree_strategy()
                    && prev_request.input_conversion() == request.input_conversion()
                    && prev_request.output_converter() == request.output_converter()
                    && prev_request.mixer_config() == request.mixer_config()
                    && prev_request.on_format_change() == request.on_format_change()
                    && prev_request.backends() == request.backends()
                    && prev_request.permission_policy() == request.permission_policy();

                if config_unchanged {
                    match manager.update_targets(request.targets().to_vec()) {
                        Ok(_) => {
                            *format = resolve_manager_output_format(manager, &desired_fallback);
                            **prev_request = request;
                            *last_active_at = None;
                            return Ok(());
                        }
                        Err(CapabilityError::FormatChangeRejected) => {}
                        Err(err) => return Err(err.to_string()),
                    }
                }

                let mut replacement = Self::open_native(request, overflow)?;
                std::mem::swap(self, &mut replacement);
                replacement.stop_and_cleanup(Stop::Drain);
                Ok(())
            }
        }
    }

    pub fn read_next(
        &mut self,
        requested_frames: usize,
        timeout: Duration,
    ) -> Result<ReadChunkOutcome, String> {
        match self {
            Self::Live {
                manager,
                format,
                last_active_at,
                ..
            } => {
                let mut res = manager
                    .read(requested_frames, timeout)
                    .map_err(|e| format!("{e:?}"))?;
                if res.reason == PartialReason::Boundary {
                    *format = resolve_manager_output_format(manager, format);
                    res = manager
                        .read(requested_frames, timeout)
                        .map_err(|e| format!("{e:?}"))?;
                }
                *format = resolve_manager_output_format(manager, format);

                let mut pcm_bytes = Vec::new();
                for slice in &res.chunks {
                    pcm_bytes.extend_from_slice(slice.bytes());
                }
                let eof = res.reason == PartialReason::Eof;
                if res.frames > 0 {
                    *last_active_at = Some(Instant::now());
                }
                let recently_active = !eof
                    && (res.frames > 0
                        || last_active_at.is_some_and(|t| t.elapsed() < PASSTHROUGH_ACTIVE_HOLD));

                let mut input_rows: Vec<InputTelemetryRow> = manager
                    .input_handles()
                    .iter()
                    .filter_map(|h| {
                        let snap = h.snapshot()?;
                        let nat = h.native_format();
                        let mix = h.mixer_format();
                        Some(InputTelemetryRow {
                            snapshot: snap,
                            backend: Some(h.backend()),
                            native_rate_hz: nat.rate(),
                            native_channels: nat.channels(),
                            mixer_rate_hz: mix.rate(),
                            mixer_channels: mix.channels(),
                            is_resampled: h.is_resampled(),
                            is_identity_bypass: h.is_identity_bypass(),
                        })
                    })
                    .collect();

                if input_rows.is_empty() {
                    let state = if eof {
                        ParticipationState::Exited
                    } else if recently_active {
                        ParticipationState::Active
                    } else {
                        ParticipationState::Idle
                    };
                    let plan_targets = &manager.plan().target_plans;
                    let work_rate = manager
                        .working_format()
                        .map_or_else(|| format.rate(), |w| w.rate());
                    let work_ch = manager
                        .working_format()
                        .map_or_else(|| format.channels(), |w| w.channels());
                    input_rows = manager
                        .targets()
                        .iter()
                        .enumerate()
                        .map(|(idx, t)| {
                            let resolved_backend = plan_targets
                                .get(idx)
                                .map(|tp| tp.resolved_backend)
                                .or_else(|| t.backend())
                                .or_else(|| Some(manager.selected_backend()));
                            InputTelemetryRow {
                                snapshot: InputDiagnosticSnapshot {
                                    input_index: idx,
                                    target_id: t.id().clone(),
                                    participation_state: state,
                                    queued_packets: usize::from(recently_active),
                                    dropped_frames: 0,
                                    clipped_samples: 0,
                                    nonfinite_samples: 0,
                                    drift_slips: 0,
                                },
                                backend: resolved_backend,
                                native_rate_hz: work_rate,
                                native_channels: work_ch,
                                mixer_rate_hz: work_rate,
                                mixer_channels: work_ch,
                                is_resampled: false,
                                is_identity_bypass: true,
                            }
                        })
                        .collect();
                }

                let metadata = manager.last_block_metadata().unwrap_or(MixedBlockMetadata {
                    membership_version: manager.segment_id(),
                    contributing_mask: u64::from(recently_active),
                    skew_estimate_ns: Some(0),
                    skew_uncertainty_ns: Some(0),
                    ..Default::default()
                });

                Ok(ReadChunkOutcome {
                    frames: res.frames,
                    pcm_bytes,
                    format: format.clone(),
                    metadata,
                    input_rows,
                    eof,
                })
            }
            Self::Synthetic {
                format,
                targets,
                targets_count,
                phase,
                sequence,
                stopped,
            } => {
                if *stopped {
                    return Ok(ReadChunkOutcome {
                        frames: 0,
                        pcm_bytes: Vec::new(),
                        format: format.clone(),
                        metadata: MixedBlockMetadata::default(),
                        input_rows: Vec::new(),
                        eof: true,
                    });
                }
                let frames = requested_frames.clamp(64, 480);
                let rate = format.rate() as f32;
                let channels = usize::from(format.channels());
                let mut pcm_bytes = Vec::with_capacity(frames * format.bytes_per_frame());
                for _ in 0..frames {
                    let s1 = (*phase * std::f32::consts::TAU * 440.0).sin() * 0.35;
                    let s2 = (*phase * std::f32::consts::TAU * 1320.0).sin() * 0.20;
                    let s3 = (*phase * std::f32::consts::TAU * 3520.0).sin() * 0.10;
                    let sample = (s1 + s2 + s3) * (*targets_count as f32).min(2.0) * 0.5;
                    *phase = (*phase + 1.0 / rate).fract();
                    for ch in 0..channels {
                        let ch_val = if ch % 2 == 0 { sample } else { -sample * 0.85 };
                        match format.sample() {
                            SampleType::F32 => pcm_bytes.extend_from_slice(&ch_val.to_le_bytes()),
                            SampleType::I16 => {
                                let q = (ch_val.clamp(-1.0, 1.0) * 32767.0) as i16;
                                pcm_bytes.extend_from_slice(&q.to_le_bytes());
                            }
                            SampleType::I24 => {
                                let q = (ch_val.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                                let le = q.to_le_bytes();
                                pcm_bytes.extend_from_slice(&le[0..3]);
                            }
                            SampleType::I32 => {
                                let q =
                                    (f64::from(ch_val.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32;
                                pcm_bytes.extend_from_slice(&q.to_le_bytes());
                            }
                        }
                    }
                }
                *sequence = sequence.saturating_add(1);
                let has_mix = *targets_count > 1;
                let mut rows = Vec::with_capacity(*targets_count);
                for idx in 0..*targets_count {
                    let tgt = targets.get(idx);
                    let is_mic = tgt.is_some_and(|t| {
                        matches!(t.kind(), TargetKind::DefaultInput | TargetKind::InputDevice)
                    });
                    let (nat_rate, nat_ch, resampled, bypass) = if is_mic && has_mix {
                        (16_000u32, 1u16, true, false)
                    } else {
                        (48_000u32, 2u16, false, true)
                    };
                    let target_id =
                        tgt.map_or_else(|| Arc::from(format!("synth:{idx}")), |t| t.id().clone());
                    rows.push(InputTelemetryRow {
                        snapshot: InputDiagnosticSnapshot {
                            input_index: idx,
                            target_id,
                            participation_state: ParticipationState::Active,
                            queued_packets: 1,
                            dropped_frames: 0,
                            clipped_samples: 0,
                            nonfinite_samples: 0,
                            drift_slips: 0,
                        },
                        backend: tgt.and_then(CaptureTarget::backend),
                        native_rate_hz: nat_rate,
                        native_channels: nat_ch,
                        mixer_rate_hz: 48_000,
                        mixer_channels: 2,
                        is_resampled: resampled,
                        is_identity_bypass: bypass,
                    });
                }
                let cross_clock = targets.iter().any(|t| {
                    matches!(t.kind(), TargetKind::DefaultInput | TargetKind::InputDevice)
                }) && targets.iter().any(|t| {
                    !matches!(t.kind(), TargetKind::DefaultInput | TargetKind::InputDevice)
                });
                Ok(ReadChunkOutcome {
                    frames,
                    pcm_bytes,
                    format: format.clone(),
                    metadata: MixedBlockMetadata {
                        membership_version: *sequence,
                        contributing_mask: (1u64 << (*targets_count).min(16)) - 1,
                        skew_estimate_ns: (!cross_clock).then_some(250_000),
                        skew_uncertainty_ns: (!cross_clock).then_some(100_000),
                        ..Default::default()
                    },
                    input_rows: rows,
                    eof: false,
                })
            }
        }
    }

    pub fn stop_and_cleanup(&mut self, mode: Stop) {
        match self {
            Self::Live { manager, .. } => {
                manager.stop(mode);
                let _ = manager.wait_cleanup(Duration::from_secs(2));
            }
            Self::Synthetic { stopped, .. } => {
                *stopped = true;
            }
        }
    }
}
