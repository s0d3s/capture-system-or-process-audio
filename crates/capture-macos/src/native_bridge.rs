#![allow(dead_code)]

#[allow(unsafe_code)]
#[allow(clippy::type_complexity, clippy::not_unsafe_ptr_arg_deref)]
#[cfg(target_os = "macos")]
mod imp {
    use crate::core::traits::CapabilityError;
    use std::ffi::{CStr, CString, c_char, c_double, c_float, c_int, c_uint, c_void};

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CspaMacProcessObject {
        pub object_id: u32,
        pub pid: u32,
        pub running_output: u32,
        pub bundle_id: [c_char; 256],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CspaMacOutputDevice {
        pub object_id: u32,
        pub is_default: u32,
        pub nominal_rate_hz: u32,
        pub channels: u32,
        pub uid: [c_char; 256],
        pub name: [c_char; 256],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CspaMacInputDevice {
        pub object_id: u32,
        pub is_default: u32,
        pub nominal_rate_hz: u32,
        pub channels: u32,
        pub transport_type: u32,
        pub uid: [c_char; 256],
        pub name: [c_char; 256],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CspaMacSckApp {
        pub pid: u32,
        pub bundle_id: [c_char; 256],
        pub app_name: [c_char; 256],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CspaMacRunningApp {
        pub pid: u32,
        pub bundle_id: [c_char; 256],
        pub app_name: [c_char; 256],
        pub exe_path: [c_char; 1024],
    }

    pub type CspaAudioPacketCallback = unsafe extern "C" fn(
        user_data: *mut c_void,
        num_buffers: u32,
        buffer_ptrs: *const *const c_float,
        buffer_channels: *const u32,
        frames: u32,
        timestamp_ns: u64,
        timestamp_valid: u32,
    );

    unsafe extern "C" {
        fn cspa_mac_os_version(major_out: *mut u32, minor_out: *mut u32, patch_out: *mut u32);
        fn cspa_mac_sck_preflight() -> c_int;
        fn cspa_mac_sck_request_access() -> c_int;
        fn cspa_mac_microphone_auth_status() -> c_int;
        fn cspa_mac_request_microphone_access(timeout_sec: c_double) -> c_int;
        fn cspa_mac_is_interactive_gui_session() -> c_int;
        fn cspa_mac_bundle_missing_usage_description(plist_key: *const c_char) -> c_int;
        fn cspa_mac_system_audio_tap_preflight() -> c_int;
        fn cspa_mac_request_system_audio_tap() -> c_int;
        fn cspa_mac_open_privacy_settings(kind_code: c_int) -> c_int;
        fn cspa_mac_list_process_objects(
            out_buf: *mut CspaMacProcessObject,
            max_items: u32,
            out_count: *mut u32,
        ) -> c_int;
        fn cspa_mac_translate_pid_to_process_object(pid: u32, out_object_id: *mut u32) -> c_int;
        fn cspa_mac_list_output_devices(
            out_buf: *mut CspaMacOutputDevice,
            max_items: u32,
            out_count: *mut u32,
        ) -> c_int;
        fn cspa_mac_list_input_devices(
            out_buf: *mut CspaMacInputDevice,
            max_items: u32,
            out_count: *mut u32,
        ) -> c_int;
        fn cspa_mac_classify_device_uid(
            device_uid: *const c_char,
            out_object_id: *mut u32,
            out_in_channels: *mut u32,
            out_out_channels: *mut u32,
            out_rate_hz: *mut u32,
            out_transport_type: *mut u32,
        ) -> c_int;
        fn cspa_mac_list_sck_applications(
            out_buf: *mut CspaMacSckApp,
            max_items: u32,
            out_count: *mut u32,
        ) -> c_int;
        fn cspa_mac_list_running_applications(
            out_buf: *mut CspaMacRunningApp,
            max_items: u32,
            out_count: *mut u32,
        ) -> c_int;
        fn cspa_mac_open_coreaudio_tap(
            process_objects: *const u32,
            process_count: u32,
            is_process_tap: c_int,
            device_uid: *const c_char,
            mute_tap: c_int,
            callback: CspaAudioPacketCallback,
            user_data: *mut c_void,
            out_rate_hz: *mut u32,
            out_channels: *mut u32,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        fn cspa_mac_update_coreaudio_tap_processes(
            handle: *mut c_void,
            process_objects: *const u32,
            process_count: u32,
        ) -> c_int;
        fn cspa_mac_close_coreaudio_tap(handle: *mut c_void) -> c_int;
        fn cspa_mac_open_coreaudio_input(
            device_uid: *const c_char,
            auth_override: c_int,
            allow_prompt: c_int,
            callback: CspaAudioPacketCallback,
            user_data: *mut c_void,
            out_rate_hz: *mut u32,
            out_channels: *mut u32,
            out_resolved_uid: *mut c_char,
            resolved_uid_cap: u32,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        fn cspa_mac_close_coreaudio_input(handle: *mut c_void) -> c_int;
        #[cfg(feature = "native-tests")]
        fn cspa_mac_create_test_virtual_mic(
            target_pid: u32,
            mono: c_int,
            uid_cstr: *const c_char,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        #[cfg(feature = "native-tests")]
        fn cspa_mac_destroy_test_virtual_mic(handle: *mut c_void) -> c_int;
        fn cspa_mac_open_sck_app_stream(
            target_pid: u32,
            exclude_current_process: c_int,
            callback: CspaAudioPacketCallback,
            user_data: *mut c_void,
            out_rate_hz: *mut u32,
            out_channels: *mut u32,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        fn cspa_mac_close_sck_app_stream(handle: *mut c_void) -> c_int;
        fn cspa_mac_register_device_listener(
            callback: unsafe extern "C" fn(user_data: *mut c_void),
            user_data: *mut c_void,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        fn cspa_mac_unregister_device_listener(handle: *mut c_void) -> c_int;
    }

    pub fn os_version() -> (u32, u32, u32) {
        let mut major = 15u32;
        let mut minor = 0u32;
        let mut patch = 0u32;
        // SAFETY: passes valid writable pointers.
        unsafe {
            cspa_mac_os_version(&mut major, &mut minor, &mut patch);
        }
        (major, minor, patch)
    }

    pub fn sck_preflight_granted() -> bool {
        // SAFETY: calls `CGPreflightScreenCaptureAccess()` without prompting.
        unsafe { cspa_mac_sck_preflight() != 0 }
    }

    pub fn sck_request_access() -> bool {
        // SAFETY: calls `CGRequestScreenCaptureAccess()` when in an interactive GUI session.
        unsafe { cspa_mac_sck_request_access() != 0 }
    }

    pub fn microphone_auth_status() -> i32 {
        // SAFETY: calls `[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]` without prompting.
        unsafe { cspa_mac_microphone_auth_status() }
    }

    pub fn request_microphone_access(timeout_sec: f64) -> i32 {
        // SAFETY: requests `AVMediaTypeAudio` TCC access when `NotDetermined` in an interactive GUI session.
        unsafe { cspa_mac_request_microphone_access(timeout_sec) }
    }

    pub fn is_interactive_gui_session() -> bool {
        // SAFETY: checks `SSH_CONNECTION`/`SSH_TTY` and `CGSessionCopyCurrentDictionary`.
        unsafe { cspa_mac_is_interactive_gui_session() != 0 }
    }

    pub fn bundle_missing_usage_description(plist_key: &str) -> bool {
        let Ok(key_c) = CString::new(plist_key) else {
            return false;
        };
        // SAFETY: passes NUL-terminated `key_c`.
        unsafe { cspa_mac_bundle_missing_usage_description(key_c.as_ptr()) != 0 }
    }

    pub fn system_audio_tap_preflight() -> i32 {
        // SAFETY: side-effect-free preflight inspecting `.app` usage description and `'wat?'`/`'!hog'` status.
        unsafe { cspa_mac_system_audio_tap_preflight() }
    }

    pub fn request_system_audio_tap() -> i32 {
        // SAFETY: creates and immediately destroys a private probe `CATapDescription` in interactive GUI sessions.
        unsafe { cspa_mac_request_system_audio_tap() }
    }

    pub fn open_privacy_settings(kind_code: i32) -> bool {
        // SAFETY: opens `x-apple.systempreferences:` URL via `NSWorkspace` when interactive.
        unsafe { cspa_mac_open_privacy_settings(kind_code) != 0 }
    }

    pub fn classify_device_uid(device_uid: &str) -> Option<(u32, u32, u32, u32, u32)> {
        let uid_c = CString::new(device_uid).ok()?;
        let mut obj_id = 0u32;
        let mut in_ch = 0u32;
        let mut out_ch = 0u32;
        let mut rate_hz = 0u32;
        let mut transport = 0u32;
        // SAFETY: passes valid pointers and NUL-terminated `uid_c`.
        let rc = unsafe {
            cspa_mac_classify_device_uid(
                uid_c.as_ptr(),
                &mut obj_id,
                &mut in_ch,
                &mut out_ch,
                &mut rate_hz,
                &mut transport,
            )
        };
        if rc == 0 && obj_id != 0 {
            Some((obj_id, in_ch, out_ch, rate_hz, transport))
        } else {
            None
        }
    }

    pub fn list_coreaudio_process_objects()
    -> Result<Vec<(u32, u32, bool, Option<String>)>, CapabilityError> {
        let mut buf = vec![
            CspaMacProcessObject {
                object_id: 0,
                pid: 0,
                running_output: 0,
                bundle_id: [0; 256],
            };
            512
        ];
        let mut count = 0u32;
        // SAFETY: `buf` has capacity for 512 records.
        let rc = unsafe { cspa_mac_list_process_objects(buf.as_mut_ptr(), 512, &mut count) };
        if rc != 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let mut out = Vec::with_capacity(count as usize);
        for item in buf.iter().take(count as usize) {
            // SAFETY: `bundle_id` is null-terminated by the bridge.
            let bundle = unsafe {
                CStr::from_ptr(item.bundle_id.as_ptr())
                    .to_string_lossy()
                    .trim()
                    .to_owned()
            };
            let bundle_opt = if bundle.is_empty() {
                None
            } else {
                Some(bundle)
            };
            out.push((
                item.object_id,
                item.pid,
                item.running_output != 0,
                bundle_opt,
            ));
        }
        Ok(out)
    }

    pub fn translate_pid_to_process_object(pid: u32) -> Option<u32> {
        let mut obj = 0u32;
        // SAFETY: queries `kAudioHardwarePropertyTranslatePIDToProcessObject`.
        let rc = unsafe { cspa_mac_translate_pid_to_process_object(pid, &mut obj) };
        if rc == 0 && obj != 0 { Some(obj) } else { None }
    }

    pub fn list_coreaudio_output_devices()
    -> Result<Vec<(u32, String, String, u32, u32, bool)>, CapabilityError> {
        let mut buf = vec![
            CspaMacOutputDevice {
                object_id: 0,
                is_default: 0,
                nominal_rate_hz: 0,
                channels: 0,
                uid: [0; 256],
                name: [0; 256],
            };
            64
        ];
        let mut count = 0u32;
        // SAFETY: `buf` has capacity for 64 records.
        let rc = unsafe { cspa_mac_list_output_devices(buf.as_mut_ptr(), 64, &mut count) };
        if rc != 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let mut out = Vec::with_capacity(count as usize);
        for item in buf.iter().take(count as usize) {
            // SAFETY: `uid` and `name` are null-terminated by the bridge.
            let uid = unsafe {
                CStr::from_ptr(item.uid.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            };
            let name = unsafe {
                CStr::from_ptr(item.name.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            };
            out.push((
                item.object_id,
                uid,
                name,
                item.nominal_rate_hz,
                item.channels,
                item.is_default != 0,
            ));
        }
        Ok(out)
    }

    pub fn list_coreaudio_input_devices()
    -> Result<Vec<(u32, String, String, u32, u32, u32, bool)>, CapabilityError> {
        let mut buf = vec![
            CspaMacInputDevice {
                object_id: 0,
                is_default: 0,
                nominal_rate_hz: 0,
                channels: 0,
                transport_type: 0,
                uid: [0; 256],
                name: [0; 256],
            };
            64
        ];
        let mut count = 0u32;
        // SAFETY: `buf` has capacity for 64 records.
        let rc = unsafe { cspa_mac_list_input_devices(buf.as_mut_ptr(), 64, &mut count) };
        if rc != 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let mut out = Vec::with_capacity(count as usize);
        for item in buf.iter().take(count as usize) {
            // SAFETY: `uid` and `name` are null-terminated by the bridge.
            let uid = unsafe {
                CStr::from_ptr(item.uid.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            };
            let name = unsafe {
                CStr::from_ptr(item.name.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            };
            out.push((
                item.object_id,
                uid,
                name,
                item.nominal_rate_hz,
                item.channels,
                item.transport_type,
                item.is_default != 0,
            ));
        }
        Ok(out)
    }

    pub fn list_sck_shareable_applications() -> Result<Vec<(u32, String, String)>, CapabilityError>
    {
        if !sck_preflight_granted() {
            return Err(CapabilityError::PermissionDenied);
        }
        let mut buf = vec![
            CspaMacSckApp {
                pid: 0,
                bundle_id: [0; 256],
                app_name: [0; 256],
            };
            256
        ];
        let mut count = 0u32;
        // SAFETY: `buf` has capacity for 256 records.
        let rc = unsafe { cspa_mac_list_sck_applications(buf.as_mut_ptr(), 256, &mut count) };
        if rc != 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let mut out = Vec::with_capacity(count as usize);
        for item in buf.iter().take(count as usize) {
            if item.pid == 0 {
                continue;
            }
            // SAFETY: `bundle_id` and `app_name` are null-terminated by the bridge.
            let bundle = unsafe {
                CStr::from_ptr(item.bundle_id.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            };
            let name = unsafe {
                CStr::from_ptr(item.app_name.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            };
            out.push((item.pid, bundle, name));
        }
        Ok(out)
    }

    pub fn list_running_applications() -> Result<Vec<(u32, String, String, String)>, CapabilityError>
    {
        let mut buf = vec![
            CspaMacRunningApp {
                pid: 0,
                bundle_id: [0; 256],
                app_name: [0; 256],
                exe_path: [0; 1024],
            };
            256
        ];
        let mut count = 0u32;
        // SAFETY: `buf` has capacity for 256 records.
        let rc = unsafe { cspa_mac_list_running_applications(buf.as_mut_ptr(), 256, &mut count) };
        if rc != 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let mut out = Vec::with_capacity(count as usize);
        for item in buf.iter().take(count as usize) {
            if item.pid == 0 {
