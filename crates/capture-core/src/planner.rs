use crate::{
    converter::validate_channel_mapping,
    schema::{
        AggregateManagerBudgets, BackendId, Backends, ByteOrder, CaptureTarget, Channel,
        CombineMode, CombineResolution, DefaultDeviceMode, Format, InputConversionPolicy, Layout,
        MixerConfig, OnFormatChange, OnRootExit, OutputConverterConfig, PermissionKind,
        PermissionPolicy, PlanError, PlatformOs, ProcessScope, ProcessTreeRange,
        ProcessTreeStrategy, SampleType, TargetAcquisition, TargetKind,
    },
    traits::CapabilityError,
};
use std::{collections::BTreeSet, sync::Arc};

pub const UNIT_GAIN_TOLERANCE: f32 = 1e-6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlapCombinationSemantics {
    SingleTarget,
    SoftwareSummation,
    NativeSetUnion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetCandidateAttempt {
    pub backend: BackendId,
    pub acquisition: Option<TargetAcquisition>,
    pub accepted: bool,
    pub rejection: Option<PlanError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetPlan {
    pub target_index: usize,
    pub target_id: Arc<str>,
    pub resolved_backend: BackendId,
    pub acquisition: TargetAcquisition,
    pub attempts: Vec<TargetCandidateAttempt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CapturePlanReport {
    pub os: PlatformOs,
    pub selected_backend: Option<BackendId>,
    pub target_plans: Vec<TargetPlan>,
    pub combine_mode: CombineMode,
    pub combine_resolution: CombineResolution,
    pub tree_strategy: ProcessTreeRange,
    pub input_conversion: InputConversionPolicy,
    pub backends: Backends,
    pub uses_software_mixer: bool,
    pub uses_output_converter: bool,
    pub requires_discovery_subscription: bool,
    pub overlap_semantics: OverlapCombinationSemantics,
    pub duplicate_target_ids: Vec<Arc<str>>,
    pub allow_reroute: bool,
    pub permission_policy: PermissionPolicy,
    pub estimated_active_streams: usize,
    pub estimated_pcm_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureRequest {
    pub(crate) targets: Vec<CaptureTarget>,
    pub(crate) tree_strategy: ProcessTreeRange,
    pub(crate) combine_mode: CombineMode,
    pub(crate) input_conversion: InputConversionPolicy,
    pub(crate) backends: Backends,
    pub(crate) output_converter: Option<OutputConverterConfig>,
    pub(crate) on_format_change: OnFormatChange,
    pub(crate) mixer_config: MixerConfig,
    pub(crate) budgets: AggregateManagerBudgets,
    pub(crate) allow_reroute: bool,
    pub(crate) require_precise_timing: bool,
    pub(crate) permission_policy: PermissionPolicy,
}

impl CaptureRequest {
    pub fn new(targets: Vec<CaptureTarget>) -> Self {
        Self {
            targets,
            tree_strategy: ProcessTreeRange::default(),
            combine_mode: CombineMode::Auto,
            input_conversion: InputConversionPolicy::AutoWhenMixing,
            backends: Backends::default(),
            output_converter: None,
            on_format_change: OnFormatChange::Adapt,
            mixer_config: MixerConfig::default(),
            budgets: AggregateManagerBudgets::default(),
            allow_reroute: false,
            require_precise_timing: false,
            permission_policy: PermissionPolicy::AutoRequest,
        }
    }

    pub fn single(target: CaptureTarget) -> Self {
        Self::new(vec![target])
    }

    #[inline]
    pub fn targets(&self) -> &[CaptureTarget] {
        &self.targets
    }

    #[inline]
    pub fn tree_strategy(&self) -> ProcessTreeRange {
        self.tree_strategy
    }

    #[inline]
    pub fn combine_mode(&self) -> CombineMode {
        self.combine_mode
    }

    #[inline]
    pub fn input_conversion(&self) -> InputConversionPolicy {
        self.input_conversion
    }

    #[inline]
    pub fn backends(&self) -> &Backends {
        &self.backends
    }

    #[inline]
    pub fn output_converter(&self) -> Option<&OutputConverterConfig> {
        self.output_converter.as_ref()
    }

    #[inline]
    pub fn on_format_change(&self) -> OnFormatChange {
        self.on_format_change
    }

    #[inline]
    pub fn mixer_config(&self) -> &MixerConfig {
        &self.mixer_config
    }

    #[inline]
    pub fn budgets(&self) -> &AggregateManagerBudgets {
        &self.budgets
    }

    #[inline]
    pub fn allow_reroute(&self) -> bool {
        self.allow_reroute
    }

    #[inline]
    pub fn require_precise_timing(&self) -> bool {
        self.require_precise_timing
    }

    #[inline]
    pub fn permission_policy(&self) -> PermissionPolicy {
        self.permission_policy
    }

    pub fn with_targets(mut self, targets: Vec<CaptureTarget>) -> Self {
        self.targets = targets;
        self
    }

    pub fn with_tree_strategy(mut self, tree_strategy: ProcessTreeRange) -> Self {
        self.tree_strategy = tree_strategy;
        self
    }

    pub fn with_combine_mode(mut self, combine_mode: CombineMode) -> Self {
        self.combine_mode = combine_mode;
        self
    }

    pub fn with_input_conversion(mut self, policy: InputConversionPolicy) -> Self {
        self.input_conversion = policy;
        self
    }

    pub fn with_backends(mut self, backends: Backends) -> Self {
        self.backends = backends;
        self
    }

    pub fn with_output_converter(
        mut self,
        output_converter: Option<OutputConverterConfig>,
    ) -> Self {
        self.output_converter = output_converter;
        self
    }

    pub fn with_on_format_change(mut self, on_format_change: OnFormatChange) -> Self {
        self.on_format_change = on_format_change;
        self
    }

    pub fn with_mixer_config(mut self, mixer_config: MixerConfig) -> Self {
        self.mixer_config = mixer_config;
        self
    }

    pub fn with_budgets(mut self, budgets: AggregateManagerBudgets) -> Self {
        self.budgets = budgets;
        self
    }

    pub fn with_allow_reroute(mut self, allow_reroute: bool) -> Self {
        self.allow_reroute = allow_reroute;
        self
    }

    pub fn with_require_precise_timing(mut self, require_precise_timing: bool) -> Self {
        self.require_precise_timing = require_precise_timing;
        self
    }

    pub fn with_permission_policy(mut self, permission_policy: PermissionPolicy) -> Self {
        self.permission_policy = permission_policy;
        self
    }

    pub fn required_permission_kinds_for_os(
        &self,
        os: PlatformOs,
    ) -> Result<BTreeSet<PermissionKind>, PlanError> {
        required_permission_kinds_for_os_with_filter(
            &self.targets,
            &self.backends,
            self.tree_strategy,
            os,
            |_, _| true,
        )
    }

    pub fn required_permission_kinds(&self) -> Result<BTreeSet<PermissionKind>, PlanError> {
        self.required_permission_kinds_for_os(PlatformOs::current())
    }

    /// Inspects the capture plan without side effects for the specified OS.
    pub fn inspect_for_os(&self, os: PlatformOs) -> Result<CapturePlanReport, PlanError> {
        self.resolve_plan_for_os_with_readiness(os, |_, _| Ok(()))
    }

    pub fn inspect(&self) -> Result<CapturePlanReport, PlanError> {
        self.inspect_for_os(PlatformOs::current())
    }

    /// Resolves a capture plan using the supplied backend readiness check.
    pub fn resolve_plan_for_os_with_readiness<F>(
        &self,
        os: PlatformOs,
        mut check_readiness: F,
    ) -> Result<CapturePlanReport, PlanError>
    where
        F: FnMut(BackendId, &CaptureTarget) -> Result<(), PlanError>,
    {
        let mut exact_plan_err: Option<PlanError> = None;
        self.resolve_plan_for_os_with_capability(os, |backend, target| {
            check_readiness(backend, target).map_err(|pe| {
                exact_plan_err.get_or_insert(pe);
                CapabilityError::from(pe)
            })
        })
        .map_err(|cap_err| exact_plan_err.unwrap_or_else(|| cap_err.to_plan_error()))
    }

    /// Resolves a capture plan while preserving structured capability errors.
    pub fn resolve_plan_for_os_with_capability<F>(
        &self,
        os: PlatformOs,
        mut check_readiness: F,
    ) -> Result<CapturePlanReport, CapabilityError>
    where
        F: FnMut(BackendId, &CaptureTarget) -> Result<(), CapabilityError>,
    {
        if self.targets.is_empty() {
            return Err(CapabilityError::InvalidTarget);
        }
        self.tree_strategy.validate()?;
        self.backends.validate()?;
        self.mixer_config.validate()?;
        self.budgets.validate()?;

        if self.targets.len() > self.budgets.max_targets
            || self.targets.len() > self.budgets.max_active_streams
        {
            return Err(CapabilityError::CapacityExceeded);
        }

        let mut seen_ids = BTreeSet::new();
        let mut duplicate_ids = Vec::new();
        for target in &self.targets {
            target.validate()?;
            if !seen_ids.insert(target.id.clone())
                && !duplicate_ids.iter().any(|dup| dup == &target.id)
            {
                duplicate_ids.push(target.id.clone());
            }
        }

        if self.require_precise_timing {
            return Err(CapabilityError::PreciseTimingUnqualified);
        }

        if let Some(ref conv) = self.output_converter {
            validate_output_converter_target_format(&conv.target_format)?;
        }

        let os_backends = self.backends.for_os(os);
        let mut target_plans = Vec::with_capacity(self.targets.len());

        for (idx, target) in self.targets.iter().enumerate() {
            let candidate_backends = candidate_backends_for_target(os, os_backends, target)?;
            let mut t_attempts = Vec::with_capacity(candidate_backends.len());
            let mut first_cap_rejection: Option<CapabilityError> = None;
            let mut chosen: Option<(BackendId, TargetAcquisition)> = None;

            for cand_backend in candidate_backends {
                if let Err(ready_err) = check_readiness(cand_backend, target) {
                    let plan_rej = ready_err.to_plan_error();
                    let allow_fb = ready_err.allows_cross_backend_fallback();
                    if first_cap_rejection.is_none() {
                        first_cap_rejection = Some(ready_err.clone());
                    }
                    t_attempts.push(TargetCandidateAttempt {
                        backend: cand_backend,
                        acquisition: None,
                        accepted: false,
                        rejection: Some(plan_rej),
                    });
                    if !allow_fb {
                        return Err(ready_err);
                    }
                    continue;
                }

                match evaluate_target_on_backend(cand_backend, target, self.tree_strategy) {
                    Ok(acq) => {
                        t_attempts.push(TargetCandidateAttempt {
                            backend: cand_backend,
                            acquisition: Some(acq),
                            accepted: true,
                            rejection: None,
                        });
                        if chosen.is_none() {
                            chosen = Some((cand_backend, acq));
                        }
                        break;
                    }
                    Err(err) => {
                        let cap_err = CapabilityError::from(err);
                        if first_cap_rejection.is_none() {
                            first_cap_rejection = Some(cap_err);
                        }
                        t_attempts.push(TargetCandidateAttempt {
                            backend: cand_backend,
                            acquisition: None,
                            accepted: false,
                            rejection: Some(err),
                        });
                        if !plan_error_allows_cross_backend_fallback(err) {
                            return Err(CapabilityError::from(err));
                        }
                    }
                }
            }

            let Some((resolved_backend, acquisition)) = chosen else {
                return Err(first_cap_rejection.unwrap_or(CapabilityError::NotImplemented));
            };

            target_plans.push(TargetPlan {
                target_index: idx,
                target_id: target.id.clone(),
                resolved_backend,
                acquisition,
                attempts: t_attempts,
            });
        }

        let first_backend = target_plans[0].resolved_backend;
        let all_same_backend = target_plans
            .iter()
            .all(|p| p.resolved_backend == first_backend);
        let any_input = self
            .targets
            .iter()
            .any(|t| matches!(t.kind, TargetKind::DefaultInput | TargetKind::InputDevice));
        let all_unit_gain = self.targets.iter().all(|t| is_unit_gain(t.gain));
        let any_software_stream_mix = target_plans.iter().any(|p| {
            p.acquisition == TargetAcquisition::ProcessTree(ProcessTreeStrategy::SoftwareStreamMix)
        });
        let all_proc_or_app = self
            .targets
            .iter()
            .all(|t| matches!(t.kind, TargetKind::Process | TargetKind::Application));
        let has_duplicates = !duplicate_ids.is_empty();

        let combine_resolution = match self.combine_mode {
            CombineMode::CspaMixer => CombineResolution::CspaMixer,
            CombineMode::NativeOnly => {
                if any_software_stream_mix {
                    return Err(CapabilityError::NotImplemented);
                }
                if self.targets.len() == 1 && all_unit_gain && self.output_converter.is_none() {
                    CombineResolution::PassThrough(first_backend)
                } else if self.targets.len() > 1
                    && all_same_backend
                    && !any_input
                    && all_unit_gain
                    && !has_duplicates
                    && self.output_converter.is_none()
                    && matches!(first_backend, BackendId::PipeWire | BackendId::CoreAudio)
                    && all_proc_or_app
                {
                    CombineResolution::SharedNativeGraphMix(first_backend)
                } else {
                    return Err(CapabilityError::NotImplemented);
                }
            }
            CombineMode::Auto => {
                if self.targets.len() == 1
                    && all_unit_gain
                    && self.output_converter.is_none()
                    && !any_software_stream_mix
                {
                    CombineResolution::PassThrough(first_backend)
                } else if self.targets.len() > 1
                    && all_same_backend
                    && !any_input
                    && all_unit_gain
                    && !has_duplicates
                    && self.output_converter.is_none()
                    && matches!(first_backend, BackendId::PipeWire | BackendId::CoreAudio)
                    && all_proc_or_app
                {
                    CombineResolution::SharedNativeGraphMix(first_backend)
                } else {
                    CombineResolution::CspaMixer
                }
            }
        };

        let uses_software_mixer = combine_resolution == CombineResolution::CspaMixer;
        let selected_backend = all_same_backend.then_some(first_backend);

        // Per-slot 8x4096 stereo f32 (32,768 B/slot = 262,144 B/stream)
        let per_stream_pcm_bytes = 8 * 4096 * 8;
        let mixer_pcm_bytes = if uses_software_mixer {
            self.mixer_config.output_queue_slots * 4096 * 8
        } else {
            0
        };
        let converter_pcm_bytes = if self.output_converter.is_some() {
            4 * 4096 * 8
        } else {
            0
        };
        let estimated_pcm_bytes = self
            .targets
            .len()
            .checked_mul(per_stream_pcm_bytes)
            .and_then(|v| v.checked_add(mixer_pcm_bytes))
            .and_then(|v| v.checked_add(converter_pcm_bytes))
            .ok_or(CapabilityError::CapacityExceeded)?;
        if estimated_pcm_bytes > self.budgets.max_total_pcm_memory {
            return Err(CapabilityError::CapacityExceeded);
        }

        let requires_discovery_subscription = uses_software_mixer
            || matches!(
                combine_resolution,
                CombineResolution::SharedNativeGraphMix(_)
            )
            || target_plans.iter().any(|p| {
                p.acquisition == TargetAcquisition::ProcessTree(ProcessTreeStrategy::NativeGraphMix)
            })
            || self.targets.iter().any(|t| {
                matches!(t.on_root_exit, OnRootExit::Wait { .. })
                    || (matches!(t.kind, TargetKind::DefaultOutput | TargetKind::DefaultInput)
                        && t.default_device_mode == DefaultDeviceMode::Follow)
            });

        let overlap_semantics = if self.targets.len() <= 1 {
            OverlapCombinationSemantics::SingleTarget
        } else if uses_software_mixer {
            OverlapCombinationSemantics::SoftwareSummation
        } else {
            OverlapCombinationSemantics::NativeSetUnion
        };

        Ok(CapturePlanReport {
            os,
            selected_backend,
            target_plans,
            combine_mode: self.combine_mode,
            combine_resolution,
            tree_strategy: self.tree_strategy,
            input_conversion: self.input_conversion,
            backends: self.backends.clone(),
            uses_software_mixer,
            uses_output_converter: self.output_converter.is_some(),
            requires_discovery_subscription,
            overlap_semantics,
            duplicate_target_ids: duplicate_ids,
            allow_reroute: self.allow_reroute,
            permission_policy: self.permission_policy,
            estimated_active_streams: self.targets.len(),
            estimated_pcm_bytes,
        })
    }
}

#[inline]
pub fn permission_kind_for_backend_target(
    os: PlatformOs,
    backend: BackendId,
    target_kind: TargetKind,
) -> PermissionKind {
    match target_kind {
        TargetKind::DefaultInput | TargetKind::InputDevice => PermissionKind::Microphone,
        TargetKind::DefaultOutput
        | TargetKind::Device
        | TargetKind::Process
        | TargetKind::Application => {
            if os == PlatformOs::MacOs && backend == BackendId::ScreenCaptureKit {
                PermissionKind::ScreenRecording
            } else {
                PermissionKind::SystemAudio
            }
        }
    }
}

/// Resolve the OS `PermissionKind` domains required by `targets` on `os` (empty `targets` returns
/// all OS-relevant audio capture permission domains), optionally filtering backends by OS floor.
pub fn required_permission_kinds_for_os_with_filter<F>(
    targets: &[CaptureTarget],
    backends: &Backends,
    tree_range: ProcessTreeRange,
    os: PlatformOs,
    mut backend_available: F,
) -> Result<BTreeSet<PermissionKind>, PlanError>
where
    F: FnMut(BackendId, TargetKind) -> bool,
{
    backends.validate()?;
    tree_range.validate()?;
    if targets.is_empty() {
        let mut all = BTreeSet::new();
        all.insert(PermissionKind::Microphone);
        all.insert(PermissionKind::SystemAudio);
        if os == PlatformOs::MacOs {
            all.insert(PermissionKind::ScreenRecording);
        }
        return Ok(all);
    }

    let os_backends = backends.for_os(os);
    let mut required = BTreeSet::new();
    for target in targets {
        target.validate()?;
        let candidates = candidate_backends_for_target(os, os_backends, target)?;
        let mut resolved_backend = None;
        let mut first_err = None;
        for cand in candidates {
            if !backend_available(cand, target.kind) {
                if first_err.is_none() {
                    first_err = Some(PlanError::BackendUnavailable);
                }
                continue;
            }
            match evaluate_target_on_backend(cand, target, tree_range) {
                Ok(_) => {
                    resolved_backend = Some(cand);
                    break;
                }
                Err(err) => {
                    let allow_next = plan_error_allows_cross_backend_fallback(err);
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                    if !allow_next {
                        break;
                    }
                }
            }
        }
        let backend = match resolved_backend {
            Some(b) => b,
            None => return Err(first_err.unwrap_or(PlanError::UnsupportedCapturePlan)),
        };
        required.insert(permission_kind_for_backend_target(os, backend, target.kind));
    }
    Ok(required)
}

pub fn required_permission_kinds_for_os(
    targets: &[CaptureTarget],
    backends: &Backends,
    os: PlatformOs,
) -> Result<BTreeSet<PermissionKind>, PlanError> {
    required_permission_kinds_for_os_with_filter(
        targets,
        backends,
        ProcessTreeRange::default(),
        os,
        |_, _| true,
    )
}

#[inline]
pub fn is_unit_gain(gain: f32) -> bool {
    (gain - 1.0).abs() <= UNIT_GAIN_TOLERANCE
}

pub fn plan_error_allows_cross_backend_fallback(error: PlanError) -> bool {
    matches!(
        error,
        PlanError::UnsupportedScope
            | PlanError::BackendUnavailable
            | PlanError::UnsupportedCapturePlan
    )
}

