#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::{
    capabilities::{
        MIN_COREAUDIO_INPUT_OS_VERSION, MIN_COREAUDIO_TAP_OS_VERSION,
        MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION, check_os_version_floor_for_target,
    },
    core::{
        planner::required_permission_kinds_for_os_with_filter,
        schema::{
            Backends, CaptureTarget, PermissionEntry, PermissionKind, PermissionPolicy,
            PermissionReport, PermissionState, PlatformOs, ProcessTreeRange,
        },
        traits::{CapabilityError, PermissionsBackend},
    },
};
use std::{
    sync::atomic::{AtomicBool, AtomicU8, Ordering::SeqCst},
    time::Duration,
};

static SCK_PROMPT_ATTEMPTED: AtomicBool = AtomicBool::new(false);
// 0 = Unprobed, 1 = Verified Granted, 2 = Denied
static SYSTEM_AUDIO_TAP_CACHE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn record_system_audio_tap_verified_granted() {
    SYSTEM_AUDIO_TAP_CACHE.store(1, SeqCst);
}

pub fn classify_macos_mic_permission(
    auth_status: i32,
    missing_usage_key: bool,
    is_interactive: bool,
    os_ver: (u32, u32, u32),
    required: bool,
) -> PermissionEntry {
    if os_ver < MIN_COREAUDIO_INPUT_OS_VERSION || missing_usage_key {
        return PermissionEntry {
            kind: PermissionKind::Microphone,
            state: PermissionState::Restricted,
            required,
            can_prompt: false,
            requires_relaunch: false,
        };
    }
    let (state, can_prompt) = match auth_status {
        3 => (PermissionState::Granted, false),
        2 => (PermissionState::Denied, false),
        1 => (PermissionState::Restricted, false),
        0 => {
            if is_interactive {
                (PermissionState::NotDetermined, true)
            } else {
                (PermissionState::Restricted, false)
            }
        }
        _ => (PermissionState::Restricted, false),
    };
    PermissionEntry {
        kind: PermissionKind::Microphone,
        state,
        required,
        can_prompt,
        requires_relaunch: false,
    }
}

pub fn classify_macos_sck_permission(
    preflight_granted: bool,
    prompt_attempted: bool,
    relaunch_needed: bool,
    is_interactive: bool,
    os_ver: (u32, u32, u32),
    required: bool,
) -> PermissionEntry {
    if os_ver < MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION {
        return PermissionEntry {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::Restricted,
            required,
            can_prompt: false,
            requires_relaunch: false,
        };
    }
    if preflight_granted {
        return PermissionEntry {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::Granted,
            required,
            can_prompt: false,
            requires_relaunch: relaunch_needed,
        };
    }
    if is_interactive && !prompt_attempted {
        PermissionEntry {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::NotDetermined,
            required,
            can_prompt: true,
            requires_relaunch: true,
        }
    } else {
        PermissionEntry {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::Denied,
            required,
            can_prompt: false,
            requires_relaunch: true,
        }
    }
}

/// Never claims `Granted` unless tap description write succeeded without `'!hog'` / `'wat?'`.
pub fn classify_macos_tap_permission(
    preflight_code: i32,
    verified_cache: u8,
    is_interactive: bool,
    os_ver: (u32, u32, u32),
    required: bool,
) -> PermissionEntry {
    if os_ver < MIN_COREAUDIO_TAP_OS_VERSION || preflight_code == -2 {
        return PermissionEntry {
            kind: PermissionKind::SystemAudio,
            state: PermissionState::Restricted,
            required,
            can_prompt: false,
            requires_relaunch: false,
        };
    }
    if preflight_code == 2 || verified_cache == 2 {
        return PermissionEntry {
            kind: PermissionKind::SystemAudio,
            state: PermissionState::Denied,
            required,
            can_prompt: false,
            requires_relaunch: false,
        };
    }
    if verified_cache == 1 || preflight_code == 3 {
        return PermissionEntry {
            kind: PermissionKind::SystemAudio,
            state: PermissionState::Granted,
            required,
            can_prompt: false,
            requires_relaunch: false,
        };
    }
    PermissionEntry {
        kind: PermissionKind::SystemAudio,
        state: PermissionState::Unknown,
        required,
        can_prompt: is_interactive,
        requires_relaunch: false,
    }
}

#[cfg(target_os = "macos")]
fn current_macos_os_version() -> (u32, u32, u32) {
    crate::native_bridge::os_version()
}

#[cfg(not(target_os = "macos"))]
fn current_macos_os_version() -> (u32, u32, u32) {
    (15, 7, 9)
}

pub(crate) fn maybe_auto_request_sck_permission(policy: PermissionPolicy) {
    #[cfg(target_os = "macos")]
    if policy.allows_prompt()
        && !crate::native_bridge::sck_preflight_granted()
        && !SCK_PROMPT_ATTEMPTED.load(SeqCst)
        && crate::native_bridge::is_interactive_gui_session()
    {
        SCK_PROMPT_ATTEMPTED.store(true, SeqCst);
        let _ = crate::native_bridge::sck_request_access();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = policy;
}

fn build_macos_report(kinds: &[PermissionKind], relaunch_sck: bool) -> PermissionReport {
    let os_ver = current_macos_os_version();
    #[cfg(target_os = "macos")]
    let (is_interactive, mic_auth, mic_missing_key, sck_granted, tap_pre) = (
        crate::native_bridge::is_interactive_gui_session(),
        crate::native_bridge::microphone_auth_status(),
        crate::native_bridge::bundle_missing_usage_description("NSMicrophoneUsageDescription"),
        crate::native_bridge::sck_preflight_granted(),
        crate::native_bridge::system_audio_tap_preflight(),
    );
    #[cfg(not(target_os = "macos"))]
    let (is_interactive, mic_auth, mic_missing_key, sck_granted, tap_pre) =
        (false, 0, false, false, 0);

    let mic_entry = classify_macos_mic_permission(
        mic_auth,
        mic_missing_key,
        is_interactive,
        os_ver,
        kinds.contains(&PermissionKind::Microphone),
    );
    let tap_entry = classify_macos_tap_permission(
        tap_pre,
        SYSTEM_AUDIO_TAP_CACHE.load(SeqCst),
        is_interactive,
        os_ver,
        kinds.contains(&PermissionKind::SystemAudio),
    );
    let sck_entry = classify_macos_sck_permission(
        sck_granted,
        SCK_PROMPT_ATTEMPTED.load(SeqCst),
        relaunch_sck,
        is_interactive,
        os_ver,
        kinds.contains(&PermissionKind::ScreenRecording),
    );
    PermissionReport::new(PlatformOs::MacOs, mic_entry, tap_entry, sck_entry)
}

fn resolve_macos_required_kinds(
    targets: &[CaptureTarget],
    backends: &Backends,
) -> Result<Vec<PermissionKind>, CapabilityError> {
    let os_ver = current_macos_os_version();
    let set = required_permission_kinds_for_os_with_filter(
        targets,
        backends,
        ProcessTreeRange::default(),
        PlatformOs::MacOs,
        |backend, target_kind| {
            check_os_version_floor_for_target(backend, target_kind, os_ver).is_ok()
        },
    )?;
    Ok(set.into_iter().collect())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformPermissions;

impl PermissionsBackend for PlatformPermissions {
    fn check_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        let kinds = resolve_macos_required_kinds(targets, backends)?;
        Self::check_permission_kinds(&kinds, backends)
    }

    fn request_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        let kinds = resolve_macos_required_kinds(targets, backends)?;
        Self::request_permission_kinds(&kinds, backends, timeout, open_settings_if_denied)
    }

    fn check_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        backends.validate()?;
        Ok(build_macos_report(kinds, false))
    }

    fn request_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        backends.validate()?;
        let mut sck_just_transitioned = false;
        #[cfg(target_os = "macos")]
        {
            let timeout_s = timeout.as_secs_f64().clamp(0.1, 120.0);
            if kinds.contains(&PermissionKind::Microphone) {
                let res = crate::native_bridge::request_microphone_access(timeout_s);
                if open_settings_if_denied && res == 2 {
                    let _ = crate::native_bridge::open_privacy_settings(0);
                }
            }
            if kinds.contains(&PermissionKind::SystemAudio) {
                let tap_res = crate::native_bridge::request_system_audio_tap();
                if tap_res == 3 {
                    SYSTEM_AUDIO_TAP_CACHE.store(1, SeqCst);
                } else if tap_res == 2 {
                    SYSTEM_AUDIO_TAP_CACHE.store(2, SeqCst);
                    if open_settings_if_denied {
                        let _ = crate::native_bridge::open_privacy_settings(1);
                    }
                }
            }
            if kinds.contains(&PermissionKind::ScreenRecording) {
                let was_granted = crate::native_bridge::sck_preflight_granted();
                if !was_granted {
                    SCK_PROMPT_ATTEMPTED.store(true, SeqCst);
                    let now_granted = crate::native_bridge::sck_request_access();
                    sck_just_transitioned = !was_granted && now_granted;
                    if !now_granted && open_settings_if_denied {
                        let _ = crate::native_bridge::open_privacy_settings(2);
                    }
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (timeout, open_settings_if_denied, &mut sck_just_transitioned);

        Ok(build_macos_report(kinds, sck_just_transitioned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_permission_classifiers_guard_missing_plist_keys_wat_hog_and_sck_relaunch() {
        let restricted_mic = classify_macos_mic_permission(0, true, true, (15, 7, 9), true);
        assert_eq!(restricted_mic.state, PermissionState::Restricted);
        assert!(!restricted_mic.can_prompt);

        let interactive_mic = classify_macos_mic_permission(0, false, true, (15, 7, 9), true);
        assert_eq!(interactive_mic.state, PermissionState::NotDetermined);
        assert!(interactive_mic.can_prompt);

        let ssh_mic = classify_macos_mic_permission(0, false, false, (15, 7, 9), true);
        assert_eq!(ssh_mic.state, PermissionState::Restricted);
        assert!(!ssh_mic.can_prompt);

        let unprobed_tap = classify_macos_tap_permission(0, 0, true, (15, 7, 9), true);
        assert_eq!(unprobed_tap.state, PermissionState::Unknown);
        assert!(unprobed_tap.can_prompt);

        let denied_tap = classify_macos_tap_permission(2, 0, true, (15, 7, 9), true);
        assert_eq!(denied_tap.state, PermissionState::Denied);
        assert!(!denied_tap.can_prompt);

        let verified_tap = classify_macos_tap_permission(0, 1, true, (15, 7, 9), true);
        assert_eq!(verified_tap.state, PermissionState::Granted);
        assert!(!verified_tap.can_prompt);

        let old_os_tap = classify_macos_tap_permission(0, 0, true, (13, 6, 0), true);
        assert_eq!(old_os_tap.state, PermissionState::Restricted);

        let unprompted_sck =
            classify_macos_sck_permission(false, false, false, true, (15, 7, 9), true);
        assert_eq!(unprompted_sck.state, PermissionState::NotDetermined);
        assert!(unprompted_sck.can_prompt);
        assert!(unprompted_sck.requires_relaunch);

        let post_prompt_denied =
            classify_macos_sck_permission(false, true, false, true, (15, 7, 9), true);
        assert_eq!(post_prompt_denied.state, PermissionState::Denied);
        assert!(!post_prompt_denied.can_prompt);

        let granted_sck = classify_macos_sck_permission(true, false, false, true, (15, 7, 9), true);
        assert_eq!(granted_sck.state, PermissionState::Granted);
        assert!(!granted_sck.requires_relaunch);
    }
}
