use crate::{
    capabilities::MIN_WINDOWS_ENDPOINT_BUILD,
    core::{
        planner::required_permission_kinds_for_os,
        schema::{
            Backends, CaptureTarget, PermissionEntry, PermissionKind, PermissionReport,
            PermissionState, PlatformOs, TargetKind,
        },
        traits::{CapabilityError, PermissionsBackend},
    },
};
use std::time::Duration;

/// Evaluate Windows 10/11 microphone privacy settings:
/// 1. Group Policy / MDM `HKLM\SOFTWARE\Policies\Microsoft\Windows\AppPrivacy\LetAppsAccessMicrophone`:
///    - `Some(2)` (Force Deny) -> `PermissionState::Restricted`
///    - `Some(1)` (Force Allow) -> `PermissionState::Granted`
/// 2. Machine `HKLM\...\CapabilityAccessManager\ConsentStore\microphone` `"Value" == "Deny"` -> `Denied`
/// 3. User `HKCU\...\CapabilityAccessManager\ConsentStore\microphone` `"Value" == "Deny"` -> `Denied`
/// 4. Win32 Desktop `HKCU\...\ConsentStore\microphone\NonPackaged` `"Value" == "Deny"` -> `Denied`
/// 5. Otherwise (`"Allow"` or unconfigured default) -> `PermissionState::Granted`.
pub fn evaluate_windows_mic_consent(
    policy_let_apps_access: Option<u32>,
    machine_value: Option<&str>,
    user_value: Option<&str>,
    non_packaged_value: Option<&str>,
) -> PermissionState {
    match policy_let_apps_access {
        Some(2) => return PermissionState::Restricted,
        Some(1) => return PermissionState::Granted,
        _ => {}
    }
    for val in [machine_value, user_value, non_packaged_value]
        .into_iter()
        .flatten()
    {
        if val.trim().eq_ignore_ascii_case("deny") {
            return PermissionState::Denied;
        }
    }
    PermissionState::Granted
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win32 {
    use super::*;
    use std::ptr;

    type Hkey = isize;
    const HKEY_CURRENT_USER: Hkey = -0x7fff_ffffisize; // 0x80000001
    const HKEY_LOCAL_MACHINE: Hkey = -0x7fff_fffeisize; // 0x80000002
    const KEY_QUERY_VALUE: u32 = 0x0001;
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const REG_DWORD: u32 = 4;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(
            hkey: Hkey,
            sub_key: *const u16,
            options: u32,
            sam_desired: u32,
            result: *mut Hkey,
        ) -> i32;
        fn RegQueryValueExW(
            hkey: Hkey,
            value_name: *const u16,
            reserved: *const u32,
            reg_type: *mut u32,
            data: *mut u8,
            cb_data: *mut u32,
        ) -> i32;
        fn RegCloseKey(hkey: Hkey) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn ProcessIdToSessionId(process_id: u32, session_id: *mut u32) -> i32;
    }

    fn encode_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn query_reg_dword(root: Hkey, subkey: &str, name: &str) -> Option<u32> {
        let sub_w = encode_wide(subkey);
        let name_w = encode_wide(name);
        let mut hkey: Hkey = 0;
        // SAFETY: `sub_w` is NUL-terminated UTF-16 and `hkey` is stack-owned.
        let rc = unsafe { RegOpenKeyExW(root, sub_w.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) };
        if rc != 0 || hkey == 0 {
            return None;
        }
        let mut reg_type = 0u32;
        let mut val = 0u32;
        let mut cb = std::mem::size_of::<u32>() as u32;
        // SAFETY: `hkey` is open with `KEY_QUERY_VALUE` and closed unconditionally below.
        let qrc = unsafe {
            RegQueryValueExW(
                hkey,
                name_w.as_ptr(),
                ptr::null(),
                &mut reg_type,
                (&mut val as *mut u32).cast::<u8>(),
                &mut cb,
            )
        };
        // SAFETY: closes key opened above.
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        (qrc == 0 && reg_type == REG_DWORD && cb == 4).then_some(val)
    }

    fn query_reg_string(root: Hkey, subkey: &str, name: &str) -> Option<String> {
        let sub_w = encode_wide(subkey);
        let name_w = encode_wide(name);
        let mut hkey: Hkey = 0;
        // SAFETY: `sub_w` is NUL-terminated UTF-16 and `hkey` is stack-owned.
        let rc = unsafe { RegOpenKeyExW(root, sub_w.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) };
        if rc != 0 || hkey == 0 {
            return None;
        }
        let mut reg_type = 0u32;
        let mut buf = [0u16; 128];
        let mut cb = (buf.len() * std::mem::size_of::<u16>()) as u32;
        // SAFETY: `buf` has `cb` writable bytes; `hkey` is closed immediately afterward.
        let qrc = unsafe {
            RegQueryValueExW(
                hkey,
                name_w.as_ptr(),
                ptr::null(),
                &mut reg_type,
                buf.as_mut_ptr().cast::<u8>(),
                &mut cb,
            )
        };
        // SAFETY: closes key opened above.
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        if qrc != 0 || !matches!(reg_type, REG_SZ | REG_EXPAND_SZ) || cb < 2 {
            return None;
        }
        let u16_len = (cb as usize) / 2;
        let slice = &buf[..u16_len.min(buf.len())];
        let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        Some(String::from_utf16_lossy(&slice[..end]))
    }

    pub fn query_live_mic_permission() -> PermissionState {
        let policy = query_reg_dword(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Policies\Microsoft\Windows\AppPrivacy",
            "LetAppsAccessMicrophone",
        );
        let machine = query_reg_string(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone",
            "Value",
        );
        let user = query_reg_string(
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone",
            "Value",
        );
        let non_packaged = query_reg_string(
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged",
            "Value",
        );
        evaluate_windows_mic_consent(
            policy,
            machine.as_deref(),
            user.as_deref(),
            non_packaged.as_deref(),
        )
    }

    pub fn is_interactive_user_session() -> bool {
        if std::env::var_os("SSH_CONNECTION").is_some()
            || std::env::var_os("CSPA_DISABLE_SETTINGS_LAUNCH").is_some()
        {
            return false;
        }
        let mut session_id = 0u32;
        // SAFETY: queries session ID for the current process; session 0 is non-interactive services.
        let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) };
        ok != 0 && session_id != 0
    }

    pub fn open_windows_mic_privacy_settings() {
        if !is_interactive_user_session() {
            return;
        }
        let op = encode_wide("open");
        let uri = encode_wide("ms-settings:privacy-microphone");
        // SAFETY: passes valid NUL-terminated UTF-16 strings to `ShellExecuteW` with `SW_SHOWNORMAL = 1`.
        unsafe {
            let _ = ShellExecuteW(0, op.as_ptr(), uri.as_ptr(), ptr::null(), ptr::null(), 1);
        }
    }
}

pub fn query_windows_microphone_permission_state() -> PermissionState {
    if crate::capabilities::current_windows_build() < MIN_WINDOWS_ENDPOINT_BUILD {
        return PermissionState::Restricted;
    }
    #[cfg(windows)]
    {
        win32::query_live_mic_permission()
    }
    #[cfg(not(windows))]
    {
        PermissionState::Granted
    }
}

pub fn ensure_windows_target_permission(target: &CaptureTarget) -> Result<(), CapabilityError> {
    if matches!(
        target.kind(),
        TargetKind::DefaultInput | TargetKind::InputDevice
    ) && query_windows_microphone_permission_state().is_denied_or_restricted()
    {
        return Err(CapabilityError::PermissionDenied);
    }
    Ok(())
}

fn build_windows_report(kinds: &[PermissionKind]) -> PermissionReport {
    let mic_state = query_windows_microphone_permission_state();
    let sys_state = if crate::capabilities::current_windows_build() >= MIN_WINDOWS_ENDPOINT_BUILD {
        PermissionState::Granted
    } else {
        PermissionState::Restricted
    };
    PermissionReport::new(
        PlatformOs::Windows,
        PermissionEntry {
            kind: PermissionKind::Microphone,
            state: mic_state,
            required: kinds.contains(&PermissionKind::Microphone),
            can_prompt: false,
            requires_relaunch: false,
        },
        PermissionEntry {
            kind: PermissionKind::SystemAudio,
            state: sys_state,
            required: kinds.contains(&PermissionKind::SystemAudio),
            can_prompt: false,
            requires_relaunch: false,
        },
        PermissionEntry {
            kind: PermissionKind::ScreenRecording,
            state: sys_state,
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
        let req = required_permission_kinds_for_os(targets, backends, PlatformOs::Windows)?;
        let kinds: Vec<PermissionKind> = req.into_iter().collect();
        Self::check_permission_kinds(&kinds, backends)
    }

    fn request_permissions(
        targets: &[CaptureTarget],
        backends: &Backends,
        timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        let req = required_permission_kinds_for_os(targets, backends, PlatformOs::Windows)?;
        let kinds: Vec<PermissionKind> = req.into_iter().collect();
        Self::request_permission_kinds(&kinds, backends, timeout, open_settings_if_denied)
    }

    fn check_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
    ) -> Result<PermissionReport, CapabilityError> {
        backends.validate()?;
        Ok(build_windows_report(kinds))
    }

    fn request_permission_kinds(
        kinds: &[PermissionKind],
        backends: &Backends,
        _timeout: Duration,
        open_settings_if_denied: bool,
    ) -> Result<PermissionReport, CapabilityError> {
        backends.validate()?;
        let report = build_windows_report(kinds);
        #[cfg(windows)]
        if open_settings_if_denied
            && report.microphone().required
            && report.microphone().state == PermissionState::Denied
        {
            win32::open_windows_mic_privacy_settings();
        }
        #[cfg(not(windows))]
        let _ = open_settings_if_denied;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema::DefaultDeviceMode;

    #[test]
    fn windows_mic_consent_evaluator_honors_gpo_machine_user_and_nonpackaged_hierarchy() {
        assert_eq!(
            evaluate_windows_mic_consent(None, Some("Allow"), Some("Allow"), Some("Allow")),
            PermissionState::Granted
        );
        assert_eq!(
            evaluate_windows_mic_consent(Some(0), None, None, None),
            PermissionState::Granted
        );

        assert_eq!(
            evaluate_windows_mic_consent(Some(2), Some("Allow"), Some("Allow"), Some("Allow")),
            PermissionState::Restricted
        );

        assert_eq!(
            evaluate_windows_mic_consent(Some(1), Some("Deny"), Some("Deny"), Some("Deny")),
            PermissionState::Granted
        );

        assert_eq!(
            evaluate_windows_mic_consent(None, Some("Deny"), Some("Allow"), Some("Allow")),
            PermissionState::Denied
        );
        assert_eq!(
            evaluate_windows_mic_consent(None, Some("Allow"), Some("deny"), Some("Allow")),
            PermissionState::Denied
        );
        assert_eq!(
            evaluate_windows_mic_consent(None, Some("Allow"), Some("Allow"), Some("Deny")),
            PermissionState::Denied
        );

        let out_rep = PlatformPermissions::check_permissions(
            &[CaptureTarget::default_output(DefaultDeviceMode::Follow)],
            &Backends::default(),
        )
        .unwrap();
        assert!(out_rep.system_audio().required);
        assert!(!out_rep.microphone().required);
        assert!(out_rep.is_granted());
    }
}
