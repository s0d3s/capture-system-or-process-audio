#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::{
    core::{
        planner::required_permission_kinds_for_os,
        schema::{
            BackendId, Backends, CaptureTarget, PermissionEntry, PermissionKind, PermissionReport,
            PermissionState, PlatformOs,
        },
        traits::{CapabilityError, PermissionsBackend},
    },
    loader::{LinuxClientPaths, probe_pipewire_server, probe_pulse_server},
};
use std::time::Duration;

/// Classify PipeWire / PulseAudio socket and Flatpak sandbox access.
pub fn classify_linux_permission_state(
    any_server_reachable: bool,
    any_socket_permission_denied: bool,
    in_flatpak_without_audio_socket: bool,
) -> PermissionState {
    if any_server_reachable {
        PermissionState::Granted
    } else if any_socket_permission_denied || in_flatpak_without_audio_socket {
        PermissionState::Restricted
    } else {
        PermissionState::Unknown
    }
}

#[cfg(target_os = "linux")]
fn check_unix_socket_permission_denied(check_pw: bool, check_pa: bool) -> (bool, bool) {
    use std::{io::ErrorKind, os::unix::net::UnixStream, path::Path, path::PathBuf};

    let mut candidates: Vec<PathBuf> = Vec::new();
    if check_pw {
        if let Ok(pw_dir) = std::env::var("PIPEWIRE_RUNTIME_DIR") {
            candidates.push(PathBuf::from(pw_dir).join("pipewire-0"));
        } else if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            candidates.push(PathBuf::from(xdg).join("pipewire-0"));
        }
    }
    if check_pa {
        if let Ok(pa_dir) = std::env::var("PULSE_RUNTIME_PATH") {
            candidates.push(PathBuf::from(pa_dir).join("native"));
        } else if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            candidates.push(PathBuf::from(&xdg).join("pulse").join("native"));
        }
    }

    let mut any_eacces = false;
    let mut any_socket_exists = false;
    for sock_path in &candidates {
        match std::fs::symlink_metadata(sock_path) {
            Ok(_) => {
                any_socket_exists = true;
                if let Err(err) = UnixStream::connect(sock_path)
                    && err.kind() == ErrorKind::PermissionDenied
                {
                    any_eacces = true;
                }
            }
            Err(err) if err.kind() == ErrorKind::PermissionDenied => {
                any_eacces = true;
            }
            Err(_) => {}
        }
    }

    let in_flatpak = Path::new("/.flatpak-info").exists();
    (any_eacces, in_flatpak && !any_socket_exists)
}

pub(crate) fn probe_linux_permission_state_with_paths(
    backends: &Backends,
    paths: &LinuxClientPaths,
) -> PermissionState {
    let linux_backends = backends.for_os(PlatformOs::Linux);
    let check_pw = linux_backends.contains(&BackendId::PipeWire);
    let check_pa = linux_backends.contains(&BackendId::PulseAudio);

    let pw_ok = check_pw && probe_pipewire_server(paths).is_ok();
    let pa_ok = check_pa && probe_pulse_server(paths).is_ok();
    let any_ok = pw_ok || pa_ok;

    if any_ok {
        return PermissionState::Granted;
    }

    #[cfg(target_os = "linux")]
    let (eacces, flatpak_isolated) = check_unix_socket_permission_denied(check_pw, check_pa);
    #[cfg(not(target_os = "linux"))]
    let (eacces, flatpak_isolated) = (false, false);

    classify_linux_permission_state(false, eacces, flatpak_isolated)
}

pub(crate) fn ensure_linux_socket_permissions(
    backends: &Backends,
    paths: &LinuxClientPaths,
) -> Result<(), CapabilityError> {
    if probe_linux_permission_state_with_paths(backends, paths) == PermissionState::Restricted {
        return Err(CapabilityError::PermissionDenied);
    }
    Ok(())
}

fn build_linux_report(kinds: &[PermissionKind], backends: &Backends) -> PermissionReport {
    let paths = LinuxClientPaths::default();
    let state = probe_linux_permission_state_with_paths(backends, &paths);
    PermissionReport::new(
        PlatformOs::Linux,
        PermissionEntry {
            kind: PermissionKind::Microphone,
            state,
            required: kinds.contains(&PermissionKind::Microphone),
            can_prompt: false,
            requires_relaunch: false,
        },
        PermissionEntry {
            kind: PermissionKind::SystemAudio,
            state,
            required: kinds.contains(&PermissionKind::SystemAudio),
            can_prompt: false,
            requires_relaunch: false,
        },
        PermissionEntry {
            kind: PermissionKind::ScreenRecording,
            state,
            required: kinds.contains(&PermissionKind::ScreenRecording),
            can_prompt: false,
            requires_relaunch: false,
        },
    )
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformPermissions;

impl PermissionsBackend for PlatformPermissions {
    fn check_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        let req = required_permission_kinds_for_os(targets, backends, PlatformOs::Linux)?;
        let kinds: Vec<PermissionKind> = req.into_iter().collect();
        Self::check_permission_kinds(&kinds, backends)
    }

    fn request_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        let req = required_permission_kinds_for_os(targets, backends, PlatformOs::Linux)?;
        let kinds: Vec<PermissionKind> = req.into_iter().collect();
        Self::request_permission_kinds(&kinds, backends, timeout, open_settings_if_denied)
    }

    fn check_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        backends.validate()?;
        Ok(build_linux_report(kinds, backends))
    }

    fn request_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
        _timeout: Duration,
        _open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        backends.validate()?;
        Ok(build_linux_report(kinds, backends))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema::DefaultDeviceMode;

    #[test]
    fn linux_permission_classifier_distinguishes_granted_sandbox_restricted_and_daemon_unknown() {
        assert_eq!(
            classify_linux_permission_state(true, false, false),
            PermissionState::Granted
        );
        assert_eq!(
            classify_linux_permission_state(false, true, false),
            PermissionState::Restricted
        );
        assert_eq!(
            classify_linux_permission_state(false, false, true),
            PermissionState::Restricted
        );
        assert_eq!(
            classify_linux_permission_state(false, false, false),
            PermissionState::Unknown
        );

        let mic_target = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        let rep = PlatformPermissions::check_permissions(
            std::slice::from_ref(&mic_target),
            &Backends::default(),
        )
        .unwrap();
        assert!(rep.microphone().required);
        assert!(!rep.system_audio().required);
        assert!(!rep.microphone().can_prompt);
    }
}
