use crate::{
    CapacityError, StorageLimits,
    converter::{
        FrameReblocker, InputNormalizer, OutputConverter, resolve_mixer_working_format,
        validate_channel_mapping,
    },
    mixer::{CspaMixer, InputDiagnosticSnapshot, MixStepOutcome, MixerInputPacket},
    ordering::Failure,
    planner::CaptureRequest,
    reads::{PartialReason, ReadError, ReadResult, Slice},
    schema::{
        AggregateManagerBudgets, BackendId, CaptureTarget, Format, Header, InputConversionPolicy,
        MembershipDescriptor, MixedBlockMetadata, ParticipationState, PlanError, PlatformOs,
        ProcessIdentity, Reason, Stamp,
    },
    storage::{CopyStats, Pool},
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Invalid(CapacityError),
    Full,
}

#[derive(Debug)]
struct Budget {
    max_sessions: usize,
    byte_ceiling: usize,
    sessions: AtomicUsize,
    bytes: AtomicUsize,
}

#[derive(Clone, Debug)]
pub struct StorageBudget(Arc<Budget>);

impl StorageBudget {
    pub fn new(max_sessions: usize, byte_ceiling: usize) -> Result<Self, AdmissionError> {
        if max_sessions == 0 || byte_ceiling == 0 {
            return Err(AdmissionError::Invalid(CapacityError::Zero));
        }
        Ok(Self(Arc::new(Budget {
            max_sessions,
            byte_ceiling,
            sessions: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        })))
    }

    pub fn reserve(
        &self,
        limits: StorageLimits,
        metadata_bytes: usize,
    ) -> Result<StorageReservation, AdmissionError> {
        let bytes = limits
            .total_bytes()
            .checked_add(metadata_bytes)
            .ok_or(AdmissionError::Invalid(CapacityError::Overflow))?;
        if bytes > self.0.byte_ceiling {
            return Err(AdmissionError::Invalid(CapacityError::ExceedsByteCeiling));
        }
        self.0
            .sessions
            .fetch_update(SeqCst, SeqCst, |used| {
                used.checked_add(1)
                    .filter(|next| *next <= self.0.max_sessions)
            })
            .map_err(|_| AdmissionError::Full)?;
        if self
            .0
            .bytes
            .fetch_update(SeqCst, SeqCst, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.0.byte_ceiling)
            })
            .is_err()
        {
            self.0.sessions.fetch_sub(1, SeqCst);
            return Err(AdmissionError::Full);
        }
        Ok(StorageReservation(Arc::new(Reservation {
            budget: self.0.clone(),
            pcm_bytes: limits.total_bytes(),
            bytes,
            counts_session: true,
        })))
    }

    /// Charge auxiliary stage PCM + metadata bytes (such as `MultiInputManager`'s mixed `out_pool`)
    /// against `byte_ceiling` (`max_total_pcm_memory`) without consuming one of the input-stream
    /// `sessions` (`max_active_streams`) slots.
    pub fn reserve_auxiliary_bytes(
        &self,
        limits: StorageLimits,
        metadata_bytes: usize,
    ) -> Result<StorageReservation, AdmissionError> {
        let bytes = limits
            .total_bytes()
            .checked_add(metadata_bytes)
            .ok_or(AdmissionError::Invalid(CapacityError::Overflow))?;
        if bytes > self.0.byte_ceiling {
            return Err(AdmissionError::Invalid(CapacityError::ExceedsByteCeiling));
        }
        self.0
            .bytes
            .fetch_update(SeqCst, SeqCst, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.0.byte_ceiling)
            })
            .map_err(|_| AdmissionError::Full)?;
        Ok(StorageReservation(Arc::new(Reservation {
            budget: self.0.clone(),
            pcm_bytes: limits.total_bytes(),
            bytes,
            counts_session: false,
        })))
    }

    pub fn resident_sessions(&self) -> usize {
        self.0.sessions.load(SeqCst)
    }

    pub fn reserved_bytes(&self) -> usize {
        self.0.bytes.load(SeqCst)
    }
}

#[derive(Debug)]
struct Reservation {
    budget: Arc<Budget>,
    pcm_bytes: usize,
    bytes: usize,
    counts_session: bool,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes, SeqCst);
        if self.counts_session {
            self.budget.sessions.fetch_sub(1, SeqCst);
        }
    }
}

#[derive(Debug)]
pub struct StorageReservation(Arc<Reservation>);

impl StorageReservation {
    pub fn retain(&self) -> Self {
        Self(self.0.clone())
    }

    pub fn pcm_bytes(&self) -> usize {
        self.0.pcm_bytes
    }

    pub fn total_bytes(&self) -> usize {
        self.0.bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Complete,
    Limited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagerError {
    InvalidConfiguration,
    InvalidState,
    InvalidObservation,
    UnknownVisibility,
    Ambiguous,
    CandidateCapacity,
    EventCapacity,
    Capacity,
    Deadline,
    CleanupTimeout,
    Permission,
    Unsupported,
    OpenFailed,
    Exhausted,
    WorkingFormatChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryKind {
    SessionChanged,
    ConfigurationChanged,
    DeviceChanged,
    FormatChanged,
    Gap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionBoundary {
    pub session: u64,
    pub segment: u64,
    pub kind: BoundaryKind,
    pub format: Arc<Format>,
    pub device_id: Option<Arc<str>>,
    pub identity: Option<ProcessIdentity>,
}

impl From<PlanError> for ManagerError {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::InvalidTarget
            | PlanError::ReversedStrategyRange
            | PlanError::InvalidBackend => Self::InvalidConfiguration,
            PlanError::UnsupportedScope
            | PlanError::PreciseTimingUnqualified
            | PlanError::UnsupportedCapturePlan => Self::Unsupported,
            PlanError::PermissionDenied => Self::Permission,
            PlanError::BackendUnavailable | PlanError::NoSuchProcess | PlanError::PidReused => {
                Self::OpenFailed
            }
            PlanError::Capacity(_) => Self::Capacity,
        }
    }
}

/// Portable `NativeTreeMix` topology controller that deduplicates native source/link identities
/// while preserving per-target associations so retiring one overlapping target never removes
/// a shared native source still needed by another active target.
pub struct NativeTreeMixTopology {
    max_links: usize,
    source_to_targets: BTreeMap<Arc<str>, BTreeSet<usize>>,
    target_to_sources: BTreeMap<usize, BTreeSet<Arc<str>>>,
}

impl NativeTreeMixTopology {
    pub fn new(max_links: usize) -> Result<Self, PlanError> {
        if !(1..=256).contains(&max_links) {
            return Err(PlanError::Capacity(CapacityError::ExceedsByteCeiling));
        }
        Ok(Self {
            max_links,
            source_to_targets: BTreeMap::new(),
            target_to_sources: BTreeMap::new(),
        })
    }

    pub fn active_link_count(&self) -> usize {
        self.source_to_targets.len()
    }

    pub fn active_sources(&self) -> Vec<Arc<str>> {
        self.source_to_targets.keys().cloned().collect()
    }

    /// Attach `source_id` on behalf of `target_index`. Returns `Ok(true)` if a new native link
    /// was created, or `Ok(false)` if an existing link was reused for the overlapping target.
    pub fn attach_source(
        &mut self,
        target_index: usize,
        source_id: impl Into<Arc<str>>,
    ) -> Result<bool, ManagerError> {
        let source_id: Arc<str> = source_id.into();
        if let Some(owners) = self.source_to_targets.get_mut(&source_id) {
            owners.insert(target_index);
            self.target_to_sources
                .entry(target_index)
                .or_default()
                .insert(source_id);
            return Ok(false);
        }
        if self.source_to_targets.len() >= self.max_links {
            return Err(ManagerError::Capacity);
        }
        let mut owners = BTreeSet::new();
        owners.insert(target_index);
        self.source_to_targets.insert(source_id.clone(), owners);
        self.target_to_sources
            .entry(target_index)
            .or_default()
            .insert(source_id);
        Ok(true)
    }

    /// Retire `target_index` and return the list of native `source_id` links that have zero
    /// remaining target associations and must now be unlinked from the native graph.
    pub fn retire_target(&mut self, target_index: usize) -> Vec<Arc<str>> {
        let mut unlinked = Vec::new();
        let Some(sources) = self.target_to_sources.remove(&target_index) else {
            return unlinked;
        };
        for src in sources {
            let remove_link = if let Some(owners) = self.source_to_targets.get_mut(&src) {
                owners.remove(&target_index);
                owners.is_empty()
            } else {
                false
            };
            if remove_link {
                self.source_to_targets.remove(&src);
                unlinked.push(src);
            }
        }
        unlinked
    }

    /// Reconfigure target indices according to `mapping` (where `mapping[new_idx] == Some(old_idx)`
    /// retains sources from `old_idx`), returning any `source_id` links that no longer have any
    /// associated target and must be unlinked from the native graph.
    pub fn reconfigure_targets(&mut self, mapping: &[Option<usize>]) -> Vec<Arc<str>> {
        let mut new_target_to_sources: BTreeMap<usize, BTreeSet<Arc<str>>> = BTreeMap::new();
        let mut new_source_to_targets: BTreeMap<Arc<str>, BTreeSet<usize>> = BTreeMap::new();

        for (new_idx, mapped_old) in mapping.iter().enumerate() {
            if let Some(old_idx) = *mapped_old
                && let Some(sources) = self.target_to_sources.get(&old_idx)
            {
                new_target_to_sources.insert(new_idx, sources.clone());
                for src in sources {
                    new_source_to_targets
                        .entry(src.clone())
                        .or_default()
                        .insert(new_idx);
                }
            }
        }

        let mut unlinked = Vec::new();
        for src in self.source_to_targets.keys() {
            if !new_source_to_targets.contains_key(src) {
                unlinked.push(src.clone());
            }
        }

        self.target_to_sources = new_target_to_sources;
        self.source_to_targets = new_source_to_targets;
        unlinked
    }
}

#[derive(Debug)]
struct PendingOpenGuard {
    pending: Arc<AtomicUsize>,
}

impl Drop for PendingOpenGuard {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, SeqCst);
    }
}

#[derive(Debug)]
pub struct PendingOpenTicket {
    _guard: PendingOpenGuard,
}

/// Status-only diagnostic input handle. Exposes read-only per-input status and metrics
/// without competing raw PCM read methods or tee buffers.
#[derive(Clone)]
pub struct InputStatusHandle {
    target_index: usize,
    target_id: Arc<str>,
    backend: BackendId,
    native_format: Arc<Format>,
    mixer_format: Arc<Format>,
    is_resampled: bool,
    identity_bypass: bool,
    shared: Arc<Mutex<Vec<InputDiagnosticSnapshot>>>,
}

impl InputStatusHandle {
    pub fn target_index(&self) -> usize {
        self.target_index
    }

    pub fn target_id(&self) -> &Arc<str> {
        &self.target_id
    }

    pub fn backend(&self) -> BackendId {
        self.backend
    }

    pub fn format(&self) -> &Arc<Format> {
        &self.native_format
    }

    pub fn native_format(&self) -> &Arc<Format> {
        &self.native_format
    }

    pub fn mixer_format(&self) -> &Arc<Format> {
        &self.mixer_format
    }

    pub fn is_resampled(&self) -> bool {
        self.is_resampled
    }

    pub fn is_identity_bypass(&self) -> bool {
        self.identity_bypass
    }

    pub fn snapshot(&self) -> Option<InputDiagnosticSnapshot> {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(self.target_index)
            .cloned()
    }
}

struct InputPipelineSlot {
    normalizer: InputNormalizer,
    reblocker: FrameReblocker,
    norm_scratch: Vec<f32>,
    primary_scratch: Vec<f32>,
    shifted_scratch: Vec<f32>,
    out_block_scratch: Vec<f32>,
    last_block: Vec<f32>,
    has_last_block: bool,
    nominal_cushion_frames: Option<usize>,
}

impl InputPipelineSlot {
    fn new(
        input_format: Format,
        working_format: Format,
        block_frames: usize,
        max_input_frames_per_call: usize,
    ) -> Result<Self, Reason> {
        let normalizer = InputNormalizer::new(
            input_format,
            working_format.clone(),
            max_input_frames_per_call,
        )?;
        let out_ch = usize::from(working_format.channels());
        let max_norm_out = normalizer.max_output_frames(max_input_frames_per_call);
        let reblock_cap = max_norm_out + block_frames * 8;
        let reblocker = FrameReblocker::new(out_ch, block_frames, reblock_cap)?;
        let block_samples = block_frames * out_ch;
        Ok(Self {
            normalizer,
            reblocker,
            norm_scratch: Vec::with_capacity(max_norm_out * out_ch),
            primary_scratch: vec![0.0_f32; block_samples],
            shifted_scratch: vec![0.0_f32; block_samples],
            out_block_scratch: vec![0.0_f32; block_samples],
            last_block: vec![0.0_f32; block_samples],
            has_last_block: false,
            nominal_cushion_frames: None,
        })
    }
}

/// Compute the canonical `10 ms` mixer block size clamped to `64..=max_frames_per_block`.
#[inline]
pub fn default_block_frames_for_rate(rate_hz: u32, max_frames_per_block: usize) -> usize {
    let cap = max_frames_per_block.max(64);
    ((rate_hz / 100) as usize).clamp(64, cap)
}

struct MixedOutputBufferState {
    out_pool: Pool,
    out_bytes_scratch: Vec<u8>,
    pending_out_slice: Option<(Slice, MixedBlockMetadata)>,
    pending_out_bytes: Option<(usize, usize, MixedBlockMetadata)>,
    sequence: u64,
}

/// Multi-input `Manager` orchestrating `CspaMixer`, per-input `InputNormalizer + FrameReblocker`,
/// optional `OutputConverter`, `StorageBudget`-backed `out_pool` + remainder slicing, aggregate
/// budgets, caller-owned overlap, and status-only diagnostic input handles.
pub struct MultiInputManager {
    request: CaptureRequest,
    working_format: Arc<Format>,
    output_format: Arc<Format>,
    block_frames: usize,
    max_frames_per_block: usize,
    storage_budget: StorageBudget,
    pending_opens: Arc<AtomicUsize>,
    topology: Mutex<NativeTreeMixTopology>,
    pipelines: Mutex<Vec<Option<InputPipelineSlot>>>,
    mixer: Mutex<CspaMixer>,
    converter: Mutex<Option<OutputConverter>>,
    diagnostics: Arc<Mutex<Vec<InputDiagnosticSnapshot>>>,
    input_handles: Vec<InputStatusHandle>,
    mix_scratch: Mutex<Vec<f32>>,
    raw_bytes_scratch: Mutex<Vec<u8>>,
    output_state: Mutex<MixedOutputBufferState>,
    last_metadata_seq: AtomicU64,
    last_metadata: Mutex<Option<MixedBlockMetadata>>,
    read_lock: Mutex<()>,
}

impl MultiInputManager {
    pub fn new(
        request: CaptureRequest,
        native_format: Arc<Format>,
        max_frames_per_block: usize,
    ) -> Result<Self, ManagerError> {
        let default_backend = request.backends.primary_for_os(PlatformOs::current());
        let per_input: Vec<(BackendId, Arc<Format>)> = request
            .targets
            .iter()
            .map(|t| (t.backend.unwrap_or(default_backend), native_format.clone()))
            .collect();
        Self::new_with_block_frames(
            request,
            per_input,
            max_frames_per_block,
            max_frames_per_block,
            None,
        )
    }

    pub fn new_heterogeneous(
        request: CaptureRequest,
        per_input: Vec<(BackendId, Arc<Format>)>,
        block_frames: usize,
        max_input_frames_per_call: usize,
    ) -> Result<Self, ManagerError> {
        Self::new_with_block_frames(
            request,
            per_input,
            block_frames,
            max_input_frames_per_call,
            None,
        )
    }

    pub fn new_heterogeneous_with_budget(
        request: CaptureRequest,
        per_input: Vec<(BackendId, Arc<Format>)>,
        block_frames: usize,
        max_input_frames_per_call: usize,
        existing_budget: StorageBudget,
    ) -> Result<Self, ManagerError> {
        Self::new_with_block_frames(
            request,
            per_input,
            block_frames,
            max_input_frames_per_call,
            Some(existing_budget),
        )
    }

    fn new_with_block_frames(
        request: CaptureRequest,
        per_input: Vec<(BackendId, Arc<Format>)>,
        block_frames: usize,
        max_frames_per_block: usize,
        existing_budget: Option<StorageBudget>,
    ) -> Result<Self, ManagerError> {
        request
            .budgets
            .validate()
            .map_err(|_| ManagerError::Capacity)?;
        if request.targets.is_empty()
            || per_input.len() != request.targets.len()
            || block_frames == 0
            || max_frames_per_block == 0
            || block_frames > max_frames_per_block
        {
            return Err(ManagerError::InvalidConfiguration);
        }
        if request.targets.len() > request.budgets.max_targets
            || request.targets.len() > request.budgets.max_active_streams
        {
            return Err(ManagerError::Capacity);
        }

        let raw_formats: Vec<Format> = per_input.iter().map(|(_, f)| (**f).clone()).collect();
        let all_identical_f32 = raw_formats
            .iter()
            .all(|f| f == &raw_formats[0] && f.sample() == crate::schema::SampleType::F32);
        let working_format = if all_identical_f32 {
            Arc::new(raw_formats[0].clone())
        } else {
            let (wf, _) = resolve_mixer_working_format(
                &raw_formats,
                request.output_converter.as_ref(),
                request.input_conversion,
            )
            .map_err(ManagerError::from)?;
            Arc::new(wf)
        };

        let storage_budget = match existing_budget {
            Some(budget) => budget,
            None => StorageBudget::new(
                request.budgets.max_active_streams,
                request.budgets.max_total_pcm_memory,
            )
            .map_err(|_| ManagerError::Capacity)?,
        };
        let topology = NativeTreeMixTopology::new(request.budgets.max_native_graph_links)
            .map_err(|_| ManagerError::Capacity)?;
        let mixer = CspaMixer::new_with_max_targets(
            working_format.clone(),
            &request.targets,
            request.mixer_config,
            max_frames_per_block,
            request.budgets.max_targets,
        )
        .map_err(ManagerError::from)?;
        let converter = match &request.output_converter {
            Some(cfg) => Some(
                OutputConverter::new((*working_format).clone(), cfg.clone(), max_frames_per_block)
                    .map_err(|_| ManagerError::Unsupported)?,
            ),
            None => None,
        };

        let output_format = Arc::new(
            request
                .output_converter
                .as_ref()
                .map_or_else(|| (*working_format).clone(), |c| c.target_format.clone()),
        );
        let out_stride = output_format.bytes_per_frame();
        let max_out_frames = match &converter {
            Some(_) => {
                ((max_frames_per_block as u64)
                    .saturating_mul(u64::from(output_format.rate()))
                    .div_ceil(u64::from(working_format.rate().max(1))) as usize)
                    + 64
            }
            None => max_frames_per_block,
        };
        let out_pool_frames = max_out_frames.max(max_frames_per_block);
        let out_ceiling = request
            .mixer_config
            .output_queue_slots
            .checked_mul(out_pool_frames)
            .and_then(|f| f.checked_mul(out_stride))
            .ok_or(ManagerError::Capacity)?;
        let out_limits = StorageLimits::new(
            request.mixer_config.output_queue_slots,
            out_pool_frames,
            out_stride,
            out_ceiling,
        )
        .map_err(|_| ManagerError::Capacity)?;
        let out_res = storage_budget
            .reserve_auxiliary_bytes(out_limits, 0)
            .map_err(|_| ManagerError::Capacity)?;
        let out_pool =
            Pool::new_reserved(out_limits, out_pool_frames, output_format.clone(), out_res)
                .map_err(|_| ManagerError::Capacity)?;

        let mut pipelines = Vec::with_capacity(request.budgets.max_targets);
        let mut diag_snapshots = Vec::with_capacity(request.budgets.max_targets);
        mixer.write_input_diagnostics(&mut diag_snapshots);
        let diagnostics = Arc::new(Mutex::new(diag_snapshots));
        let mut input_handles = Vec::with_capacity(request.targets.len());

        for (idx, (t, (backend, in_fmt))) in request.targets.iter().zip(per_input).enumerate() {
            let pipe_opt = if matches!(in_fmt.layout(), crate::schema::Layout::Known(_)) {
                Some(
                    InputPipelineSlot::new(
                        (*in_fmt).clone(),
                        (*working_format).clone(),
                        block_frames,
                        max_frames_per_block,
                    )
                    .map_err(|_| ManagerError::Unsupported)?,
                )
            } else {
                None
            };
            let is_resampled = in_fmt.rate() != working_format.rate();
            let identity_bypass = pipe_opt.as_ref().map_or(in_fmt == working_format, |p| {
                p.normalizer.is_identity_bypass()
            });
            pipelines.push(pipe_opt);
            input_handles.push(InputStatusHandle {
                target_index: idx,
                target_id: t.id.clone(),
                backend,
                native_format: in_fmt,
                mixer_format: working_format.clone(),
                is_resampled,
                identity_bypass,
                shared: diagnostics.clone(),
            });
        }

        let ch = usize::from(working_format.channels());
        Ok(Self {
            request,
            working_format,
            output_format,
            block_frames,
            max_frames_per_block,
            storage_budget,
            pending_opens: Arc::new(AtomicUsize::new(0)),
            topology: Mutex::new(topology),
            pipelines: Mutex::new(pipelines),
            mixer: Mutex::new(mixer),
            converter: Mutex::new(converter),
            diagnostics,
            input_handles,
            mix_scratch: Mutex::new(vec![0.0_f32; max_frames_per_block * ch]),
            raw_bytes_scratch: Mutex::new(Vec::with_capacity(max_frames_per_block * ch * 4)),
            output_state: Mutex::new(MixedOutputBufferState {
                out_pool,
                out_bytes_scratch: Vec::with_capacity(out_pool_frames * out_stride),
                pending_out_slice: None,
                pending_out_bytes: None,
                sequence: 0,
            }),
            last_metadata_seq: AtomicU64::new(0),
            last_metadata: Mutex::new(None),
            read_lock: Mutex::new(()),
        })
    }

    pub fn working_format(&self) -> &Arc<Format> {
        &self.working_format
    }

    pub fn output_format(&self) -> &Arc<Format> {
        &self.output_format
    }

    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    pub fn storage_budget(&self) -> &StorageBudget {
        &self.storage_budget
    }

    pub fn budgets(&self) -> &AggregateManagerBudgets {
        &self.request.budgets
    }

    pub fn targets(&self) -> &[CaptureTarget] {
        &self.request.targets
    }

    pub fn last_block_metadata(&self) -> Option<MixedBlockMetadata> {
        *self.last_metadata.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn clear_pending_output(&self) {
        let mut out_st = self.output_state.lock().unwrap_or_else(|e| e.into_inner());
        out_st.pending_out_slice = None;
        out_st.pending_out_bytes = None;
    }

    pub fn has_pending_output(&self) -> bool {
        let out_st = self.output_state.lock().unwrap_or_else(|e| e.into_inner());
        out_st.pending_out_slice.is_some() || out_st.pending_out_bytes.is_some()
    }

    /// Reconfigure active targets in `CspaMixer`, preserving queued packets and
    /// participation state for retained targets while initializing newly added targets in `Starting`.
    /// Returns the old slot index for each retained new slot, or `None` for newly added slots.
    pub fn reconfigure_targets(
        &mut self,
        new_targets: Vec<CaptureTarget>,
    ) -> Result<Vec<Option<usize>>, ManagerError> {
        let default_backend = self.request.backends.primary_for_os(PlatformOs::current());
        let per_input: Vec<(BackendId, Arc<Format>)> = new_targets
            .iter()
            .map(|t| {
                (
                    t.backend.unwrap_or(default_backend),
                    self.working_format.clone(),
                )
            })
            .collect();
        self.reconfigure_targets_with_formats(new_targets, per_input)
    }

    pub fn reconfigure_targets_with_formats(
        &mut self,
        new_targets: Vec<CaptureTarget>,
        per_input: Vec<(BackendId, Arc<Format>)>,
    ) -> Result<Vec<Option<usize>>, ManagerError> {
        if new_targets.is_empty() || per_input.len() != new_targets.len() {
            return Err(ManagerError::InvalidConfiguration);
        }
        if new_targets.len() > self.request.budgets.max_targets
            || new_targets.len() > self.request.budgets.max_active_streams
        {
            return Err(ManagerError::Capacity);
        }

        // Reject unsupported formats before mutating the mixer or pipelines.
        if self.request.input_conversion == InputConversionPolicy::StrictNoResample
            && per_input
                .iter()
                .any(|(_, fmt)| fmt.rate() != self.working_format.rate())
        {
            return Err(ManagerError::Unsupported);
        }

        let raw_formats: Vec<Format> = per_input.iter().map(|(_, f)| (**f).clone()).collect();
        let all_identical_f32 = raw_formats
            .iter()
            .all(|f| f == &raw_formats[0] && f.sample() == crate::schema::SampleType::F32);
        let desired_working = if all_identical_f32 {
            raw_formats[0].clone()
        } else {
            let (wf, _) = resolve_mixer_working_format(
                &raw_formats,
                self.request.output_converter.as_ref(),
                self.request.input_conversion,
            )
            .map_err(ManagerError::from)?;
            wf
        };

        if desired_working.rate() > self.working_format.rate()
            || desired_working.channels() > self.working_format.channels()
        {
            return Err(ManagerError::WorkingFormatChanged);
        }
        for (_, fmt) in &per_input {
            if matches!(fmt.layout(), crate::schema::Layout::Known(_)) {
                validate_channel_mapping(fmt, &self.working_format)
                    .map_err(|_| ManagerError::WorkingFormatChanged)?;
            } else if fmt.channels() != self.working_format.channels()
                || fmt.rate() != self.working_format.rate()
            {
                return Err(ManagerError::Unsupported);
            }
        }

        let mapping = {
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            let map_slice = mixer
                .reconfigure_targets(&new_targets)
                .map_err(ManagerError::from)?;
            let mapped = map_slice.to_vec();
            mixer.write_input_diagnostics(
                &mut self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()),
            );
            mapped
        };

        {
            let mut old_pipes = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
            let mut next_pipes = Vec::with_capacity(self.request.budgets.max_targets);
            for (idx, &mapped_old) in mapping.iter().enumerate() {
                let (_, ref fmt) = per_input[idx];
                if let Some(old_idx) = mapped_old
                    && let Some(existing_pipe) = old_pipes.get_mut(old_idx).and_then(Option::take)
                    && existing_pipe.normalizer.input_format() == fmt.as_ref()
                {
                    next_pipes.push(Some(existing_pipe));
                } else if matches!(fmt.layout(), crate::schema::Layout::Known(_)) {
                    next_pipes.push(Some(
                        InputPipelineSlot::new(
                            (**fmt).clone(),
                            (*self.working_format).clone(),
                            self.block_frames,
                            self.max_frames_per_block,
                        )
                        .map_err(|_| ManagerError::Unsupported)?,
                    ));
                } else {
                    next_pipes.push(None);
                }
            }
            *old_pipes = next_pipes;
        }

        let pipes_guard = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
        self.input_handles = new_targets
            .iter()
            .zip(per_input)
            .enumerate()
            .map(|(idx, (t, (backend, in_fmt)))| {
                let is_resampled = in_fmt.rate() != self.working_format.rate();
                let identity_bypass = pipes_guard
                    .get(idx)
                    .and_then(Option::as_ref)
                    .map_or(in_fmt == self.working_format, |p| {
                        p.normalizer.is_identity_bypass()
                    });
                InputStatusHandle {
                    target_index: idx,
                    target_id: t.id.clone(),
                    backend,
                    native_format: in_fmt,
                    mixer_format: self.working_format.clone(),
                    is_resampled,
                    identity_bypass,
                    shared: self.diagnostics.clone(),
                }
            })
            .collect();
        drop(pipes_guard);
        self.request.targets = new_targets;
        Ok(mapping)
    }

    pub fn acquire_pending_open(&self) -> Result<PendingOpenTicket, ManagerError> {
        let max_pending = self.request.budgets.max_pending_opens;
        self.pending_opens
            .fetch_update(SeqCst, SeqCst, |used| {
                used.checked_add(1).filter(|next| *next <= max_pending)
            })
            .map_err(|_| ManagerError::Capacity)?;
        Ok(PendingOpenTicket {
            _guard: PendingOpenGuard {
                pending: self.pending_opens.clone(),
            },
        })
    }

    pub fn input_handles(&self) -> &[InputStatusHandle] {
        &self.input_handles
    }

    pub fn membership_descriptor(&self) -> MembershipDescriptor {
        self.mixer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .membership_descriptor()
    }

    pub fn attach_topology_source(
        &self,
        target_index: usize,
        source_id: impl Into<Arc<str>>,
    ) -> Result<bool, ManagerError> {
        self.topology
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .attach_source(target_index, source_id)
    }

    pub fn retire_topology_target(&self, target_index: usize) -> Vec<Arc<str>> {
        self.topology
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retire_target(target_index)
    }

    /// Normalize raw input PCM bytes (`I16`/`I24`/`I32`/`F32`, `Mono`/`Stereo`, any supported rate),
    /// reblock into exact `block_frames` chunks, apply sample-exact cross-clock slip/repeat drift
    /// compensation, and enqueue into `CspaMixer` with `0` steady-state heap allocations.
    pub fn push_raw_input_bytes(
        &self,
        input_index: usize,
        raw_bytes: &[u8],
        timestamp: Option<Stamp>,
        now_ns: u64,
    ) -> Result<usize, Reason> {
        let mut pipes = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
        let pipe = pipes
            .get_mut(input_index)
            .and_then(Option::as_mut)
            .ok_or(Reason::InvalidFormat)?;

        pipe.normalizer
            .normalize_into(raw_bytes, &mut pipe.norm_scratch)?;
        pipe.reblocker.push_samples(&pipe.norm_scratch)?;

        let rate_hz = u64::from(self.working_format.rate().max(1));
        let ch = usize::from(self.working_format.channels());
        let block_frames = self.block_frames;
        let threshold_frames = ((u128::from(self.request.mixer_config.drift_threshold_ns)
            * u128::from(rate_hz))
            / 1_000_000_000)
            .max(1) as usize;
        let threshold_frames = threshold_frames.min(block_frames.saturating_sub(1).max(1));

        let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
        let mut pushed_blocks = 0usize;

        loop {
            let queued_pkts = mixer.queued_packets(input_index);
            let total_avail = pipe.reblocker.buffered_frames() + queued_pkts * block_frames;
            let nominal = *pipe
                .nominal_cushion_frames
                .get_or_insert_with(|| total_avail.max(block_frames));

            // Positive drift check (producer faster than consumer)
            if total_avail >= nominal + threshold_frames
                && pipe.reblocker.buffered_frames() >= block_frames + threshold_frames
            {
                let lead_frames = total_avail - nominal;
                let obs_skew_ns = ((lead_frames as u64) * 1_000_000_000) / rate_hz;
                pipe.reblocker.peek_into(0, &mut pipe.primary_scratch)?;
                pipe.reblocker
                    .peek_into(threshold_frames, &mut pipe.shifted_scratch)?;
                let slipped = mixer.apply_drift_correction(
                    input_index,
                    obs_skew_ns,
                    now_ns,
                    &pipe.primary_scratch,
                    &pipe.shifted_scratch,
                    &mut pipe.out_block_scratch,
                )?;
                if slipped {
                    pipe.reblocker
                        .discard_frames(block_frames + threshold_frames);
                    pipe.last_block.copy_from_slice(&pipe.out_block_scratch);
                    pipe.has_last_block = true;
                    mixer.push_input_samples(
                        input_index,
                        &pipe.out_block_scratch,
                        block_frames,
                        timestamp,
                        now_ns,
                    )?;
                    pushed_blocks += 1;
                    continue;
                }
            }

