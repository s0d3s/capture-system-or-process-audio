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

pub fn candidate_backends_for_target(
    os: PlatformOs,
    os_backends: &[BackendId],
    target: &CaptureTarget,
) -> Result<Vec<BackendId>, PlanError> {
    if os_backends.is_empty() {
        return Err(PlanError::InvalidBackend);
    }
    match target.backend {
        Some(pinned) => {
            if !pinned.is_valid_for_os(os) {
                return Err(PlanError::InvalidBackend);
            }
            Ok(vec![pinned])
        }
        None => {
            if os == PlatformOs::MacOs
                && target.kind == TargetKind::Application
                && target.scope == ProcessScope::Exact
                && os_backends == [BackendId::CoreAudio, BackendId::ScreenCaptureKit]
            {
                Ok(vec![BackendId::ScreenCaptureKit, BackendId::CoreAudio])
            } else {
                Ok(os_backends.to_vec())
            }
        }
    }
}

/// Reject unknown layouts. Mono and stereo must map to stereo; multichannel layouts
/// must support identity mapping to an identical native layout.
pub fn validate_output_converter_target_format(target_format: &Format) -> Result<(), PlanError> {
    match target_format.layout() {
        Layout::Unknown => Err(PlanError::UnsupportedCapturePlan),
        Layout::Known(labels) => {
            if target_format.channels() <= 2 {
                let stereo_ref = Format::new(
                    SampleType::F32,
                    ByteOrder::Little,
                    48_000,
                    2,
                    Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
                )
                .map_err(|_| PlanError::UnsupportedCapturePlan)?;
                validate_channel_mapping(&stereo_ref, target_format)
                    .map_err(|_| PlanError::UnsupportedCapturePlan)?;
                Ok(())
            } else {
                let identity_ref = Format::new(
                    SampleType::F32,
                    ByteOrder::Little,
                    target_format.rate(),
                    target_format.channels(),
                    Layout::Known(labels.clone()),
                )
                .map_err(|_| PlanError::UnsupportedCapturePlan)?;
                validate_channel_mapping(&identity_ref, target_format)
                    .map_err(|_| PlanError::UnsupportedCapturePlan)?;
                Ok(())
            }
        }
    }
}

pub fn evaluate_target_on_backend(
    backend: BackendId,
    target: &CaptureTarget,
    tree_range: ProcessTreeRange,
) -> Result<TargetAcquisition, PlanError> {
    match target.kind {
        TargetKind::DefaultOutput
        | TargetKind::Device
        | TargetKind::DefaultInput
        | TargetKind::InputDevice => match backend {
            BackendId::ScreenCaptureKit => Err(PlanError::UnsupportedScope),
            BackendId::Wasapi
            | BackendId::PipeWire
            | BackendId::PulseAudio
            | BackendId::CoreAudio => Ok(TargetAcquisition::DirectEndpoint),
        },
        TargetKind::Process | TargetKind::Application => match backend {
            BackendId::Wasapi => {
                if target.scope == ProcessScope::Exact {
                    return Err(PlanError::UnsupportedScope);
                }
                if tree_range.allows(ProcessTreeStrategy::NativeKernelCapture) {
                    Ok(TargetAcquisition::ProcessTree(
                        ProcessTreeStrategy::NativeKernelCapture,
                    ))
                } else {
                    Err(PlanError::UnsupportedCapturePlan)
                }
            }
            BackendId::PipeWire | BackendId::CoreAudio => {
                if tree_range.allows(ProcessTreeStrategy::NativeGraphMix) {
                    Ok(TargetAcquisition::ProcessTree(
                        ProcessTreeStrategy::NativeGraphMix,
                    ))
                } else {
                    Err(PlanError::UnsupportedCapturePlan)
                }
            }
            BackendId::PulseAudio => {
                // PulseAudio process capture uses per-sink-input software mixing.
                if tree_range.allows(ProcessTreeStrategy::SoftwareStreamMix) {
                    Ok(TargetAcquisition::ProcessTree(
                        ProcessTreeStrategy::SoftwareStreamMix,
                    ))
                } else {
                    Err(PlanError::UnsupportedCapturePlan)
                }
            }
            BackendId::ScreenCaptureKit => {
                if target.kind != TargetKind::Application || target.scope != ProcessScope::Exact {
                    return Err(PlanError::UnsupportedScope);
                }
                if tree_range.allows(ProcessTreeStrategy::NativeKernelCapture) {
                    Ok(TargetAcquisition::ProcessTree(
                        ProcessTreeStrategy::NativeKernelCapture,
                    ))
                } else if tree_range.allows(ProcessTreeStrategy::NativeGraphMix) {
                    Ok(TargetAcquisition::ProcessTree(
                        ProcessTreeStrategy::NativeGraphMix,
                    ))
                } else {
                    Err(PlanError::UnsupportedCapturePlan)
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{DiagnosticOptions, DitherMode, ProcessIdentity};

    #[test]
    fn request_inspect_reports_waterfall_and_caller_owned_overlap_without_side_effects() {
        let t1 = CaptureTarget::process(
            ProcessIdentity {
                pid: 100,
                generation: 1000,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap();
        let t2 = CaptureTarget::process(
            ProcessIdentity {
                pid: 200,
                generation: 2000,
            },
            ProcessScope::Tree,
            OnRootExit::Wait {
                wait_timeout_ns: 2_000_000_000,
            },
        )
        .unwrap();

        let single_req = CaptureRequest::single(t1.clone());
        let single_plan = single_req.inspect_for_os(PlatformOs::Windows).unwrap();
        assert_eq!(single_plan.selected_backend, Some(BackendId::Wasapi));
        assert_eq!(
            single_plan.combine_resolution,
            CombineResolution::PassThrough(BackendId::Wasapi)
        );
        assert_eq!(
            single_plan.target_plans[0].acquisition,
            TargetAcquisition::ProcessTree(ProcessTreeStrategy::NativeKernelCapture)
        );
        assert!(!single_plan.uses_software_mixer);
        assert_eq!(
            single_plan.overlap_semantics,
            OverlapCombinationSemantics::SingleTarget
        );

        let multi_req = CaptureRequest::new(vec![t1.clone(), t1.clone(), t2.clone()]);
        let multi_plan = multi_req.inspect_for_os(PlatformOs::Windows).unwrap();
        assert_eq!(multi_plan.selected_backend, Some(BackendId::Wasapi));
        assert_eq!(multi_plan.combine_resolution, CombineResolution::CspaMixer);
        assert!(multi_plan.uses_software_mixer);
        assert!(multi_plan.requires_discovery_subscription);
        assert_eq!(
            multi_plan.overlap_semantics,
            OverlapCombinationSemantics::SoftwareSummation
        );
        assert_eq!(multi_plan.duplicate_target_ids, vec![t1.id().clone()]);

        let native_only_dup = multi_req.clone().with_combine_mode(CombineMode::NativeOnly);
        assert_eq!(
            native_only_dup.inspect_for_os(PlatformOs::Windows),
            Err(PlanError::UnsupportedCapturePlan)
        );
        assert_eq!(
            native_only_dup.inspect_for_os(PlatformOs::Linux),
            Err(PlanError::UnsupportedCapturePlan)
        );

        let distinct_native =
            CaptureRequest::new(vec![t1, t2]).with_combine_mode(CombineMode::NativeOnly);
        assert_eq!(
            distinct_native.inspect_for_os(PlatformOs::Windows),
            Err(PlanError::UnsupportedCapturePlan)
        );
        let linux_plan = distinct_native.inspect_for_os(PlatformOs::Linux).unwrap();
        assert_eq!(linux_plan.selected_backend, Some(BackendId::PipeWire));
        assert_eq!(
            linux_plan.combine_resolution,
            CombineResolution::SharedNativeGraphMix(BackendId::PipeWire)
        );
        assert_eq!(
            linux_plan.overlap_semantics,
            OverlapCombinationSemantics::NativeSetUnion
        );
    }

    #[test]
    fn planner_enforces_encapsulation_multichannel_converter_and_readiness_abort_rules() {
        let mut forged = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        forged.id = "pipewire:default_input:follow".into(); // backend is still None
        assert_eq!(forged.validate(), Err(PlanError::InvalidTarget));

        let mut forged2 =
            CaptureTarget::microphone(DefaultDeviceMode::Follow).with_backend(BackendId::PipeWire);
        forged2.id = "pulseaudio:default_input:follow".into(); // mismatched backend prefix
        assert_eq!(forged2.validate(), Err(PlanError::InvalidTarget));

        let pinned_dev = CaptureTarget::microphone(DefaultDeviceMode::Pin)
            .with_backend(BackendId::CoreAudio)
            .with_gain(0.75)
            .unwrap()
            .resolve_pinned_endpoint(None, Some("BuiltInMic"));
        assert_eq!(pinned_dev.kind(), TargetKind::InputDevice);
        assert_eq!(pinned_dev.backend(), Some(BackendId::CoreAudio));
        assert_eq!(
            pinned_dev.id().as_ref(),
            "coreaudio:input_device:BuiltInMic"
        );
        assert_eq!(pinned_dev.clean_device_id(), Some("BuiltInMic"));
        assert!((pinned_dev.gain() - 0.75).abs() <= 1e-6);
        pinned_dev.validate().unwrap();
        assert!(
            pinned_dev
                .diagnostic_summary(DiagnosticOptions::default())
                .contains("id=<redacted>")
        );

        let mono_known = Format::new(
            SampleType::I16,
            ByteOrder::Little,
            16_000,
            1,
            Layout::Known(Arc::from([Channel::Mono])),
        )
        .unwrap();
        let req_mono =
            CaptureRequest::single(CaptureTarget::default_output(DefaultDeviceMode::Follow))
                .with_output_converter(Some(OutputConverterConfig::new(
                    mono_known,
                    DitherMode::Tpdf,
                )));
        assert!(req_mono.inspect_for_os(PlatformOs::Windows).is_ok());

        let surround_51 = Format::new(
            SampleType::I24,
            ByteOrder::Little,
            48_000,
            6,
            Layout::Known(Arc::from([
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::Center,
                Channel::LowFrequency,
                Channel::RearLeft,
                Channel::RearRight,
            ])),
        )
        .unwrap();
        let req_51 =
            CaptureRequest::single(CaptureTarget::default_output(DefaultDeviceMode::Follow))
                .with_output_converter(Some(OutputConverterConfig::new(
                    surround_51,
                    DitherMode::Tpdf,
                )));
        assert!(req_51.inspect_for_os(PlatformOs::Windows).is_ok());

        let unknown_layout = Format::new(
            SampleType::I16,
            ByteOrder::Little,
            16_000,
            1,
            Layout::Unknown,
        )
        .unwrap();
        let req_unknown =
            CaptureRequest::single(CaptureTarget::default_output(DefaultDeviceMode::Follow))
                .with_output_converter(Some(OutputConverterConfig::new(
                    unknown_layout,
                    DitherMode::Tpdf,
                )));
        assert_eq!(
            req_unknown.inspect_for_os(PlatformOs::Windows),
            Err(PlanError::UnsupportedCapturePlan)
        );

        let proc_target = CaptureTarget::process(
            ProcessIdentity {
                pid: 42,
                generation: 420,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_backend(BackendId::PulseAudio);
        let pa_req = CaptureRequest::single(proc_target.clone()).with_allow_reroute(true);
        let pa_plan = pa_req.inspect_for_os(PlatformOs::Linux).unwrap();
        assert_eq!(
            pa_plan.target_plans[0].acquisition,
            TargetAcquisition::ProcessTree(ProcessTreeStrategy::SoftwareStreamMix)
        );
        assert_eq!(pa_plan.combine_resolution, CombineResolution::CspaMixer);

        let unpinned_proc = CaptureTarget::process(
            ProcessIdentity {
                pid: 42,
                generation: 420,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap();
        let linux_req = CaptureRequest::single(unpinned_proc);
        let fb_plan = linux_req
            .resolve_plan_for_os_with_readiness(PlatformOs::Linux, |b, _| {
                if b == BackendId::PipeWire {
                    Err(PlanError::BackendUnavailable)
                } else {
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(
            fb_plan.target_plans[0].resolved_backend,
            BackendId::PulseAudio
        );

        let perm_err = linux_req.resolve_plan_for_os_with_readiness(PlatformOs::Linux, |b, _| {
            if b == BackendId::PipeWire {
                Err(PlanError::PermissionDenied)
            } else {
                Ok(())
            }
        });
        assert_eq!(perm_err, Err(PlanError::PermissionDenied));

        let win_proc = CaptureTarget::process(
            ProcessIdentity {
                pid: 99,
                generation: 990,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap();
        let ver_err = CaptureRequest::single(win_proc).resolve_plan_for_os_with_capability(
            PlatformOs::Windows,
            |b, _| {
                Err(CapabilityError::BackendVersionTooOld {
                    backend: b,
                    required: (10, 0, 20348),
                    observed: (10, 0, 19041),
                })
            },
        );
        assert_eq!(
            ver_err,
            Err(CapabilityError::BackendVersionTooOld {
                backend: BackendId::Wasapi,
                required: (10, 0, 20348),
                observed: (10, 0, 19041),
            })
        );

        let cap_fb = linux_req
            .resolve_plan_for_os_with_capability(PlatformOs::Linux, |b, _| {
                if b == BackendId::PipeWire {
                    Err(CapabilityError::ClientLibraryUnavailable {
                        backend: b,
                        detail: "libpipewire-0.3.so.0 missing".into(),
                    })
                } else {
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(
            cap_fb.target_plans[0].resolved_backend,
            BackendId::PulseAudio
        );
    }

    #[test]
    fn permission_domains_and_policy_resolve_target_requirements_across_platforms() {
        let mic = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        let out = CaptureTarget::default_output(DefaultDeviceMode::Follow);
        let app_exact = CaptureTarget::application(
            ProcessIdentity {
                pid: 700,
                generation: 7000,
            },
            Some("com.example.Zoom".into()),
            Some("Zoom".into()),
            None,
            ProcessScope::Exact,
            OnRootExit::Stop,
        )
        .unwrap();
        let app_tree = app_exact.clone().with_scope(ProcessScope::Tree);

        let req_default = CaptureRequest::new(vec![mic.clone(), app_exact.clone()]);
        assert_eq!(
            req_default.permission_policy(),
            PermissionPolicy::AutoRequest
        );
        let req_check_only = req_default
            .clone()
            .with_permission_policy(PermissionPolicy::CheckOnly);
        assert_eq!(
            req_check_only.permission_policy(),
            PermissionPolicy::CheckOnly
        );
        assert_eq!(
            req_check_only
                .inspect_for_os(PlatformOs::MacOs)
                .unwrap()
                .permission_policy,
            PermissionPolicy::CheckOnly
        );

        let mac_exact_kinds = req_default
            .required_permission_kinds_for_os(PlatformOs::MacOs)
            .unwrap();
        assert_eq!(
            mac_exact_kinds,
            BTreeSet::from([PermissionKind::Microphone, PermissionKind::ScreenRecording])
        );

        let mac_tree_kinds = CaptureRequest::new(vec![mic.clone(), app_tree])
            .required_permission_kinds_for_os(PlatformOs::MacOs)
            .unwrap();
        assert_eq!(
            mac_tree_kinds,
            BTreeSet::from([PermissionKind::Microphone, PermissionKind::SystemAudio])
        );

        let mac_13_kinds = required_permission_kinds_for_os_with_filter(
            std::slice::from_ref(&app_exact),
            &Backends::default(),
            ProcessTreeRange::default(),
            PlatformOs::MacOs,
            |b, tk| {
                b != BackendId::CoreAudio
                    || matches!(tk, TargetKind::DefaultInput | TargetKind::InputDevice)
            },
        )
        .unwrap();
        assert_eq!(
            mac_13_kinds,
            BTreeSet::from([PermissionKind::ScreenRecording])
        );

        assert_eq!(
            required_permission_kinds_for_os(
                std::slice::from_ref(&out),
                &Backends::default(),
                PlatformOs::Windows
            )
            .unwrap(),
            BTreeSet::from([PermissionKind::SystemAudio])
        );
        assert_eq!(
            required_permission_kinds_for_os(
                std::slice::from_ref(&mic),
                &Backends::default(),
                PlatformOs::Windows
            )
            .unwrap(),
            BTreeSet::from([PermissionKind::Microphone])
        );

        assert_eq!(
            required_permission_kinds_for_os(&[], &Backends::default(), PlatformOs::MacOs).unwrap(),
            BTreeSet::from([
                PermissionKind::Microphone,
                PermissionKind::SystemAudio,
                PermissionKind::ScreenRecording,
            ])
        );
        assert_eq!(
            required_permission_kinds_for_os(&[], &Backends::default(), PlatformOs::Windows)
                .unwrap(),
            BTreeSet::from([PermissionKind::Microphone, PermissionKind::SystemAudio])
        );
    }
}
