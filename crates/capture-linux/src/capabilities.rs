#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::core::{
    planner::{CapturePlanReport, CaptureRequest},
    schema::{BackendId, PlatformOs},
    traits::{CapabilityError, TimingRequirement},
};

pub const MIN_PIPEWIRE_VERSION: (u32, u32, u32) = (0, 3, 65);
pub const MIN_PULSEAUDIO_VERSION: (u32, u32, u32) = (15, 0, 0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PulseServerKind {
    StandalonePulseAudio,
    PipeWirePulse,
}

impl PulseServerKind {
    pub fn classify(server_name: &str) -> Self {
        if server_name.contains("on PipeWire") || server_name.contains("pipewire") {
            Self::PipeWirePulse
        } else {
            Self::StandalonePulseAudio
        }
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StandalonePulseAudio => "StandalonePulseAudio",
            Self::PipeWirePulse => "PipeWirePulse",
        }
    }
}

/// Resolve the plan using per-backend readiness and preserve structured capability errors.
pub(crate) fn resolve_linux_plan_with_readiness(
    request: &CaptureRequest,
    timing: TimingRequirement,
    pipewire_readiness: Result<(), CapabilityError>,
    pulse_readiness: Result<(), CapabilityError>,
) -> Result<CapturePlanReport, CapabilityError> {
    let effective = if timing == TimingRequirement::Precise && !request.require_precise_timing() {
        request.clone().with_require_precise_timing(true)
    } else {
        request.clone()
    };
    effective.resolve_plan_for_os_with_capability(PlatformOs::Linux, |backend, _target| {
        match backend {
            BackendId::PipeWire => pipewire_readiness.clone(),
            BackendId::PulseAudio => pulse_readiness.clone(),
            _ => Err(CapabilityError::InvalidBackend),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema::{
        Backends, CaptureTarget, CombineMode, CombineResolution, DefaultDeviceMode, OnRootExit,
        ProcessIdentity, ProcessScope, ProcessTreeRange, ProcessTreeStrategy, TargetAcquisition,
    };

    #[test]
    fn waterfall_selects_pipewire_native_graph_mix_then_falls_back_to_pulse_cspa_mixer() {
        let target = CaptureTarget::process(
            ProcessIdentity {
                pid: 1000,
                generation: 5000,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap();
        let req = CaptureRequest::single(target.clone());

        let plan = resolve_linux_plan_with_readiness(&req, TimingRequirement::Raw, Ok(()), Ok(()))
            .unwrap();
        assert_eq!(plan.selected_backend, Some(BackendId::PipeWire));
        assert_eq!(
            plan.combine_resolution,
            CombineResolution::PassThrough(BackendId::PipeWire)
        );
        assert_eq!(
            plan.target_plans[0].acquisition,
            TargetAcquisition::ProcessTree(ProcessTreeStrategy::NativeGraphMix)
        );

        let fallback_plan = resolve_linux_plan_with_readiness(
            &req,
            TimingRequirement::Raw,
            Err(CapabilityError::ClientLibraryUnavailable {
                backend: BackendId::PipeWire,
                detail: "library absent: libpipewire-0.3.so.0".into(),
            }),
            Ok(()),
        )
        .unwrap();
        assert_eq!(fallback_plan.selected_backend, Some(BackendId::PulseAudio));
        assert_eq!(
            fallback_plan.combine_resolution,
            CombineResolution::CspaMixer
        );
        assert_eq!(
            fallback_plan.target_plans[0].acquisition,
            TargetAcquisition::ProcessTree(ProcessTreeStrategy::SoftwareStreamMix)
        );

        let pw_only = Backends {
            linux: vec![BackendId::PipeWire],
            ..Backends::default()
        };
        assert_eq!(
            resolve_linux_plan_with_readiness(
                &req.clone().with_backends(pw_only),
                TimingRequirement::Raw,
                Err(CapabilityError::ServerUnavailable),
                Ok(()),
            ),
            Err(CapabilityError::ServerUnavailable)
        );

        for fatal in [
            CapabilityError::PermissionDenied,
            CapabilityError::PidReused,
            CapabilityError::NoSuchProcess,
        ] {
            assert_eq!(
                resolve_linux_plan_with_readiness(
                    &req,
                    TimingRequirement::Raw,
                    Err(fatal.clone()),
                    Ok(()),
                ),
                Err(fatal)
            );
        }

        assert_eq!(
            resolve_linux_plan_with_readiness(&req, TimingRequirement::Precise, Ok(()), Ok(())),
            Err(CapabilityError::PreciseTimingUnqualified)
        );
    }

    #[test]
    fn multi_backend_plan_supports_input_devices_backend_pinning_and_cross_backend_cspa_mixer() {
        let pw_proc = CaptureTarget::process(
            ProcessIdentity {
                pid: 2000,
                generation: 9000,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap();
        let pa_mic = CaptureTarget::microphone(DefaultDeviceMode::Follow)
            .with_backend(BackendId::PulseAudio);

        let single_pa_mic = resolve_linux_plan_with_readiness(
            &CaptureRequest::single(pa_mic.clone()),
            TimingRequirement::Raw,
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(single_pa_mic.selected_backend, Some(BackendId::PulseAudio));
        assert_eq!(
            single_pa_mic.combine_resolution,
            CombineResolution::PassThrough(BackendId::PulseAudio)
        );
        assert_eq!(
            single_pa_mic.target_plans[0].acquisition,
            TargetAcquisition::DirectEndpoint
        );

        let cross_plan = resolve_linux_plan_with_readiness(
            &CaptureRequest::new(vec![pw_proc, pa_mic]),
            TimingRequirement::Raw,
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(cross_plan.selected_backend, None);
        assert_eq!(cross_plan.combine_resolution, CombineResolution::CspaMixer);
        assert_eq!(
            cross_plan.target_plans[0].resolved_backend,
            BackendId::PipeWire
        );
        assert_eq!(
            cross_plan.target_plans[0].acquisition,
            TargetAcquisition::ProcessTree(ProcessTreeStrategy::NativeGraphMix)
        );
        assert_eq!(
            cross_plan.target_plans[1].resolved_backend,
            BackendId::PulseAudio
        );
        assert_eq!(
            cross_plan.target_plans[1].acquisition,
            TargetAcquisition::DirectEndpoint
        );

        let native_only_req = CaptureRequest::new(vec![
            CaptureTarget::default_output(DefaultDeviceMode::Follow),
            CaptureTarget::microphone(DefaultDeviceMode::Follow)
                .with_backend(BackendId::PulseAudio),
        ])
        .with_tree_strategy(ProcessTreeRange::default())
        .with_combine_mode(CombineMode::NativeOnly);
        assert_eq!(
            resolve_linux_plan_with_readiness(
                &native_only_req,
                TimingRequirement::Raw,
                Ok(()),
                Ok(()),
            ),
            Err(CapabilityError::NotImplemented)
        );
    }
}
