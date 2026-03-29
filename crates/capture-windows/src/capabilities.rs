use crate::core::{
    planner::{CapturePlanReport, CaptureRequest},
    schema::{
        BackendId, Backends, CaptureTarget, PlatformOs, ProcessTreeRange, TargetAcquisition,
        TargetKind,
    },
    traits::{CapabilityError, TimingRequirement},
};

pub const MIN_WINDOWS_ENDPOINT_BUILD: u32 = 19041;
pub const MIN_WINDOWS_PROCESS_LOOPBACK_BUILD: u32 = 20348;

pub const HRESULT_E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;
pub const HRESULT_ERROR_INVALID_PARAMETER: i32 = 0x8007_0057u32 as i32;
pub const HRESULT_ERROR_NOT_FOUND: i32 = 0x8007_0490u32 as i32;
pub const HRESULT_AUDCLNT_E_DEVICE_INVALIDATED: i32 = 0x8889_0004u32 as i32;
pub const HRESULT_AUDCLNT_E_DEVICE_IN_USE: i32 = 0x8889_000Au32 as i32;
pub const HRESULT_AUDCLNT_E_ENDPOINT_CREATE_FAILED: i32 = 0x8889_000Fu32 as i32;
pub const HRESULT_AUDCLNT_E_SERVICE_NOT_RUNNING: i32 = 0x8889_0010u32 as i32;

/// Map known WASAPI / MMDevice / Windows privacy `HRESULT` codes to structured `CapabilityError`s.
pub fn map_wasapi_hresult(hr: i32) -> Option<CapabilityError> {
    match hr {
        HRESULT_E_ACCESSDENIED => Some(CapabilityError::PermissionDenied),
        HRESULT_ERROR_NOT_FOUND
        | HRESULT_AUDCLNT_E_DEVICE_INVALIDATED
        | HRESULT_AUDCLNT_E_DEVICE_IN_USE
        | HRESULT_AUDCLNT_E_ENDPOINT_CREATE_FAILED
        | HRESULT_AUDCLNT_E_SERVICE_NOT_RUNNING => Some(CapabilityError::DeviceUnavailable),
        _ => None,
    }
}

#[cfg(windows)]
pub(crate) fn map_windows_error(err: windows::core::Error) -> CapabilityError {
    let hr = err.code().0;
    map_wasapi_hresult(hr).unwrap_or(CapabilityError::BackendFailure {
        backend: BackendId::Wasapi,
        code: hr,
    })
}

pub fn current_windows_build() -> u32 {
    #[cfg(windows)]
    {
        crate::process::windows_build_number().unwrap_or(26200)
    }
    #[cfg(not(windows))]
    {
        26200
    }
}

/// Enforce Windows build floors: 19041 for endpoints and microphones, 20348 for process loopback.
pub fn resolve_windows_plan_for_build(
    request: &CaptureRequest,
    timing: TimingRequirement,
    os_build: u32,
) -> Result<CapturePlanReport, CapabilityError> {
    if timing == TimingRequirement::Precise {
        return Err(CapabilityError::PreciseTimingUnqualified);
    }
    request.resolve_plan_for_os_with_capability(PlatformOs::Windows, |backend, target| {
        if os_build < MIN_WINDOWS_ENDPOINT_BUILD {
            return Err(CapabilityError::BackendVersionTooOld {
                backend,
                required: (10, 0, MIN_WINDOWS_ENDPOINT_BUILD),
                observed: (10, 0, os_build),
            });
        }
        if matches!(target.kind(), TargetKind::Process | TargetKind::Application)
            && os_build < MIN_WINDOWS_PROCESS_LOOPBACK_BUILD
        {
            return Err(CapabilityError::BackendVersionTooOld {
                backend,
                required: (10, 0, MIN_WINDOWS_PROCESS_LOOPBACK_BUILD),
                observed: (10, 0, os_build),
            });
        }
        Ok(())
    })
}

pub fn resolve_windows_plan(
    request: &CaptureRequest,
    timing: TimingRequirement,
) -> Result<CapturePlanReport, CapabilityError> {
    resolve_windows_plan_for_build(request, timing, current_windows_build())
}

pub fn check_capture_target_for_build(
    target: &CaptureTarget,
    tree_range: ProcessTreeRange,
    backends: &Backends,
    timing: TimingRequirement,
    os_build: u32,
) -> Result<TargetAcquisition, CapabilityError> {
    let req = CaptureRequest::single(target.clone())
        .with_tree_strategy(tree_range)
        .with_backends(backends.clone());
    let report = resolve_windows_plan_for_build(&req, timing, os_build)?;
    Ok(report.target_plans[0].acquisition)
}

pub fn check_capture_target(
    target: &CaptureTarget,
    tree_range: ProcessTreeRange,
    backends: &Backends,
    timing: TimingRequirement,
) -> Result<TargetAcquisition, CapabilityError> {
    check_capture_target_for_build(
        target,
        tree_range,
        backends,
        timing,
        current_windows_build(),
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::{
        PlatformStream,
        core::{
            ordering::Overflow,
            schema::{
                DefaultDeviceMode, OnRootExit, ProcessIdentity, ProcessScope, ProcessTreeStrategy,
            },
        },
    };

    #[test]
    fn ordinary_opens_enforce_scope_and_precision_rejections() {
        let identity = ProcessIdentity {
            pid: std::process::id(),
            generation: 0,
        };
        let capture_tree =
            CaptureTarget::process(identity, ProcessScope::Tree, OnRootExit::Stop).unwrap();
        assert_eq!(
            check_capture_target(
                &capture_tree,
                ProcessTreeRange::default(),
                &Backends::default(),
                TimingRequirement::Raw,
            ),
            Ok(TargetAcquisition::ProcessTree(
                ProcessTreeStrategy::NativeKernelCapture
            ))
        );
        let capture_exact =
            CaptureTarget::process(identity, ProcessScope::Exact, OnRootExit::Stop).unwrap();
        assert_eq!(
            check_capture_target(
                &capture_exact,
                ProcessTreeRange::default(),
                &Backends::default(),
                TimingRequirement::Raw,
            ),
            Err(CapabilityError::UnsupportedScope)
        );
        assert_eq!(
            check_capture_target(
                &capture_tree,
                ProcessTreeRange {
                    start_level: ProcessTreeStrategy::SoftwareStreamMix,
                    fallback_limit: Some(ProcessTreeStrategy::NativeKernelCapture),
                },
                &Backends::default(),
                TimingRequirement::Raw,
            ),
            Err(CapabilityError::ReversedStrategyRange)
        );
        assert_eq!(
            PlatformStream::open_with_options(
                &capture_exact,
                ProcessTreeRange::default(),
                &Backends::default(),
                Overflow::DropOldest,
                TimingRequirement::Raw,
            )
            .err(),
            Some(CapabilityError::UnsupportedScope)
        );
        let endpoint = CaptureTarget::default_output(DefaultDeviceMode::Pin);
        assert_eq!(
            check_capture_target(
                &endpoint,
                ProcessTreeRange::default(),
                &Backends::default(),
                TimingRequirement::Raw,
            ),
            Ok(TargetAcquisition::DirectEndpoint)
        );
        assert_eq!(
            PlatformStream::open_with_options(
                &endpoint,
                ProcessTreeRange::default(),
                &Backends::default(),
                Overflow::DropOldest,
                TimingRequirement::Precise,
            )
            .err(),
            Some(CapabilityError::PreciseTimingUnqualified)
        );
        assert_eq!(
            check_capture_target(
                &capture_tree,
                ProcessTreeRange {
                    start_level: ProcessTreeStrategy::NativeGraphMix,
                    fallback_limit: Some(ProcessTreeStrategy::SoftwareStreamMix),
                },
                &Backends::default(),
                TimingRequirement::Raw,
            ),
            Err(CapabilityError::NotImplemented)
        );
    }

    #[test]
    fn windows_10_vs_windows_11_build_floors_distinguish_endpoint_and_process_loopback() {
        let identity = ProcessIdentity {
            pid: std::process::id(),
            generation: 0,
        };
        let process_tree =
            CaptureTarget::process(identity, ProcessScope::Tree, OnRootExit::Stop).unwrap();
        let default_out = CaptureTarget::default_output(DefaultDeviceMode::Follow);
        let device_out = CaptureTarget::device("endpoint-0").unwrap();

        assert!(current_windows_build() >= MIN_WINDOWS_PROCESS_LOOPBACK_BUILD);

        assert_eq!(
            check_capture_target_for_build(
                &default_out,
                ProcessTreeRange::default(),
                &Backends::default(),
                TimingRequirement::Raw,
                18363,
            ),
            Err(CapabilityError::BackendVersionTooOld {
                backend: BackendId::Wasapi,
                required: (10, 0, MIN_WINDOWS_ENDPOINT_BUILD),
                observed: (10, 0, 18363),
            })
        );

        for build in [19041, 19044, 19045, 20347] {
            assert_eq!(
                check_capture_target_for_build(
                    &default_out,
                    ProcessTreeRange::default(),
                    &Backends::default(),
                    TimingRequirement::Raw,
                    build,
                ),
                Ok(TargetAcquisition::DirectEndpoint)
            );
            assert_eq!(
                check_capture_target_for_build(
                    &device_out,
                    ProcessTreeRange::default(),
                    &Backends::default(),
                    TimingRequirement::Raw,
                    build,
                ),
                Ok(TargetAcquisition::DirectEndpoint)
            );
            assert_eq!(
                check_capture_target_for_build(
                    &process_tree,
                    ProcessTreeRange::default(),
                    &Backends::default(),
                    TimingRequirement::Raw,
                    build,
                ),
                Err(CapabilityError::BackendVersionTooOld {
                    backend: BackendId::Wasapi,
                    required: (10, 0, MIN_WINDOWS_PROCESS_LOOPBACK_BUILD),
                    observed: (10, 0, build),
                })
            );
        }

        for build in [20348, 22000, 26100, 26200] {
            assert_eq!(
                check_capture_target_for_build(
                    &default_out,
                    ProcessTreeRange::default(),
                    &Backends::default(),
                    TimingRequirement::Raw,
                    build,
                ),
                Ok(TargetAcquisition::DirectEndpoint)
            );
            assert_eq!(
                check_capture_target_for_build(
                    &process_tree,
                    ProcessTreeRange::default(),
                    &Backends::default(),
                    TimingRequirement::Raw,
                    build,
                ),
                Ok(TargetAcquisition::ProcessTree(
                    ProcessTreeStrategy::NativeKernelCapture
                ))
            );
        }

        let mic_follow = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        let mic_dev = CaptureTarget::input_device("mic-endpoint-0").unwrap();
        assert_eq!(
            check_capture_target_for_build(
                &mic_follow,
                ProcessTreeRange {
                    start_level: ProcessTreeStrategy::NativeKernelCapture,
                    fallback_limit: None,
                },
                &Backends::default(),
                TimingRequirement::Raw,
                19041,
            ),
            Ok(TargetAcquisition::DirectEndpoint)
        );
        assert_eq!(
            check_capture_target_for_build(
                &mic_dev,
                ProcessTreeRange::default(),
                &Backends::default(),
                TimingRequirement::Raw,
                18363,
            ),
            Err(CapabilityError::BackendVersionTooOld {
                backend: BackendId::Wasapi,
                required: (10, 0, MIN_WINDOWS_ENDPOINT_BUILD),
                observed: (10, 0, 18363),
            })
        );
        let wrong_backend = mic_follow.with_backend(BackendId::PulseAudio);
        assert_eq!(
            check_capture_target_for_build(
                &wrong_backend,
                ProcessTreeRange::default(),
                &Backends::default(),
                TimingRequirement::Raw,
                26200,
            ),
            Err(CapabilityError::InvalidBackend)
        );
        assert_eq!(
            map_wasapi_hresult(HRESULT_E_ACCESSDENIED),
            Some(CapabilityError::PermissionDenied)
        );
        assert_eq!(
            map_wasapi_hresult(HRESULT_AUDCLNT_E_DEVICE_IN_USE),
            Some(CapabilityError::DeviceUnavailable)
        );
        assert_eq!(
            map_wasapi_hresult(HRESULT_ERROR_NOT_FOUND),
            Some(CapabilityError::DeviceUnavailable)
        );
        assert_eq!(map_wasapi_hresult(0), None);
    }
}
