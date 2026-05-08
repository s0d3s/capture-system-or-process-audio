#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::core::{
    planner::{CapturePlanReport, CaptureRequest},
    schema::{BackendId, PlatformOs, TargetKind},
    traits::{CapabilityError, TimingRequirement},
};

pub const MIN_COREAUDIO_TAP_OS_VERSION: (u32, u32, u32) = (14, 2, 0);
pub const MIN_COREAUDIO_INPUT_OS_VERSION: (u32, u32, u32) = (13, 0, 0);
pub const MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION: (u32, u32, u32) = (13, 0, 0);

/// Evaluate macOS `AVMediaTypeAudio` (`AVAuthorizationStatus`) TCC consent without prompting.
/// - `1` (`Restricted`) -> `PermissionDenied`
/// - `2` (`Denied`) -> `PermissionDenied`
/// - `3` (`Authorized`) -> `Ok(())`
/// - `0` (`NotDetermined`) -> allowed on synthetic/virtual aggregates or interactive desktop sessions
///   (where `open` requests consent); rejected immediately on headless SSH physical microphone opens.
#[cfg(test)]
pub fn check_microphone_tcc_status(
    auth_status: i32,
    is_headless_ssh: bool,
    is_virtual_device: bool,
) -> Result<(), CapabilityError> {
    match auth_status {
        3 => Ok(()),
        1 | 2 => Err(CapabilityError::PermissionDenied),
        0 => {
            if is_virtual_device || !is_headless_ssh {
                Ok(())
            } else {
                Err(CapabilityError::PermissionDenied)
            }
        }
        _ => Err(CapabilityError::PermissionDenied),
    }
}

pub fn check_os_version_floor_for_target(
    backend: BackendId,
    target_kind: TargetKind,
    os_version: (u32, u32, u32),
) -> Result<(), CapabilityError> {
    match backend {
        BackendId::CoreAudio => {
            let required = if matches!(
                target_kind,
                TargetKind::DefaultInput | TargetKind::InputDevice
            ) {
                MIN_COREAUDIO_INPUT_OS_VERSION
            } else {
                MIN_COREAUDIO_TAP_OS_VERSION
            };
            if os_version >= required {
                Ok(())
            } else {
                Err(CapabilityError::BackendVersionTooOld {
                    backend,
                    required,
                    observed: os_version,
                })
            }
        }
        BackendId::ScreenCaptureKit => {
            if os_version >= MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION {
                Ok(())
            } else {
                Err(CapabilityError::BackendVersionTooOld {
                    backend,
                    required: MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION,
                    observed: os_version,
                })
            }
        }
        _ => Err(CapabilityError::InvalidBackend),
    }
}

/// Resolve a macOS capture plan using OS version floors and per-backend readiness probes.
pub fn resolve_macos_plan_with_readiness(
    request: &CaptureRequest,
    timing: TimingRequirement,
    os_version: (u32, u32, u32),
    coreaudio_tap_readiness: Result<(), CapabilityError>,
    coreaudio_input_readiness: Result<(), CapabilityError>,
    sck_readiness: Result<(), CapabilityError>,
) -> Result<CapturePlanReport, CapabilityError> {
    if timing == TimingRequirement::Precise {
        return Err(CapabilityError::PreciseTimingUnqualified);
    }
    request.resolve_plan_for_os_with_capability(PlatformOs::MacOs, |backend, target| {
        check_os_version_floor_for_target(backend, target.kind(), os_version)?;
        match backend {
            BackendId::CoreAudio => {
                if matches!(
                    target.kind(),
                    TargetKind::DefaultInput | TargetKind::InputDevice
                ) {
                    coreaudio_input_readiness.clone()
                } else {
                    coreaudio_tap_readiness.clone()
                }
            }
            BackendId::ScreenCaptureKit => sck_readiness.clone(),
            _ => Err(CapabilityError::InvalidBackend),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema::{
        Backends, CaptureTarget, CombineResolution, DefaultDeviceMode, OnRootExit, ProcessIdentity,
        ProcessScope,
    };

    #[test]
    fn waterfall_selects_sck_for_exact_app_and_coreaudio_for_tree_app() {
        let app_exact = CaptureTarget::application(
            ProcessIdentity {
                pid: 2000,
                generation: 99_000,
            },
            Some("app.bundle.test".into()),
            Some("TestApp".into()),
            None,
            ProcessScope::Exact,
            OnRootExit::Stop,
        )
        .unwrap();

        let sel_exact = resolve_macos_plan_with_readiness(
            &CaptureRequest::single(app_exact.clone()),
            TimingRequirement::Raw,
            (15, 7, 9),
            Ok(()),
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(
            sel_exact.selected_backend,
            Some(BackendId::ScreenCaptureKit)
        );
        assert_eq!(
            sel_exact.combine_resolution,
            CombineResolution::PassThrough(BackendId::ScreenCaptureKit)
        );

        let app_tree = app_exact.clone().with_scope(ProcessScope::Tree);
        let sel_tree = resolve_macos_plan_with_readiness(
            &CaptureRequest::single(app_tree),
            TimingRequirement::Raw,
            (15, 7, 9),
            Ok(()),
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(sel_tree.selected_backend, Some(BackendId::CoreAudio));
        assert_eq!(
            sel_tree.combine_resolution,
            CombineResolution::PassThrough(BackendId::CoreAudio)
        );

        let denied = resolve_macos_plan_with_readiness(
            &CaptureRequest::single(app_exact.clone()),
            TimingRequirement::Raw,
            (15, 7, 9),
            Ok(()),
            Ok(()),
            Err(CapabilityError::PermissionDenied),
        );
        assert_eq!(denied, Err(CapabilityError::PermissionDenied));

        let sel_13_6 = resolve_macos_plan_with_readiness(
            &CaptureRequest::single(app_exact),
            TimingRequirement::Raw,
            (13, 6, 0),
            Ok(()),
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(sel_13_6.selected_backend, Some(BackendId::ScreenCaptureKit));
    }

    #[test]
    fn multi_backend_plan_supports_13_0_hal_input_tcc_pins_and_sck_coreaudio_cspa_mixer() {
        assert_eq!(check_microphone_tcc_status(3, true, false), Ok(()));
        assert_eq!(
            check_microphone_tcc_status(1, false, false),
            Err(CapabilityError::PermissionDenied)
        );
        assert_eq!(
            check_microphone_tcc_status(2, false, true),
            Err(CapabilityError::PermissionDenied)
        );
        assert_eq!(
            check_microphone_tcc_status(0, true, false),
            Err(CapabilityError::PermissionDenied)
        );
        assert_eq!(check_microphone_tcc_status(0, true, true), Ok(()));
        assert_eq!(check_microphone_tcc_status(0, false, false), Ok(()));

        let app_exact = CaptureTarget::application(
            ProcessIdentity {
                pid: 4100,
                generation: 123_456,
            },
            Some("dev.cspa.app".into()),
            Some("CSPAApp".into()),
            None,
            ProcessScope::Exact,
            OnRootExit::Stop,
        )
        .unwrap();
        let mic_follow = CaptureTarget::microphone(DefaultDeviceMode::Follow);

        let plan_13_6 = resolve_macos_plan_with_readiness(
            &CaptureRequest::new(vec![app_exact.clone(), mic_follow.clone()]),
            TimingRequirement::Raw,
            (13, 6, 0),
            Ok(()),
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(plan_13_6.combine_resolution, CombineResolution::CspaMixer);
        assert_eq!(
            plan_13_6.target_plans[0].resolved_backend,
            BackendId::ScreenCaptureKit
        );
        assert_eq!(
            plan_13_6.target_plans[1].resolved_backend,
            BackendId::CoreAudio
        );

        let proc_tree = CaptureTarget::process(
            ProcessIdentity {
                pid: 4101,
                generation: 123_457,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap();
        let err_proc_13_6 = resolve_macos_plan_with_readiness(
            &CaptureRequest::single(proc_tree.clone()),
            TimingRequirement::Raw,
            (13, 6, 0),
            Ok(()),
            Ok(()),
            Ok(()),
        )
        .err();
        assert!(matches!(
            err_proc_13_6,
            Some(CapabilityError::BackendVersionTooOld {
                backend: BackendId::CoreAudio,
                required: (14, 2, 0),
                observed: (13, 6, 0),
            })
        ));

        let pinned_ca_app = app_exact.with_backend(BackendId::CoreAudio);
        let plan_pinned = resolve_macos_plan_with_readiness(
            &CaptureRequest::new(vec![pinned_ca_app, mic_follow]),
            TimingRequirement::Raw,
            (15, 7, 9),
            Ok(()),
            Ok(()),
            Ok(()),
        )
        .unwrap();
        assert_eq!(plan_pinned.combine_resolution, CombineResolution::CspaMixer);
        assert_eq!(
            plan_pinned.target_plans[0].resolved_backend,
            BackendId::CoreAudio
        );
        assert_eq!(
            plan_pinned.target_plans[1].resolved_backend,
            BackendId::CoreAudio
        );

        assert_eq!(
            resolve_macos_plan_with_readiness(
                &CaptureRequest::single(proc_tree),
                TimingRequirement::Precise,
                (15, 7, 9),
                Ok(()),
                Ok(()),
                Ok(()),
            ),
            Err(CapabilityError::PreciseTimingUnqualified)
        );

        let bad_pin = CaptureTarget::input_device("BuiltInMicrophoneDevice")
            .unwrap()
            .with_backend(BackendId::Wasapi);
        assert_eq!(
            resolve_macos_plan_with_readiness(
                &CaptureRequest::single(bad_pin).with_backends(Backends::default()),
                TimingRequirement::Raw,
                (15, 7, 9),
                Ok(()),
                Ok(()),
                Ok(()),
            )
            .err(),
            Some(CapabilityError::InvalidBackend)
        );
    }
}
