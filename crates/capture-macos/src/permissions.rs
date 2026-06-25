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

