#![allow(dead_code)]

use crate::capabilities::{MIN_PIPEWIRE_VERSION, MIN_PULSEAUDIO_VERSION, PulseServerKind};
use crate::core::{
    schema::{BackendId, SampleType},
    traits::CapabilityError,
};
use std::ffi::{CStr, c_char, c_int, c_void};

pub const DEFAULT_PIPEWIRE_SO: &str = "libpipewire-0.3.so.0";
pub const DEFAULT_PULSE_SO: &str = "libpulse.so.0";

pub const REQUIRED_PIPEWIRE_SYMBOLS: &[&[u8]] = &[
    b"pw_init\0",
    b"pw_deinit\0",
    b"pw_get_library_version\0",
    b"pw_main_loop_new\0",
    b"pw_main_loop_destroy\0",
    b"pw_main_loop_get_loop\0",
    b"pw_main_loop_run\0",
    b"pw_main_loop_quit\0",
    b"pw_thread_loop_new\0",
    b"pw_thread_loop_destroy\0",
    b"pw_thread_loop_get_loop\0",
    b"pw_thread_loop_start\0",
    b"pw_thread_loop_stop\0",
    b"pw_thread_loop_lock\0",
    b"pw_thread_loop_unlock\0",
    b"pw_thread_loop_signal\0",
    b"pw_thread_loop_wait\0",
    b"pw_context_new\0",
    b"pw_context_destroy\0",
    b"pw_context_connect\0",
    b"pw_core_disconnect\0",
    b"pw_properties_new\0",
    b"pw_properties_free\0",
    b"pw_properties_set\0",
    b"pw_properties_setf\0",
    b"pw_stream_new\0",
    b"pw_stream_destroy\0",
    b"pw_stream_add_listener\0",
    b"pw_stream_connect\0",
    b"pw_stream_disconnect\0",
    b"pw_stream_dequeue_buffer\0",
    b"pw_stream_queue_buffer\0",
    b"pw_stream_get_time_n\0",
    b"pw_stream_get_state\0",
    b"pw_proxy_add_listener\0",
    b"pw_proxy_add_object_listener\0",
    b"pw_proxy_destroy\0",
];

pub const REQUIRED_PULSE_SYMBOLS: &[&[u8]] = &[
    b"pa_get_library_version\0",
    b"pa_strerror\0",
    b"pa_threaded_mainloop_new\0",
    b"pa_threaded_mainloop_free\0",
    b"pa_threaded_mainloop_start\0",
    b"pa_threaded_mainloop_stop\0",
    b"pa_threaded_mainloop_lock\0",
    b"pa_threaded_mainloop_unlock\0",
    b"pa_threaded_mainloop_wait\0",
    b"pa_threaded_mainloop_signal\0",
    b"pa_threaded_mainloop_get_api\0",
    b"pa_context_new\0",
    b"pa_context_unref\0",
    b"pa_context_connect\0",
    b"pa_context_disconnect\0",
    b"pa_context_get_state\0",
    b"pa_context_set_state_callback\0",
    b"pa_context_set_subscribe_callback\0",
    b"pa_context_subscribe\0",
    b"pa_context_get_server_info\0",
    b"pa_context_get_sink_info_list\0",
    b"pa_context_get_source_info_list\0",
    b"pa_context_get_sink_input_info_list\0",
    b"pa_context_get_client_info_list\0",
    b"pa_context_move_sink_input_by_index\0",
    b"pa_context_move_sink_input_by_name\0",
    b"pa_context_load_module\0",
    b"pa_context_unload_module\0",
    b"pa_operation_unref\0",
    b"pa_operation_get_state\0",
    b"pa_proplist_gets\0",
    b"pa_stream_new\0",
    b"pa_stream_unref\0",
    b"pa_stream_get_state\0",
    b"pa_stream_set_state_callback\0",
    b"pa_stream_set_read_callback\0",
    b"pa_stream_set_monitor_stream\0",
    b"pa_stream_connect_record\0",
    b"pa_stream_disconnect\0",
    b"pa_stream_peek\0",
    b"pa_stream_drop\0",
    b"pa_stream_get_time\0",
    b"pa_stream_get_sample_spec\0",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientLoadError {
    LibraryAbsent(String),
    SymbolMissing(String),
    VersionTooOld {
        found: String,
        minimum: &'static str,
    },
}

impl ClientLoadError {
    pub(crate) fn into_capability_error(
        self,
        backend: BackendId,
        required: (u32, u32, u32),
    ) -> CapabilityError {
        match self {
            Self::LibraryAbsent(path) => CapabilityError::ClientLibraryUnavailable {
                backend,
                detail: format!("library absent: {path}").into(),
            },
            Self::SymbolMissing(symbol) => CapabilityError::ClientLibraryUnavailable {
                backend,
                detail: format!("missing symbol: {symbol}").into(),
            },
            Self::VersionTooOld { found, .. } => CapabilityError::BackendVersionTooOld {
                backend,
                required,
                observed: parse_semver_triplet(&found).unwrap_or((0, 0, 0)),
            },
        }
    }
}

impl std::fmt::Display for ClientLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryAbsent(path) => {
                write!(formatter, "runtime client library unavailable: {path}")
            }
            Self::SymbolMissing(symbol) => {
                write!(
                    formatter,
                    "runtime client library missing required symbol: {symbol}"
                )
            }
            Self::VersionTooOld { found, minimum } => write!(
                formatter,
                "runtime client library version {found} is older than required floor {minimum}"
            ),
        }
    }
}

impl std::error::Error for ClientLoadError {}

#[derive(Clone, Debug, PartialEq)]
pub struct LinuxClientPaths {
    pub pipewire_so: String,
    pub pulse_so: String,
    pub pipewire_min_version: (u32, u32, u32),
    pub pulse_min_version: (u32, u32, u32),
}

impl Default for LinuxClientPaths {
    fn default() -> Self {
        Self {
            pipewire_so: std::env::var("CSPA_PIPEWIRE_SO")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_PIPEWIRE_SO.to_owned()),
            pulse_so: std::env::var("CSPA_PULSE_SO")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_PULSE_SO.to_owned()),
            pipewire_min_version: MIN_PIPEWIRE_VERSION,
            pulse_min_version: MIN_PULSEAUDIO_VERSION,
        }
    }
}

pub fn parse_semver_triplet(version: &str) -> Option<(u32, u32, u32)> {
    let token = version
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let clean = token.split('-').next().unwrap_or(token);
    let mut parts = clean.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u32 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

pub fn validate_version_floor(
    version: &str,
    floor: (u32, u32, u32),
    floor_label: &'static str,
) -> Result<(u32, u32, u32), ClientLoadError> {
    let parsed = parse_semver_triplet(version).ok_or_else(|| ClientLoadError::VersionTooOld {
        found: version.to_owned(),
        minimum: floor_label,
    })?;
    if parsed < floor {
        return Err(ClientLoadError::VersionTooOld {
            found: version.to_owned(),
            minimum: floor_label,
        });
    }
    Ok(parsed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeWireServerIdentity {
    pub library_version: String,
    pub server_version: String,
    pub server_name: String,
    pub default_sink_name: Option<String>,
    pub default_source_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PulseServerIdentity {
    pub library_version: String,
    pub server_name: String,
    pub server_version: String,
    pub server_kind: PulseServerKind,
    pub default_sink_name: Option<String>,
    pub default_source_name: Option<String>,
}

// --- Runtime-loaded PipeWire and PulseAudio FFI on Linux ---

#[repr(C)]
pub struct SpaDictItem {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct SpaDict {
    pub flags: u32,
    pub n_items: u32,
    pub items: *const SpaDictItem,
}

impl SpaDict {
    /// Lookup a key in a `spa_dict`.
    ///
    /// # Safety
    /// `dict` must be either null or point to a valid `spa_dict` for the duration of this call.
    #[allow(unsafe_code)]
    pub unsafe fn lookup(dict: *const Self, key: &str) -> Option<String> {
        if dict.is_null() {
            return None;
        }
        // SAFETY: caller guarantees `dict` is valid.
        let d = unsafe { &*dict };
        if d.items.is_null() || d.n_items == 0 {
            return None;
        }
        // SAFETY: `d.items` has `d.n_items` contiguous elements.
        let items = unsafe { std::slice::from_raw_parts(d.items, d.n_items as usize) };
        for item in items {
            if item.key.is_null() || item.value.is_null() {
                continue;
            }
            // SAFETY: PipeWire dict keys and values are NUL-terminated C strings.
            let k = unsafe { CStr::from_ptr(item.key) }.to_str().ok()?;
            if k == key {
                // SAFETY: `item.value` is a valid C string.
                return Some(
                    unsafe { CStr::from_ptr(item.value) }
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        None
    }
}

#[repr(C)]
pub struct PwProperties {
    pub dict: SpaDict,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SpaCallbacks {
    pub funcs: *const c_void,
    pub data: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SpaInterface {
    pub type_: *const c_char,
    pub version: u32,
    pub cb: SpaCallbacks,
}

#[repr(C)]
#[derive(Default)]
pub struct SpaHook {
    pub _opaque: [u64; 6],
}

#[repr(C)]
pub struct PwCoreEvents {
    pub version: u32,
    pub info: Option<unsafe extern "C" fn(*mut c_void, *const PwCoreInfo)>,
    pub done: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    pub ping: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    pub error: Option<unsafe extern "C" fn(*mut c_void, u32, c_int, c_int, *const c_char)>,
    pub remove_id: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    pub bound_id: Option<unsafe extern "C" fn(*mut c_void, u32, u32)>,
    pub add_mem: Option<unsafe extern "C" fn(*mut c_void, u32, u32, c_int, u32)>,
    pub remove_mem: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    pub bound_props: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const SpaDict)>,
}

#[repr(C)]
pub struct PwCoreInfo {
    pub id: u32,
    pub cookie: u32,
    pub user_name: *const c_char,
    pub host_name: *const c_char,
    pub version: *const c_char,
    pub name: *const c_char,
    pub change_mask: u64,
    pub props: *const SpaDict,
}

#[repr(C)]
pub struct PwCoreMethods {
    pub version: u32,
    pub add_listener: Option<
        unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const PwCoreEvents, *mut c_void) -> c_int,
    >,
    pub hello: Option<unsafe extern "C" fn(*mut c_void, u32) -> c_int>,
    pub sync: Option<unsafe extern "C" fn(*mut c_void, u32, c_int) -> c_int>,
    pub pong: Option<unsafe extern "C" fn(*mut c_void, u32, c_int) -> c_int>,
    pub error: Option<unsafe extern "C" fn(*mut c_void, u32, c_int, c_int, *const c_char) -> c_int>,
    pub get_registry: Option<unsafe extern "C" fn(*mut c_void, u32, usize) -> *mut c_void>,
    pub create_object: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            u32,
            *const SpaDict,
            usize,
        ) -> *mut c_void,
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
}

#[repr(C)]
pub struct PwRegistryEvents {
    pub version: u32,
    pub global:
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_char, u32, *const SpaDict)>,
    pub global_remove: Option<unsafe extern "C" fn(*mut c_void, u32)>,
}

#[repr(C)]
pub struct PwRegistryMethods {
    pub version: u32,
    pub add_listener: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut SpaHook,
            *const PwRegistryEvents,
            *mut c_void,
        ) -> c_int,
    >,
    pub bind:
        Option<unsafe extern "C" fn(*mut c_void, u32, *const c_char, u32, usize) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void, u32) -> c_int>,
}

#[repr(C)]
pub struct PwNodeInfo {
    pub id: u32,
    pub max_input_ports: u32,
    pub max_output_ports: u32,
    pub change_mask: u64,
    pub n_input_ports: u32,
    pub n_output_ports: u32,
    pub state: c_int,
    pub error: *const c_char,
    pub props: *const SpaDict,
    pub params: *const c_void,
    pub n_params: u32,
}

#[repr(C)]
pub struct PwNodeEvents {
    pub version: u32,
    pub info: Option<unsafe extern "C" fn(*mut c_void, *const PwNodeInfo)>,
    pub param: Option<unsafe extern "C" fn(*mut c_void, c_int, u32, u32, u32, *const c_void)>,
}

#[repr(C)]
pub struct PwClientInfo {
    pub id: u32,
    pub change_mask: u64,
    pub props: *const SpaDict,
}

#[repr(C)]
pub struct PwClientEvents {
    pub version: u32,
    pub info: Option<unsafe extern "C" fn(*mut c_void, *const PwClientInfo)>,
    pub permissions: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_void)>,
}

#[repr(C)]
pub struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
pub struct SpaData {
    pub type_: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut SpaChunk,
}

#[repr(C)]
pub struct SpaBuffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut SpaData,
}

#[repr(C)]
pub struct PwBuffer {
    pub buffer: *mut SpaBuffer,
    pub user_data: *mut c_void,
    pub size: u64,
    pub requested: u64,
    pub time: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SpaFraction {
    pub num: u32,
    pub denom: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PwTime {
    pub now: i64,
    pub rate: SpaFraction,
    pub ticks: u64,
    pub delay: i64,
    pub queued: u64,
    pub buffered: u64,
    pub queued_buffers: u32,
    pub avail_buffers: u32,
    pub size: u64,
}

#[repr(C)]
pub struct PwStreamEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    pub param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub process: Option<unsafe extern "C" fn(*mut c_void)>,
    pub drained: Option<unsafe extern "C" fn(*mut c_void)>,
    pub command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    pub trigger_done: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct AlignedSpaPod(pub [u8; 168]);

pub const SPA_AUDIO_FORMAT_S16_LE: u32 = 0x107;
pub const SPA_AUDIO_FORMAT_S32_LE: u32 = 0x109;
pub const SPA_AUDIO_FORMAT_S24_LE: u32 = 0x10b;
pub const SPA_AUDIO_FORMAT_F32_LE: u32 = 0x11b;
pub const SPA_AUDIO_CHANNEL_MONO: u32 = 2;
pub const SPA_AUDIO_CHANNEL_FL: u32 = 3;
pub const SPA_AUDIO_CHANNEL_FR: u32 = 4;

/// Build the 8-byte-aligned 168-byte `SPA_PARAM_EnumFormat` `spa_pod` for raw little-endian PCM
/// (`F32`/`I16`/`I24`/`I32`, `1ch Mono` or `2ch Stereo`) at the requested `rate_hz`.
pub fn build_spa_audio_raw_pod(
    sample: SampleType,
    rate_hz: u32,
    channels: u16,
) -> Result<AlignedSpaPod, CapabilityError> {
    if rate_hz == 0 || !matches!(channels, 1 | 2) {
        return Err(CapabilityError::InvalidTarget);
    }
    let spa_fmt: u32 = match sample {
        SampleType::F32 => SPA_AUDIO_FORMAT_F32_LE,
        SampleType::I16 => SPA_AUDIO_FORMAT_S16_LE,
        SampleType::I24 => SPA_AUDIO_FORMAT_S24_LE,
        SampleType::I32 => SPA_AUDIO_FORMAT_S32_LE,
    };
    let mut bytes: [u8; 168] = [
        0xa0, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x03, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x00, 0x1b, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x80,
        0xbb, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x0d, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04,
        0x00, 0x00, 0x00,
    ];
    bytes[80..84].copy_from_slice(&spa_fmt.to_le_bytes());
    bytes[104..108].copy_from_slice(&rate_hz.to_le_bytes());
    bytes[128..132].copy_from_slice(&u32::from(channels).to_le_bytes());
    if channels == 1 {
        bytes[144..148].copy_from_slice(&12u32.to_le_bytes());
        bytes[160..164].copy_from_slice(&SPA_AUDIO_CHANNEL_MONO.to_le_bytes());
        bytes[164..168].fill(0);
    } else {
        bytes[144..148].copy_from_slice(&16u32.to_le_bytes());
        bytes[160..164].copy_from_slice(&SPA_AUDIO_CHANNEL_FL.to_le_bytes());
        bytes[164..168].copy_from_slice(&SPA_AUDIO_CHANNEL_FR.to_le_bytes());
    }
    Ok(AlignedSpaPod(bytes))
}

/// Build the 8-byte-aligned 168-byte `SPA_PARAM_EnumFormat` `spa_pod` for `SPA_AUDIO_FORMAT_F32_LE`
/// stereo (`FL, FR`) at the requested `rate_hz` (verified byte-for-byte against `spa_format_audio_raw_build`).
pub fn build_spa_f32_le_stereo_pod(rate_hz: u32) -> AlignedSpaPod {
    build_spa_audio_raw_pod(SampleType::F32, rate_hz.max(1), 2).expect("valid stereo F32 SPA pod")
}

// --- PulseAudio C ABI structs ---

pub const PA_CHANNELS_MAX: usize = 32;
pub const PA_INVALID_INDEX: u32 = u32::MAX;
pub const PA_CONTEXT_NOAUTOSPAWN: c_int = 0x0001;
pub const PA_CONTEXT_READY: c_int = 4;
pub const PA_CONTEXT_FAILED: c_int = 5;
pub const PA_CONTEXT_TERMINATED: c_int = 6;
pub const PA_STREAM_READY: c_int = 2;
pub const PA_STREAM_FAILED: c_int = 3;
pub const PA_STREAM_TERMINATED: c_int = 4;
pub const PA_SAMPLE_S16LE: c_int = 3;
pub const PA_SAMPLE_FLOAT32LE: c_int = 5;
pub const PA_SAMPLE_S32LE: c_int = 7;
pub const PA_SAMPLE_S24LE: c_int = 9;
pub const PA_STREAM_ADJUST_LATENCY: c_int = 0x2000;
pub const PA_SUBSCRIPTION_MASK_SINK: c_int = 0x0001;
pub const PA_SUBSCRIPTION_MASK_SOURCE: c_int = 0x0002;
pub const PA_SUBSCRIPTION_MASK_SINK_INPUT: c_int = 0x0004;
pub const PA_SUBSCRIPTION_MASK_CLIENT: c_int = 0x0020;
pub const PA_SUBSCRIPTION_MASK_SERVER: c_int = 0x0080;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PaSampleSpec {
    pub format: c_int,
    pub rate: u32,
    pub channels: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PaChannelMap {
    pub channels: u8,
    pub map: [c_int; PA_CHANNELS_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PaCVolume {
    pub channels: u8,
    pub values: [u32; PA_CHANNELS_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PaBufferAttr {
    pub maxlength: u32,
    pub tlength: u32,
    pub prebuf: u32,
    pub minreq: u32,
    pub fragsize: u32,
}

#[repr(C)]
pub struct PaServerInfo {
    pub user_name: *const c_char,
    pub host_name: *const c_char,
    pub server_version: *const c_char,
    pub server_name: *const c_char,
    pub sample_spec: PaSampleSpec,
    pub default_sink_name: *const c_char,
    pub default_source_name: *const c_char,
    pub cookie: u32,
    pub channel_map: PaChannelMap,
}

#[repr(C)]
pub struct PaSinkInfo {
    pub name: *const c_char,
    pub index: u32,
    pub description: *const c_char,
    pub sample_spec: PaSampleSpec,
    pub channel_map: PaChannelMap,
    pub owner_module: u32,
    pub volume: PaCVolume,
    pub mute: c_int,
    pub monitor_source: u32,
    pub monitor_source_name: *const c_char,
    pub latency: u64,
    pub driver: *const c_char,
    pub flags: c_int,
    pub proplist: *mut c_void,
    pub configured_latency: u64,
    pub base_volume: u32,
    pub state: c_int,
    pub n_volume_steps: c_int,
    pub card: u32,
    pub n_ports: u32,
    pub ports: *const *const c_void,
    pub active_port: *const c_void,
}

#[repr(C)]
pub struct PaSourceInfo {
    pub name: *const c_char,
    pub index: u32,
    pub description: *const c_char,
    pub sample_spec: PaSampleSpec,
    pub channel_map: PaChannelMap,
    pub owner_module: u32,
    pub volume: PaCVolume,
    pub mute: c_int,
    pub monitor_of_sink: u32,
    pub monitor_of_sink_name: *const c_char,
    pub latency: u64,
    pub driver: *const c_char,
    pub flags: c_int,
    pub proplist: *mut c_void,
    pub configured_latency: u64,
    pub base_volume: u32,
    pub state: c_int,
    pub n_volume_steps: u32,
    pub card: u32,
    pub n_ports: u32,
    pub ports: *const *const c_void,
    pub active_port: *const c_void,
}

#[repr(C)]
pub struct PaSinkInputInfo {
    pub index: u32,
    pub name: *const c_char,
    pub owner_module: u32,
    pub client: u32,
    pub sink: u32,
    pub sample_spec: PaSampleSpec,
    pub channel_map: PaChannelMap,
    pub volume: PaCVolume,
    pub buffer_usec: u64,
    pub sink_usec: u64,
    pub resample_method: *const c_char,
    pub driver: *const c_char,
    pub mute: c_int,
    pub proplist: *mut c_void,
    pub corked: c_int,
    pub has_volume: c_int,
    pub volume_writable: c_int,
}

#[repr(C)]
pub struct PaClientInfo {
    pub index: u32,
    pub name: *const c_char,
    pub owner_module: u32,
    pub driver: *const c_char,
    pub proplist: *mut c_void,
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    use libloading::Library;
    use std::ffi::{CString, c_uint};

    pub struct PipeWireLibrary {
        pub version: String,
        pub pw_init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
        pub pw_get_library_version: unsafe extern "C" fn() -> *const c_char,
        pub pw_main_loop_new: unsafe extern "C" fn(*const SpaDict) -> *mut c_void,
        pub pw_main_loop_destroy: unsafe extern "C" fn(*mut c_void),
        pub pw_main_loop_get_loop: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
        pub pw_main_loop_run: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pw_main_loop_quit: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pw_thread_loop_new: unsafe extern "C" fn(*const c_char, *const SpaDict) -> *mut c_void,
        pub pw_thread_loop_destroy: unsafe extern "C" fn(*mut c_void),
        pub pw_thread_loop_get_loop: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
        pub pw_thread_loop_start: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pw_thread_loop_stop: unsafe extern "C" fn(*mut c_void),
        pub pw_thread_loop_lock: unsafe extern "C" fn(*mut c_void),
        pub pw_thread_loop_unlock: unsafe extern "C" fn(*mut c_void),
        pub pw_thread_loop_signal: unsafe extern "C" fn(*mut c_void, bool),
        pub pw_thread_loop_wait: unsafe extern "C" fn(*mut c_void),
        pub pw_context_new:
            unsafe extern "C" fn(*mut c_void, *mut PwProperties, usize) -> *mut c_void,
        pub pw_context_destroy: unsafe extern "C" fn(*mut c_void),
        pub pw_context_connect:
            unsafe extern "C" fn(*mut c_void, *mut PwProperties, usize) -> *mut c_void,
        pub pw_core_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pw_properties_new_dict: unsafe extern "C" fn(*const SpaDict) -> *mut PwProperties,
        pub pw_properties_free: unsafe extern "C" fn(*mut PwProperties),
        pub pw_properties_set:
            unsafe extern "C" fn(*mut PwProperties, *const c_char, *const c_char) -> c_int,
        pub pw_stream_new:
            unsafe extern "C" fn(*mut c_void, *const c_char, *mut PwProperties) -> *mut c_void,
        pub pw_stream_destroy: unsafe extern "C" fn(*mut c_void),
        pub pw_stream_add_listener:
            unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const PwStreamEvents, *mut c_void),
        pub pw_stream_connect: unsafe extern "C" fn(
            *mut c_void,
            c_uint,
            u32,
            c_uint,
            *mut *const c_void,
            u32,
        ) -> c_int,
        pub pw_stream_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pw_stream_dequeue_buffer: unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer,
        pub pw_stream_queue_buffer: unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int,
        pub pw_stream_get_time_n: unsafe extern "C" fn(*mut c_void, *mut PwTime, usize) -> c_int,
        pub pw_stream_get_state: unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int,
        pub pw_proxy_add_listener:
            unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const c_void, *mut c_void),
        pub pw_proxy_add_object_listener:
            unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const c_void, *mut c_void),
        pub pw_proxy_destroy: unsafe extern "C" fn(*mut c_void),
        _lib: Library,
    }

    impl PipeWireLibrary {
        pub fn load(paths: &LinuxClientPaths) -> Result<Self, ClientLoadError> {
            // SAFETY: dynamically loads the requested system PipeWire shared library and resolves
            // its C ABI symbols while keeping `Library` alive inside `Self`.
            unsafe {
                let lib = Library::new(&paths.pipewire_so)
                    .map_err(|_| ClientLoadError::LibraryAbsent(paths.pipewire_so.clone()))?;
                for &sym in REQUIRED_PIPEWIRE_SYMBOLS {
                    let sym_name = CStr::from_bytes_with_nul(sym)
                        .ok()
                        .and_then(|s| s.to_str().ok())
                        .unwrap_or("unknown");
                    if lib.get::<*const c_void>(sym).is_err() {
                        return Err(ClientLoadError::SymbolMissing(sym_name.to_owned()));
                    }
                }
                let pw_get_library_version: unsafe extern "C" fn() -> *const c_char =
                    *lib.get(b"pw_get_library_version\0").unwrap();
                let ver_ptr = pw_get_library_version();
                let version = if ver_ptr.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(ver_ptr).to_string_lossy().into_owned()
                };
                validate_version_floor(&version, paths.pipewire_min_version, "0.3.65")?;

                let pw_init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) =
                    *lib.get(b"pw_init\0").unwrap();
                pw_init(std::ptr::null_mut(), std::ptr::null_mut());

                Ok(Self {
                    version,
                    pw_init,
                    pw_get_library_version,
                    pw_main_loop_new: *lib.get(b"pw_main_loop_new\0").unwrap(),
                    pw_main_loop_destroy: *lib.get(b"pw_main_loop_destroy\0").unwrap(),
                    pw_main_loop_get_loop: *lib.get(b"pw_main_loop_get_loop\0").unwrap(),
                    pw_main_loop_run: *lib.get(b"pw_main_loop_run\0").unwrap(),
                    pw_main_loop_quit: *lib.get(b"pw_main_loop_quit\0").unwrap(),
                    pw_thread_loop_new: *lib.get(b"pw_thread_loop_new\0").unwrap(),
                    pw_thread_loop_destroy: *lib.get(b"pw_thread_loop_destroy\0").unwrap(),
                    pw_thread_loop_get_loop: *lib.get(b"pw_thread_loop_get_loop\0").unwrap(),
                    pw_thread_loop_start: *lib.get(b"pw_thread_loop_start\0").unwrap(),
                    pw_thread_loop_stop: *lib.get(b"pw_thread_loop_stop\0").unwrap(),
                    pw_thread_loop_lock: *lib.get(b"pw_thread_loop_lock\0").unwrap(),
                    pw_thread_loop_unlock: *lib.get(b"pw_thread_loop_unlock\0").unwrap(),
                    pw_thread_loop_signal: *lib.get(b"pw_thread_loop_signal\0").unwrap(),
                    pw_thread_loop_wait: *lib.get(b"pw_thread_loop_wait\0").unwrap(),
                    pw_context_new: *lib.get(b"pw_context_new\0").unwrap(),
                    pw_context_destroy: *lib.get(b"pw_context_destroy\0").unwrap(),
                    pw_context_connect: *lib.get(b"pw_context_connect\0").unwrap(),
                    pw_core_disconnect: *lib.get(b"pw_core_disconnect\0").unwrap(),
                    pw_properties_new_dict: *lib.get(b"pw_properties_new_dict\0").unwrap(),
                    pw_properties_free: *lib.get(b"pw_properties_free\0").unwrap(),
                    pw_properties_set: *lib.get(b"pw_properties_set\0").unwrap(),
                    pw_stream_new: *lib.get(b"pw_stream_new\0").unwrap(),
                    pw_stream_destroy: *lib.get(b"pw_stream_destroy\0").unwrap(),
                    pw_stream_add_listener: *lib.get(b"pw_stream_add_listener\0").unwrap(),
                    pw_stream_connect: *lib.get(b"pw_stream_connect\0").unwrap(),
                    pw_stream_disconnect: *lib.get(b"pw_stream_disconnect\0").unwrap(),
                    pw_stream_dequeue_buffer: *lib.get(b"pw_stream_dequeue_buffer\0").unwrap(),
                    pw_stream_queue_buffer: *lib.get(b"pw_stream_queue_buffer\0").unwrap(),
                    pw_stream_get_time_n: *lib.get(b"pw_stream_get_time_n\0").unwrap(),
                    pw_stream_get_state: *lib.get(b"pw_stream_get_state\0").unwrap(),
                    pw_proxy_add_listener: *lib.get(b"pw_proxy_add_listener\0").unwrap(),
                    pw_proxy_add_object_listener: *lib
                        .get(b"pw_proxy_add_object_listener\0")
                        .unwrap(),
                    pw_proxy_destroy: *lib.get(b"pw_proxy_destroy\0").unwrap(),
                    _lib: lib,
                })
            }
        }

        /// Create a new `pw_properties` from key-value pairs.
        ///
        /// # Safety
        /// Returned pointer is owned by caller and must be freed via `pw_properties_free` or
        /// transferred to a PipeWire constructor that takes ownership.
        pub unsafe fn new_properties(&self, pairs: &[(&str, &str)]) -> *mut PwProperties {
            let empty = SpaDict {
                flags: 0,
                n_items: 0,
                items: std::ptr::null(),
            };
            // SAFETY: `pw_properties_new_dict` creates an owned properties dictionary.
            let props = unsafe { (self.pw_properties_new_dict)(&empty) };
            if props.is_null() {
                return std::ptr::null_mut();
            }
            for &(k, v) in pairs {
                if let (Ok(ck), Ok(cv)) = (CString::new(k), CString::new(v)) {
                    // SAFETY: `props`, `ck`, and `cv` are valid C pointers.
                    unsafe {
                        (self.pw_properties_set)(props, ck.as_ptr(), cv.as_ptr());
                    }
                }
            }
            props
        }

        /// Invoke `pw_core_add_listener` via the `spa_interface` vtable on `core`.
        ///
        /// # Safety
        /// `core`, `listener`, `events`, and `data` must be valid pointers for the listener's lifetime.
        pub unsafe fn core_add_listener(
            &self,
            core: *mut c_void,
            listener: *mut SpaHook,
            events: *const PwCoreEvents,
            data: *mut c_void,
        ) -> c_int {
            // SAFETY: `core` starts with `struct spa_interface` whose `cb.funcs` points to `pw_core_methods`.
            unsafe {
                let iface = &*(core as *const SpaInterface);
                let methods = &*(iface.cb.funcs as *const PwCoreMethods);
                if let Some(func) = methods.add_listener {
                    func(iface.cb.data, listener, events, data)
                } else {
                    -95
                }
            }
        }

