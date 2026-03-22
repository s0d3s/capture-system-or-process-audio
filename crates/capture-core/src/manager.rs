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

            // Negative drift check (producer slower than consumer while continuous audio is flowing)
            if pipe.has_last_block
                && queued_pkts == 0
                && pipe.reblocker.buffered_frames() < block_frames
                && pipe.reblocker.buffered_frames() + threshold_frames >= block_frames
            {
                let deficit_frames = nominal.saturating_sub(total_avail).max(threshold_frames);
                let obs_skew_ns = ((deficit_frames as u64) * 1_000_000_000) / rate_hz;
                let consume_new = block_frames - threshold_frames;
                let tail_start = consume_new * ch;
                pipe.primary_scratch.copy_from_slice(&pipe.last_block);
                pipe.shifted_scratch[..threshold_frames * ch]
                    .copy_from_slice(&pipe.last_block[tail_start..]);
                pipe.reblocker
                    .peek_into(0, &mut pipe.shifted_scratch[threshold_frames * ch..])?;
                let repeated = mixer.apply_drift_correction(
                    input_index,
                    obs_skew_ns,
                    now_ns,
                    &pipe.primary_scratch,
                    &pipe.shifted_scratch,
                    &mut pipe.out_block_scratch,
                )?;
                if repeated {
                    pipe.reblocker.discard_frames(consume_new);
                    pipe.last_block.copy_from_slice(&pipe.out_block_scratch);
                    mixer.push_input_samples(
                        input_index,
                        &pipe.out_block_scratch,
                        block_frames,
                        timestamp,
                        now_ns,
                    )?;
                    pushed_blocks += 1;
                    break;
                }
            }

            if !pipe.reblocker.pop_block_into(&mut pipe.out_block_scratch)? {
                break;
            }
            if total_avail <= nominal + threshold_frames / 2
                && nominal <= total_avail + threshold_frames / 2
            {
                let clear_skew = self
                    .request
                    .mixer_config
                    .drift_threshold_ns
                    .saturating_sub(self.request.mixer_config.drift_hysteresis_ns);
                let _ = mixer.apply_drift_correction(
                    input_index,
                    clear_skew,
                    now_ns,
                    &pipe.out_block_scratch,
                    &pipe.out_block_scratch,
                    &mut pipe.primary_scratch,
                );
            }
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
        }

        mixer.write_input_diagnostics(
            &mut self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()),
        );
        Ok(pushed_blocks)
    }

    pub fn push_input_packet(
        &self,
        input_index: usize,
        packet: MixerInputPacket,
    ) -> Result<(), Reason> {
        let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
        mixer.push_input_packet(input_index, packet)?;
        mixer.write_input_diagnostics(
            &mut self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()),
        );
        Ok(())
    }

    pub fn set_input_state(
        &self,
        input_index: usize,
        state: ParticipationState,
    ) -> Result<(), Reason> {
        let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
        mixer.set_input_state(input_index, state)?;
        mixer.write_input_diagnostics(
            &mut self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()),
        );
        Ok(())
    }

    fn poll_mix_bytes_unlocked(
        &self,
        frames: usize,
        now_ns: u64,
        oldest_wait_ns: u64,
        out_bytes: &mut Vec<u8>,
    ) -> Result<(MixStepOutcome, MixedBlockMetadata), ReadError> {
        let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
        let ch = usize::from(mixer.format().channels());
        let total_samples = frames * ch;
        let mut mix_scratch = self.mix_scratch.lock().unwrap_or_else(|e| e.into_inner());
        if mix_scratch.len() < total_samples {
            mix_scratch.resize(total_samples, 0.0);
        }
        let (outcome, meta) = mixer
            .mix_block_into(
                frames,
                now_ns,
                oldest_wait_ns,
                &mut mix_scratch[..total_samples],
            )
            .map_err(|_| ReadError::InvalidDestination)?;
        mixer.write_input_diagnostics(
            &mut self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()),
        );

        let final_outcome = if let MixStepOutcome::Produced(_) = outcome {
            let mut conv_opt = self.converter.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(conv) = conv_opt.as_mut() {
                let mut raw_bytes = self
                    .raw_bytes_scratch
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                raw_bytes.clear();
                for &sample in &mix_scratch[..total_samples] {
                    raw_bytes.extend_from_slice(&sample.to_le_bytes());
                }
                let out_frames = conv
                    .convert_into(&raw_bytes, out_bytes)
                    .map_err(|_| ReadError::InvalidDestination)?;
                MixStepOutcome::Produced(out_frames)
            } else {
                out_bytes.clear();
                for &sample in &mix_scratch[..total_samples] {
                    out_bytes.extend_from_slice(&sample.to_le_bytes());
                }
                outcome
            }
        } else {
            outcome
        };

        *self.last_metadata.lock().unwrap_or_else(|e| e.into_inner()) = Some(meta);
        self.last_metadata_seq.fetch_add(1, SeqCst);
        Ok((final_outcome, meta))
    }

    /// Pull one mixed (and optionally converted) block into `out_bytes` under exclusive one-reader arbitration.
    pub fn poll_mix_bytes(
        &self,
        frames: usize,
        now_ns: u64,
        oldest_wait_ns: u64,
        out_bytes: &mut Vec<u8>,
    ) -> Result<(MixStepOutcome, MixedBlockMetadata), ReadError> {
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;
        self.poll_mix_bytes_unlocked(frames, now_ns, oldest_wait_ns, out_bytes)
    }

    /// Drain any previously buffered remainder frames (`pending_out_slice` or `pending_out_bytes`)
    /// into an owned `ReadResult` using `self.out_pool` (`StorageBudget`-accounted).
    pub fn try_pop_pending_owned(
        &self,
        requested_frames: usize,
    ) -> Result<Option<(ReadResult, MixedBlockMetadata)>, ReadError> {
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;
        self.try_pop_pending_owned_unlocked(requested_frames.max(1))
    }

    fn try_pop_pending_owned_unlocked(
        &self,
        req_frames: usize,
    ) -> Result<Option<(ReadResult, MixedBlockMetadata)>, ReadError> {
        let mut out_st = self.output_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((mut pending_slice, meta)) = out_st.pending_out_slice.take() {
            let emit_frames = pending_slice.frames.min(req_frames);
            let emit_slice = Slice {
                block: pending_slice.block.clone(),
                first: pending_slice.first,
                frames: emit_frames,
            };
            if emit_frames < pending_slice.frames {
                pending_slice.first += emit_frames;
                pending_slice.frames -= emit_frames;
                out_st.pending_out_slice = Some((pending_slice, meta));
            }
            drop(out_st);
            *self.last_metadata.lock().unwrap_or_else(|e| e.into_inner()) = Some(meta);
            let emit_bytes = emit_slice.bytes().len() as u64;
            return Ok(Some((
                ReadResult {
                    frames: emit_frames,
                    chunks: vec![emit_slice],
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Complete,
                    control: None,
                    copies: CopyStats {
                        bytes: emit_bytes,
                        destinations: 1,
                        ..Default::default()
                    },
                },
                meta,
            )));
        }

        if let Some((byte_off, rem_frames, meta)) = out_st.pending_out_bytes.take() {
            let stride = out_st.out_pool.format().bytes_per_frame();
            let emit_frames = rem_frames.min(req_frames);
            let emit_bytes = emit_frames * stride;
            out_st.sequence = out_st.sequence.saturating_add(1);
            let hdr = Header {
                sequence: out_st.sequence,
                segment: meta.membership_version,
                generation: 1,
                frames: emit_frames,
                sample_position: None,
                timestamp: None,
            };
            let MixedOutputBufferState {
                ref mut out_pool,
                ref out_bytes_scratch,
                ref mut pending_out_bytes,
                ..
            } = *out_st;
            let block = out_pool
                .offer(&out_bytes_scratch[byte_off..byte_off + emit_bytes], hdr)
                .map_err(|_| ReadError::InvalidDestination)?;
            if emit_frames < rem_frames {
                *pending_out_bytes = Some((byte_off + emit_bytes, rem_frames - emit_frames, meta));
            }
            drop(out_st);
            *self.last_metadata.lock().unwrap_or_else(|e| e.into_inner()) = Some(meta);
            let slice = Slice {
                block,
                first: 0,
                frames: emit_frames,
            };
            return Ok(Some((
                ReadResult {
                    frames: emit_frames,
                    chunks: vec![slice],
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Complete,
                    control: None,
                    copies: CopyStats {
                        bytes: emit_bytes as u64,
                        destinations: 1,
                        ..Default::default()
                    },
                },
                meta,
            )));
        }
        Ok(None)
    }

    /// Drain any previously buffered remainder frames into caller `dst` with `0` heap allocations.
    pub fn try_pop_pending_into(
        &self,
        dst: &mut [u8],
        requested_frames: usize,
    ) -> Result<Option<(ReadResult, MixedBlockMetadata)>, ReadError> {
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;
        self.try_pop_pending_into_unlocked(dst, requested_frames.max(1))
    }

    fn try_pop_pending_into_unlocked(
        &self,
        dst: &mut [u8],
        req_frames: usize,
    ) -> Result<Option<(ReadResult, MixedBlockMetadata)>, ReadError> {
        let out_stride = self.output_format.bytes_per_frame();
        if dst.len() < req_frames.saturating_mul(out_stride) {
            return Err(ReadError::InvalidDestination);
        }
        let mut out_st = self.output_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((byte_off, rem_frames, meta)) = out_st.pending_out_bytes.take() {
            let emit_frames = rem_frames.min(req_frames);
            let emit_bytes = emit_frames * out_stride;
            dst[..emit_bytes]
                .copy_from_slice(&out_st.out_bytes_scratch[byte_off..byte_off + emit_bytes]);
            if emit_frames < rem_frames {
                out_st.pending_out_bytes =
                    Some((byte_off + emit_bytes, rem_frames - emit_frames, meta));
            }
            drop(out_st);
            *self.last_metadata.lock().unwrap_or_else(|e| e.into_inner()) = Some(meta);
            return Ok(Some((
                ReadResult {
                    frames: emit_frames,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Complete,
                    control: None,
                    copies: CopyStats {
                        bytes: emit_bytes as u64,
                        destinations: 1,
                        ..Default::default()
                    },
                },
                meta,
            )));
        }

        if let Some((mut pending_slice, meta)) = out_st.pending_out_slice.take() {
            let emit_frames = pending_slice.frames.min(req_frames);
            let emit_bytes = emit_frames * out_stride;
            let src_bytes = pending_slice.bytes();
            dst[..emit_bytes].copy_from_slice(&src_bytes[..emit_bytes]);
            if emit_frames < pending_slice.frames {
                pending_slice.first += emit_frames;
                pending_slice.frames -= emit_frames;
                out_st.pending_out_slice = Some((pending_slice, meta));
            }
            drop(out_st);
            *self.last_metadata.lock().unwrap_or_else(|e| e.into_inner()) = Some(meta);
            return Ok(Some((
                ReadResult {
                    frames: emit_frames,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Complete,
                    control: None,
                    copies: CopyStats {
                        bytes: emit_bytes as u64,
                        destinations: 1,
                        ..Default::default()
                    },
                },
                meta,
            )));
        }
        Ok(None)
    }

    /// Mix one `block_frames` quantum (unless remainder frames are already pending) and return up
    /// to `requested_frames` as an owned `Pool`-backed `ReadResult` (`StorageBudget`-charged).
    pub fn read_mixed_owned(
        &self,
        requested_frames: usize,
        now_ns: u64,
        oldest_wait_ns: u64,
    ) -> Result<(ReadResult, MixedBlockMetadata), ReadError> {
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;
        let req_frames = requested_frames.max(1);
        if let Some(res) = self.try_pop_pending_owned_unlocked(req_frames)? {
            return Ok(res);
        }

        let mut out_st = self.output_state.lock().unwrap_or_else(|e| e.into_inner());
        let (outcome, meta) = self.poll_mix_bytes_unlocked(
            self.block_frames,
            now_ns,
            oldest_wait_ns,
            &mut out_st.out_bytes_scratch,
        )?;

        match outcome {
            MixStepOutcome::Produced(out_frames) => {
                out_st.sequence = out_st.sequence.saturating_add(1);
                let hdr = Header {
                    sequence: out_st.sequence,
                    segment: meta.membership_version,
                    generation: 1,
                    frames: out_frames,
                    sample_position: None,
                    timestamp: None,
                };
                let MixedOutputBufferState {
                    ref mut out_pool,
                    ref out_bytes_scratch,
                    ref mut pending_out_slice,
                    ..
                } = *out_st;
                let block = out_pool
                    .offer(out_bytes_scratch, hdr)
                    .map_err(|_| ReadError::InvalidDestination)?;
                let emit_frames = out_frames.min(req_frames);
                if emit_frames < out_frames {
                    *pending_out_slice = Some((
                        Slice {
                            block: block.clone(),
                            first: emit_frames,
                            frames: out_frames - emit_frames,
                        },
                        meta,
                    ));
                }
                let slice = Slice {
                    block,
                    first: 0,
                    frames: emit_frames,
                };
                let emit_bytes = slice.bytes().len() as u64;
                Ok((
                    ReadResult {
                        frames: emit_frames,
                        chunks: vec![slice],
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Complete,
                        control: None,
                        copies: CopyStats {
                            bytes: emit_bytes,
                            destinations: 1,
                            ..Default::default()
                        },
                    },
                    meta,
                ))
            }
            MixStepOutcome::ParkedIdle => Ok((
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Timeout,
                    control: None,
                    copies: Default::default(),
                },
                meta,
            )),
            MixStepOutcome::Eof => Ok((
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Eof,
                    control: None,
                    copies: Default::default(),
                },
                meta,
            )),
            MixStepOutcome::Failed(_) => Ok((
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Failure(Failure::Backend),
                    control: None,
                    copies: Default::default(),
                },
                meta,
            )),
        }
    }

    /// Mix one `block_frames` quantum (unless remainder bytes are already pending) directly into
    /// caller `dst` with **`0` steady-state heap allocations** (`pending_out_bytes` remainder slicing).
    pub fn read_mixed_into(
        &self,
        dst: &mut [u8],
        requested_frames: usize,
        now_ns: u64,
        oldest_wait_ns: u64,
    ) -> Result<(ReadResult, MixedBlockMetadata), ReadError> {
        let _guard = self.read_lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => ReadError::AlreadyPending,
            TryLockError::Poisoned(_) => ReadError::Poisoned,
        })?;
        let req_frames = requested_frames.max(1);
        let out_stride = self.output_format.bytes_per_frame();
        if dst.len() < req_frames.saturating_mul(out_stride) {
            return Err(ReadError::InvalidDestination);
        }
        if let Some(res) = self.try_pop_pending_into_unlocked(dst, req_frames)? {
            return Ok(res);
        }

        let mut out_st = self.output_state.lock().unwrap_or_else(|e| e.into_inner());
        let (outcome, meta) = self.poll_mix_bytes_unlocked(
            self.block_frames,
            now_ns,
            oldest_wait_ns,
            &mut out_st.out_bytes_scratch,
        )?;

        match outcome {
            MixStepOutcome::Produced(out_frames) => {
                out_st.sequence = out_st.sequence.saturating_add(1);
                let emit_frames = out_frames.min(req_frames);
                let emit_bytes = emit_frames * out_stride;
                dst[..emit_bytes].copy_from_slice(&out_st.out_bytes_scratch[..emit_bytes]);
                if emit_frames < out_frames {
                    out_st.pending_out_bytes = Some((emit_bytes, out_frames - emit_frames, meta));
                }
                Ok((
                    ReadResult {
                        frames: emit_frames,
                        chunks: Vec::new(),
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Complete,
                        control: None,
                        copies: CopyStats {
                            bytes: emit_bytes as u64,
                            destinations: 1,
                            ..Default::default()
                        },
                    },
                    meta,
                ))
            }
            MixStepOutcome::ParkedIdle => Ok((
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Timeout,
                    control: None,
                    copies: Default::default(),
                },
                meta,
            )),
            MixStepOutcome::Eof => Ok((
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Eof,
                    control: None,
                    copies: Default::default(),
                },
                meta,
            )),
            MixStepOutcome::Failed(_) => Ok((
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Failure(Failure::Backend),
                    control: None,
                    copies: Default::default(),
                },
                meta,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        schema::{
            BackendId, Backends, ByteOrder, Channel, DefaultDeviceMode, Format, Header, Layout,
            OnRootExit, ProcessScope, SampleType, TargetKind,
        },
        storage::Pool,
    };

    const ROOT: ProcessIdentity = ProcessIdentity {
        pid: 10,
        generation: 100,
    };
    const CHILD: ProcessIdentity = ProcessIdentity {
        pid: 11,
        generation: 200,
    };
    const GRANDCHILD: ProcessIdentity = ProcessIdentity {
        pid: 12,
        generation: 300,
    };

    #[test]
    fn multi_input_manager_overlap_topology_and_remainder_slicing() {
        let target = CaptureTarget::application(
            ROOT,
            Some("com.example.app".into()),
            Some("Example".into()),
            Some("/bin/example".into()),
            ProcessScope::Tree,
            OnRootExit::Wait {
                wait_timeout_ns: 5_000_000_000,
            },
        )
        .unwrap();
        assert_eq!(
            target.lower_to_process().unwrap().kind(),
            TargetKind::Process
        );
        assert!(Backends::new(Some(vec![BackendId::PipeWire]), None, None).is_err());

        let fmt_m3 = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .unwrap(),
        );
        let t_dup = CaptureTarget::process(ROOT, ProcessScope::Tree, OnRootExit::Stop).unwrap();
        let t_child = CaptureTarget::process(CHILD, ProcessScope::Tree, OnRootExit::Stop).unwrap();
        let req_m3 = CaptureRequest::new(vec![t_dup.clone(), t_dup.clone(), t_child.clone()]);
        let mut multi = MultiInputManager::new(req_m3, fmt_m3, 64).unwrap();
        assert_eq!(multi.input_handles().len(), 3);
        assert_eq!(
            multi.input_handles()[0]
                .snapshot()
                .unwrap()
                .participation_state,
            ParticipationState::Starting
        );
        let p_tickets: Vec<_> = (0..4)
            .map(|_| multi.acquire_pending_open().unwrap())
            .collect();
        assert!(matches!(
            multi.acquire_pending_open(),
            Err(ManagerError::Capacity)
        ));
        drop(p_tickets);
        assert!(multi.acquire_pending_open().is_ok());
        assert!(multi.attach_topology_source(0, "pw-node-77").unwrap());
        assert!(!multi.attach_topology_source(2, "pw-node-77").unwrap());
        assert!(multi.retire_topology_target(0).is_empty());
        assert_eq!(
            multi.retire_topology_target(2),
            vec![Arc::<str>::from("pw-node-77")]
        );
        for idx in 0..3 {
            multi
                .push_input_packet(
                    idx,
                    MixerInputPacket {
                        samples: vec![0.25_f32; 128],
                        frames: 64,
                        timestamp: None,
                        arrival_ns: 0,
                    },
                )
                .unwrap();
        }
        let (owned_r1, out_meta) = multi.read_mixed_owned(32, 1000, 0).unwrap();
        assert_eq!(owned_r1.frames, 32);
        assert!(multi.has_pending_output());
        assert_eq!(out_meta.contributing_mask, 0b111);
        let first_f32 = f32::from_le_bytes(owned_r1.chunks[0].bytes()[0..4].try_into().unwrap());
        assert!((first_f32 - 0.75).abs() <= 1e-6);
        let (owned_r2, _) = multi.read_mixed_owned(32, 1000, 0).unwrap();
        assert_eq!(owned_r2.frames, 32);
        assert!(!multi.has_pending_output());
        assert_eq!(
            multi.input_handles()[0]
                .snapshot()
                .unwrap()
                .participation_state,
            ParticipationState::Active
        );
        let t_grand =
            CaptureTarget::process(GRANDCHILD, ProcessScope::Tree, OnRootExit::Stop).unwrap();
        let mapping = multi
            .reconfigure_targets(vec![t_child.clone(), t_grand.clone()])
            .unwrap();
        assert_eq!(mapping, vec![Some(2), None]);
        assert_eq!(multi.targets(), &[t_child, t_grand]);
        assert_eq!(multi.input_handles().len(), 2);
        assert_eq!(
            multi.input_handles()[0]
                .snapshot()
                .unwrap()
                .participation_state,
            ParticipationState::Active
        );
        assert_eq!(
            multi.input_handles()[1]
                .snapshot()
                .unwrap()
                .participation_state,
            ParticipationState::Starting
        );
    }

    #[test]
    fn retained_block_keeps_session_charged() {
        let limits = StorageLimits::new(8, 4096, 8, 262144).unwrap();
        let budget = StorageBudget::new(2, 557056).unwrap();
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
        let mut pool =
            Pool::new_reserved(limits, 4096, format, budget.reserve(limits, 16384).unwrap())
                .unwrap();
        let held = pool
            .offer(
                &[7; 8],
                Header {
                    sequence: 1,
                    segment: 1,
                    generation: 1,
                    frames: 1,
                    sample_position: None,
                    timestamp: None,
                },
            )
            .unwrap();
        drop(pool);
        let second = budget.reserve(limits, 16384).unwrap();
        assert_eq!(budget.resident_sessions(), 2);
        assert_eq!(budget.reserved_bytes(), 557056);
        assert!(matches!(
            budget.reserve(limits, 16384),
            Err(AdmissionError::Full)
        ));
        assert_eq!(held.bytes(), &[7; 8]);
        std::thread::spawn(move || drop(held)).join().unwrap();
        assert_eq!(budget.resident_sessions(), 1);
        assert_eq!(budget.reserved_bytes(), 278528);
        let replacement = budget.reserve(limits, 16384).unwrap();
        drop(second);
        drop(replacement);
        assert_eq!(budget.reserved_bytes(), 0);
        assert_eq!(budget.resident_sessions(), 0);
    }

    #[test]
    fn admission_limits_overflow_and_reuse() {
        let limits = StorageLimits::new(1, 1, 8, 8).unwrap();
        assert!(matches!(
            StorageBudget::new(0, 8),
            Err(AdmissionError::Invalid(CapacityError::Zero))
        ));
        let budget = StorageBudget::new(2, 24).unwrap();
        assert!(matches!(
            budget.reserve(limits, usize::MAX),
            Err(AdmissionError::Invalid(CapacityError::Overflow))
        ));
        assert!(matches!(
            budget.reserve(limits, 17),
            Err(AdmissionError::Invalid(CapacityError::ExceedsByteCeiling))
        ));
        for _ in 0..1000 {
            let first = budget.reserve(limits, 8).unwrap();
            assert!(matches!(
                budget.reserve(limits, 1),
                Err(AdmissionError::Full)
            ));
            assert_eq!(budget.resident_sessions(), 1);
            let second = budget.reserve(limits, 0).unwrap();
            assert_eq!(budget.reserved_bytes(), 24);
            assert!(matches!(
                budget.reserve(limits, 0),
                Err(AdmissionError::Full)
            ));
            let clone = first.retain();
            drop(first);
            assert_eq!(budget.reserved_bytes(), 24);
            drop(clone);
            drop(second);
            assert_eq!(budget.resident_sessions(), 0);
            assert_eq!(budget.reserved_bytes(), 0);
        }
    }

    #[test]
    fn heterogeneous_multi_input_manager_normalizes_compensates_drift_and_runs_zero_allocations() {
        use crate::schema::Channel;

        let t_app = CaptureTarget::application(
            ROOT,
            Some("us.zoom.xos".into()),
            Some("Zoom".into()),
            None,
            ProcessScope::Exact,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_backend(BackendId::ScreenCaptureKit);
        let t_mic =
            CaptureTarget::microphone(DefaultDeviceMode::Follow).with_backend(BackendId::CoreAudio);

        let fmt_48k_stereo = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .unwrap(),
        );
        let fmt_16k_mono_i16 = Arc::new(
            Format::new(
                SampleType::I16,
                ByteOrder::Little,
                16_000,
                1,
                Layout::Known(Arc::from([Channel::Mono])),
            )
            .unwrap(),
        );

        let req = CaptureRequest::new(vec![t_app.clone(), t_mic.clone()]);
        let mut multi = MultiInputManager::new_heterogeneous(
            req,
            vec![
                (BackendId::ScreenCaptureKit, fmt_48k_stereo.clone()),
                (BackendId::CoreAudio, fmt_16k_mono_i16.clone()),
            ],
            480,
            1024,
        )
        .unwrap();

        assert_eq!(multi.working_format(), &fmt_48k_stereo);
        assert_eq!(
            multi.input_handles()[0].backend(),
            BackendId::ScreenCaptureKit
        );
        assert!(!multi.input_handles()[0].is_resampled());
        assert!(multi.input_handles()[0].is_identity_bypass());
        assert_eq!(multi.input_handles()[1].backend(), BackendId::CoreAudio);
        assert!(multi.input_handles()[1].is_resampled());
        assert!(!multi.input_handles()[1].is_identity_bypass());

        // The microphone packet models a clock running 0.625% fast.
        let mut app_pkt = Vec::with_capacity(480 * 8);
        for _ in 0..(480 * 2) {
            app_pkt.extend_from_slice(&0.25_f32.to_le_bytes());
        }
        let mut mic_fast_pkt = Vec::with_capacity(161 * 2);
        for _ in 0..161 {
            mic_fast_pkt.extend_from_slice(&(8192_i16).to_le_bytes()); // ~0.25 FS
        }

        let mut out_bytes = Vec::with_capacity(480 * 8);
        // Prime initial filter delay (32 input frames at 16 kHz)
        multi.push_raw_input_bytes(0, &app_pkt, None, 0).unwrap();
        multi
            .push_raw_input_bytes(1, &mic_fast_pkt, None, 0)
            .unwrap();
        multi
            .push_raw_input_bytes(1, &mic_fast_pkt, None, 5_000_000)
            .unwrap();
        let _ = multi
            .poll_mix_bytes(480, 10_000_000, 0, &mut out_bytes)
            .unwrap();

        let allocs = allocation_counter::measure(|| {
            for step in 1..=300u64 {
                let now_ns = step * 10_000_000;
                multi
                    .push_raw_input_bytes(0, &app_pkt, None, now_ns)
                    .unwrap();
                multi
                    .push_raw_input_bytes(1, &mic_fast_pkt, None, now_ns)
                    .unwrap();
                let (outcome, _) = multi
                    .poll_mix_bytes(480, now_ns, 0, &mut out_bytes)
                    .unwrap();
                assert_eq!(outcome, MixStepOutcome::Produced(480));
            }
        });
        assert_eq!(allocs.count_total, 0);
        let mic_snap = multi.input_handles()[1].snapshot().unwrap();
        assert!(mic_snap.drift_slips >= 2);
        assert_eq!(mic_snap.dropped_frames, 0);

        let mapping = multi
            .reconfigure_targets_with_formats(
                vec![t_mic.clone()],
                vec![(BackendId::CoreAudio, fmt_16k_mono_i16.clone())],
            )
            .unwrap();
        assert_eq!(mapping, vec![Some(1)]);
        assert!(multi.input_handles()[0].is_resampled());

        let mut dst_sub = vec![0u8; 240 * 8];
        multi
            .push_raw_input_bytes(0, &mic_fast_pkt, None, 3_010_000_000)
            .unwrap();
        let _ = multi
            .read_mixed_into(&mut dst_sub, 240, 3_010_000_000, 0)
            .unwrap();
        let _ = multi
            .read_mixed_into(&mut dst_sub, 240, 3_010_000_000, 0)
            .unwrap();

        let into_allocs = allocation_counter::measure(|| {
            for step in 302..=400u64 {
                let now_ns = step * 10_000_000;
                multi
                    .push_raw_input_bytes(0, &mic_fast_pkt, None, now_ns)
                    .unwrap();
                let (r1, m1) = multi.read_mixed_into(&mut dst_sub, 240, now_ns, 0).unwrap();
                assert_eq!(r1.frames, 240);
                assert!(multi.has_pending_output());
                let (r2, m2) = multi.read_mixed_into(&mut dst_sub, 240, now_ns, 0).unwrap();
                assert_eq!(r2.frames, 240);
                assert!(!multi.has_pending_output());
                assert_eq!(m1.membership_version, m2.membership_version);
            }
        });
        assert_eq!(into_allocs.count_total, 0);
        assert!(multi.last_block_metadata().is_some());
    }

    #[test]
    fn multi_input_manager_charges_out_pool_budget_and_enforces_hot_swap_format_gates() {
        use crate::schema::Channel;

        let fmt_16k_mono = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                16_000,
                1,
                Layout::Known(Arc::from([Channel::Mono])),
            )
            .unwrap(),
        );
        let fmt_48k_stereo = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .unwrap(),
        );
        let t1 = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        let t2 = CaptureTarget::default_output(DefaultDeviceMode::Follow);

        let shared_budget = StorageBudget::new(16, 4 * 1024 * 1024).unwrap();
        let req_16k = CaptureRequest::single(t1.clone());
        let mut mgr_16k = MultiInputManager::new_heterogeneous_with_budget(
            req_16k,
            vec![(BackendId::Wasapi, fmt_16k_mono.clone())],
            default_block_frames_for_rate(16_000, 1024),
            1024,
            shared_budget.clone(),
        )
        .unwrap();
        assert_eq!(shared_budget.resident_sessions(), 0);
        assert_eq!(shared_budget.reserved_bytes(), 8 * 1024 * 4);
        let stream_limits = StorageLimits::new(8, 256, 8, 8 * 256 * 8).unwrap();
        let mut stream_res = Vec::new();
        for _ in 0..16 {
            stream_res.push(shared_budget.reserve(stream_limits, 0).unwrap());
        }
        assert_eq!(shared_budget.resident_sessions(), 16);
        drop(stream_res);

        let tiny_budget = StorageBudget::new(4, 16 * 1024).unwrap();
        let too_small = MultiInputManager::new_heterogeneous_with_budget(
            CaptureRequest::single(t1.clone()),
            vec![(BackendId::Wasapi, fmt_16k_mono.clone())],
            160,
            1024,
            tiny_budget,
        );
        assert_eq!(too_small.err(), Some(ManagerError::Capacity));

        let promote_err = mgr_16k.reconfigure_targets_with_formats(
            vec![t1.clone(), t2.clone()],
            vec![
                (BackendId::Wasapi, fmt_16k_mono.clone()),
                (BackendId::Wasapi, fmt_48k_stereo.clone()),
            ],
        );
        assert_eq!(promote_err, Err(ManagerError::WorkingFormatChanged));
        assert_eq!(mgr_16k.targets().len(), 1); // original state untouched on error!

        drop(mgr_16k);
        assert_eq!(shared_budget.reserved_bytes(), 0);

        let strict_req = CaptureRequest::single(t2.clone())
            .with_input_conversion(InputConversionPolicy::StrictNoResample);
        let mut strict_mgr = MultiInputManager::new_heterogeneous(
            strict_req,
            vec![(BackendId::Wasapi, fmt_48k_stereo.clone())],
            480,
            1024,
        )
        .unwrap();
        let strict_err = strict_mgr.reconfigure_targets_with_formats(
            vec![t2, t1],
            vec![
                (BackendId::Wasapi, fmt_48k_stereo),
                (BackendId::Wasapi, fmt_16k_mono),
            ],
        );
        assert_eq!(strict_err, Err(ManagerError::Unsupported));
        assert_eq!(strict_mgr.targets().len(), 1);
    }
}
