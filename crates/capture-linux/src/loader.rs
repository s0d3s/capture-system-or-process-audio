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

        /// Invoke `pw_core_sync` via the `spa_interface` vtable on `core`.
        ///
        /// # Safety
        /// `core` must be a valid connected `pw_core *`.
        pub unsafe fn core_sync(&self, core: *mut c_void, id: u32, seq: c_int) -> c_int {
            // SAFETY: `core` starts with `struct spa_interface` whose `cb.funcs` points to `pw_core_methods`.
            unsafe {
                let iface = &*(core as *const SpaInterface);
                let methods = &*(iface.cb.funcs as *const PwCoreMethods);
                if let Some(func) = methods.sync {
                    func(iface.cb.data, id, seq)
                } else {
                    -95
                }
            }
        }

        /// Invoke `pw_core_get_registry` via the `spa_interface` vtable on `core`.
        ///
        /// # Safety
        /// `core` must be a valid connected `pw_core *`.
        pub unsafe fn core_get_registry(
            &self,
            core: *mut c_void,
            version: u32,
            user_data_size: usize,
        ) -> *mut c_void {
            // SAFETY: `core` starts with `struct spa_interface` whose `cb.funcs` points to `pw_core_methods`.
            unsafe {
                let iface = &*(core as *const SpaInterface);
                let methods = &*(iface.cb.funcs as *const PwCoreMethods);
                if let Some(func) = methods.get_registry {
                    func(iface.cb.data, version, user_data_size)
                } else {
                    std::ptr::null_mut()
                }
            }
        }

        /// Invoke `pw_core_create_object` via the `spa_interface` vtable on `core`.
        ///
        /// # Safety
        /// `core`, `factory_name`, `type_name`, and `props` must be valid pointers.
        pub unsafe fn core_create_object(
            &self,
            core: *mut c_void,
            factory_name: *const c_char,
            type_name: *const c_char,
            version: u32,
            props: *const SpaDict,
            user_data_size: usize,
        ) -> *mut c_void {
            // SAFETY: `core` starts with `struct spa_interface` whose `cb.funcs` points to `pw_core_methods`.
            unsafe {
                let iface = &*(core as *const SpaInterface);
                let methods = &*(iface.cb.funcs as *const PwCoreMethods);
                if let Some(func) = methods.create_object {
                    func(
                        iface.cb.data,
                        factory_name,
                        type_name,
                        version,
                        props,
                        user_data_size,
                    )
                } else {
                    std::ptr::null_mut()
                }
            }
        }

        /// Invoke `pw_registry_add_listener` via the `spa_interface` vtable on `registry`.
        ///
        /// # Safety
        /// `registry`, `listener`, `events`, and `data` must be valid pointers.
        pub unsafe fn registry_add_listener(
            &self,
            registry: *mut c_void,
            listener: *mut SpaHook,
            events: *const PwRegistryEvents,
            data: *mut c_void,
        ) -> c_int {
            // SAFETY: `registry` starts with `struct spa_interface` whose `cb.funcs` points to `pw_registry_methods`.
            unsafe {
                let iface = &*(registry as *const SpaInterface);
                let methods = &*(iface.cb.funcs as *const PwRegistryMethods);
                if let Some(func) = methods.add_listener {
                    func(iface.cb.data, listener, events, data)
                } else {
                    -95
                }
            }
        }

        /// Invoke `pw_registry_bind` via the `spa_interface` vtable on `registry`.
        ///
        /// # Safety
        /// `registry` and `type_name` must be valid pointers.
        pub unsafe fn registry_bind(
            &self,
            registry: *mut c_void,
            id: u32,
            type_name: *const c_char,
            version: u32,
            user_data_size: usize,
        ) -> *mut c_void {
            // SAFETY: `registry` starts with `struct spa_interface` whose `cb.funcs` points to `pw_registry_methods`.
            unsafe {
                let iface = &*(registry as *const SpaInterface);
                let methods = &*(iface.cb.funcs as *const PwRegistryMethods);
                if let Some(func) = methods.bind {
                    func(iface.cb.data, id, type_name, version, user_data_size)
                } else {
                    std::ptr::null_mut()
                }
            }
        }
    }

    pub struct PulseLibrary {
        pub version: String,
        pub pa_get_library_version: unsafe extern "C" fn() -> *const c_char,
        pub pa_strerror: unsafe extern "C" fn(c_int) -> *const c_char,
        pub pa_threaded_mainloop_new: unsafe extern "C" fn() -> *mut c_void,
        pub pa_threaded_mainloop_free: unsafe extern "C" fn(*mut c_void),
        pub pa_threaded_mainloop_start: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pa_threaded_mainloop_stop: unsafe extern "C" fn(*mut c_void),
        pub pa_threaded_mainloop_lock: unsafe extern "C" fn(*mut c_void),
        pub pa_threaded_mainloop_unlock: unsafe extern "C" fn(*mut c_void),
        pub pa_threaded_mainloop_wait: unsafe extern "C" fn(*mut c_void),
        pub pa_threaded_mainloop_signal: unsafe extern "C" fn(*mut c_void, c_int),
        pub pa_threaded_mainloop_get_api: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
        pub pa_context_new: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
        pub pa_context_unref: unsafe extern "C" fn(*mut c_void),
        pub pa_context_connect:
            unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_void) -> c_int,
        pub pa_context_disconnect: unsafe extern "C" fn(*mut c_void),
        pub pa_context_get_state: unsafe extern "C" fn(*const c_void) -> c_int,
        pub pa_context_set_state_callback: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
            *mut c_void,
        ),
        pub pa_context_set_subscribe_callback: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, c_int, u32, *mut c_void)>,
            *mut c_void,
        ),
        pub pa_context_subscribe: unsafe extern "C" fn(
            *mut c_void,
            c_int,
            Option<unsafe extern "C" fn(*mut c_void, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_get_server_info: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *const PaServerInfo, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_get_sink_info_list: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *const PaSinkInfo, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_get_source_info_list: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *const PaSourceInfo, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_get_sink_input_info_list: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *const PaSinkInputInfo, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_get_client_info_list: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *const PaClientInfo, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_move_sink_input_by_index: unsafe extern "C" fn(
            *mut c_void,
            u32,
            u32,
            Option<unsafe extern "C" fn(*mut c_void, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_move_sink_input_by_name: unsafe extern "C" fn(
            *mut c_void,
            u32,
            *const c_char,
            Option<unsafe extern "C" fn(*mut c_void, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_load_module: unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_context_unload_module: unsafe extern "C" fn(
            *mut c_void,
            u32,
            Option<unsafe extern "C" fn(*mut c_void, c_int, *mut c_void)>,
            *mut c_void,
        ) -> *mut c_void,
        pub pa_operation_unref: unsafe extern "C" fn(*mut c_void),
        pub pa_operation_get_state: unsafe extern "C" fn(*const c_void) -> c_int,
        pub pa_proplist_gets: unsafe extern "C" fn(*const c_void, *const c_char) -> *const c_char,
        pub pa_stream_new: unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const PaSampleSpec,
            *const PaChannelMap,
        ) -> *mut c_void,
        pub pa_stream_unref: unsafe extern "C" fn(*mut c_void),
        pub pa_stream_get_state: unsafe extern "C" fn(*const c_void) -> c_int,
        pub pa_stream_set_state_callback: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
            *mut c_void,
        ),
        pub pa_stream_set_read_callback: unsafe extern "C" fn(
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, usize, *mut c_void)>,
            *mut c_void,
        ),
        pub pa_stream_set_monitor_stream: unsafe extern "C" fn(*mut c_void, u32) -> c_int,
        pub pa_stream_connect_record:
            unsafe extern "C" fn(*mut c_void, *const c_char, *const PaBufferAttr, c_int) -> c_int,
        pub pa_stream_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pa_stream_peek:
            unsafe extern "C" fn(*mut c_void, *mut *const c_void, *mut usize) -> c_int,
        pub pa_stream_drop: unsafe extern "C" fn(*mut c_void) -> c_int,
        pub pa_stream_get_time: unsafe extern "C" fn(*mut c_void, *mut u64) -> c_int,
        pub pa_stream_get_sample_spec: unsafe extern "C" fn(*mut c_void) -> *const PaSampleSpec,
        _lib: Library,
    }

    impl PulseLibrary {
        pub fn load(paths: &LinuxClientPaths) -> Result<Self, ClientLoadError> {
            // SAFETY: dynamically loads the requested system PulseAudio shared library and resolves
            // its C ABI symbols while keeping `Library` alive inside `Self`.
            unsafe {
                let lib = Library::new(&paths.pulse_so)
                    .map_err(|_| ClientLoadError::LibraryAbsent(paths.pulse_so.clone()))?;
                for &sym in REQUIRED_PULSE_SYMBOLS {
                    let sym_name = CStr::from_bytes_with_nul(sym)
                        .ok()
                        .and_then(|s| s.to_str().ok())
                        .unwrap_or("unknown");
                    if lib.get::<*const c_void>(sym).is_err() {
                        return Err(ClientLoadError::SymbolMissing(sym_name.to_owned()));
                    }
                }
                let pa_get_library_version: unsafe extern "C" fn() -> *const c_char =
                    *lib.get(b"pa_get_library_version\0").unwrap();
                let ver_ptr = pa_get_library_version();
                let version = if ver_ptr.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(ver_ptr).to_string_lossy().into_owned()
                };
                validate_version_floor(&version, paths.pulse_min_version, "15.0.0")?;

                Ok(Self {
                    version,
                    pa_get_library_version,
                    pa_strerror: *lib.get(b"pa_strerror\0").unwrap(),
                    pa_threaded_mainloop_new: *lib.get(b"pa_threaded_mainloop_new\0").unwrap(),
                    pa_threaded_mainloop_free: *lib.get(b"pa_threaded_mainloop_free\0").unwrap(),
                    pa_threaded_mainloop_start: *lib.get(b"pa_threaded_mainloop_start\0").unwrap(),
                    pa_threaded_mainloop_stop: *lib.get(b"pa_threaded_mainloop_stop\0").unwrap(),
                    pa_threaded_mainloop_lock: *lib.get(b"pa_threaded_mainloop_lock\0").unwrap(),
                    pa_threaded_mainloop_unlock: *lib
                        .get(b"pa_threaded_mainloop_unlock\0")
                        .unwrap(),
                    pa_threaded_mainloop_wait: *lib.get(b"pa_threaded_mainloop_wait\0").unwrap(),
                    pa_threaded_mainloop_signal: *lib
                        .get(b"pa_threaded_mainloop_signal\0")
                        .unwrap(),
                    pa_threaded_mainloop_get_api: *lib
                        .get(b"pa_threaded_mainloop_get_api\0")
                        .unwrap(),
                    pa_context_new: *lib.get(b"pa_context_new\0").unwrap(),
                    pa_context_unref: *lib.get(b"pa_context_unref\0").unwrap(),
                    pa_context_connect: *lib.get(b"pa_context_connect\0").unwrap(),
                    pa_context_disconnect: *lib.get(b"pa_context_disconnect\0").unwrap(),
                    pa_context_get_state: *lib.get(b"pa_context_get_state\0").unwrap(),
                    pa_context_set_state_callback: *lib
                        .get(b"pa_context_set_state_callback\0")
                        .unwrap(),
                    pa_context_set_subscribe_callback: *lib
                        .get(b"pa_context_set_subscribe_callback\0")
                        .unwrap(),
                    pa_context_subscribe: *lib.get(b"pa_context_subscribe\0").unwrap(),
                    pa_context_get_server_info: *lib.get(b"pa_context_get_server_info\0").unwrap(),
                    pa_context_get_sink_info_list: *lib
                        .get(b"pa_context_get_sink_info_list\0")
                        .unwrap(),
                    pa_context_get_source_info_list: *lib
                        .get(b"pa_context_get_source_info_list\0")
                        .unwrap(),
                    pa_context_get_sink_input_info_list: *lib
                        .get(b"pa_context_get_sink_input_info_list\0")
                        .unwrap(),
                    pa_context_get_client_info_list: *lib
                        .get(b"pa_context_get_client_info_list\0")
                        .unwrap(),
                    pa_context_move_sink_input_by_index: *lib
                        .get(b"pa_context_move_sink_input_by_index\0")
                        .unwrap(),
                    pa_context_move_sink_input_by_name: *lib
                        .get(b"pa_context_move_sink_input_by_name\0")
                        .unwrap(),
                    pa_context_load_module: *lib.get(b"pa_context_load_module\0").unwrap(),
                    pa_context_unload_module: *lib.get(b"pa_context_unload_module\0").unwrap(),
                    pa_operation_unref: *lib.get(b"pa_operation_unref\0").unwrap(),
                    pa_operation_get_state: *lib.get(b"pa_operation_get_state\0").unwrap(),
                    pa_proplist_gets: *lib.get(b"pa_proplist_gets\0").unwrap(),
                    pa_stream_new: *lib.get(b"pa_stream_new\0").unwrap(),
                    pa_stream_unref: *lib.get(b"pa_stream_unref\0").unwrap(),
                    pa_stream_get_state: *lib.get(b"pa_stream_get_state\0").unwrap(),
                    pa_stream_set_state_callback: *lib
                        .get(b"pa_stream_set_state_callback\0")
                        .unwrap(),
                    pa_stream_set_read_callback: *lib
                        .get(b"pa_stream_set_read_callback\0")
                        .unwrap(),
                    pa_stream_set_monitor_stream: *lib
                        .get(b"pa_stream_set_monitor_stream\0")
                        .unwrap(),
                    pa_stream_connect_record: *lib.get(b"pa_stream_connect_record\0").unwrap(),
                    pa_stream_disconnect: *lib.get(b"pa_stream_disconnect\0").unwrap(),
                    pa_stream_peek: *lib.get(b"pa_stream_peek\0").unwrap(),
                    pa_stream_drop: *lib.get(b"pa_stream_drop\0").unwrap(),
                    pa_stream_get_time: *lib.get(b"pa_stream_get_time\0").unwrap(),
                    pa_stream_get_sample_spec: *lib.get(b"pa_stream_get_sample_spec\0").unwrap(),
                    _lib: lib,
                })
            }
        }

        /// Lookup a string property in a PulseAudio `pa_proplist *`.
        ///
        /// # Safety
        /// `proplist` must be valid for the call.
        pub unsafe fn proplist_get(&self, proplist: *const c_void, key: &CStr) -> Option<String> {
            if proplist.is_null() {
                return None;
            }
            // SAFETY: `proplist` and `key` are valid C pointers.
            let val_ptr = unsafe { (self.pa_proplist_gets)(proplist, key.as_ptr()) };
            if val_ptr.is_null() {
                None
            } else {
                // SAFETY: `val_ptr` is a NUL-terminated string owned by `proplist`.
                Some(
                    unsafe { CStr::from_ptr(val_ptr) }
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        }
    }

    pub fn probe_pipewire_server(
        paths: &LinuxClientPaths,
    ) -> Result<PipeWireServerIdentity, CapabilityError> {
        let lib = PipeWireLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PipeWire, paths.pipewire_min_version)
        })?;
        struct ProbeState {
            loop_ptr: *mut c_void,
            server_version: String,
            server_name: String,
            default_sink: Option<String>,
            failed: bool,
            sync_seq: c_int,
        }
        unsafe extern "C" fn on_core_info(data: *mut c_void, info: *const PwCoreInfo) {
            // SAFETY: `data` points to `ProbeState` and `info` is valid during the callback.
            unsafe {
                let state = &mut *(data as *mut ProbeState);
                if !info.is_null() {
                    let inf = &*info;
                    if !inf.version.is_null() {
                        state.server_version =
                            CStr::from_ptr(inf.version).to_string_lossy().into_owned();
                    }
                    if !inf.name.is_null() {
                        state.server_name = CStr::from_ptr(inf.name).to_string_lossy().into_owned();
                    }
                }
            }
        }
        unsafe extern "C" fn on_core_done(data: *mut c_void, id: u32, seq: c_int) {
            // SAFETY: `data` points to `ProbeState`.
            unsafe {
                let state = &mut *(data as *mut ProbeState);
                if id == 0 && seq == state.sync_seq {
                    state.sync_seq = -1;
                }
            }
        }
        unsafe extern "C" fn on_core_error(
            data: *mut c_void,
            _id: u32,
            _seq: c_int,
            _res: c_int,
            _msg: *const c_char,
        ) {
            // SAFETY: `data` points to `ProbeState`.
            unsafe {
                let state = &mut *(data as *mut ProbeState);
                state.failed = true;
            }
        }

        // SAFETY: all PipeWire handles are created, synchronized, and destroyed on this thread.
        unsafe {
            let main_loop = (lib.pw_main_loop_new)(std::ptr::null());
            if main_loop.is_null() {
                return Err(CapabilityError::ServerUnavailable);
            }
            let loop_obj = (lib.pw_main_loop_get_loop)(main_loop);
            let context = (lib.pw_context_new)(loop_obj, std::ptr::null_mut(), 0);
            if context.is_null() {
                (lib.pw_main_loop_destroy)(main_loop);
                return Err(CapabilityError::ServerUnavailable);
            }
            let core = (lib.pw_context_connect)(context, std::ptr::null_mut(), 0);
            if core.is_null() {
                (lib.pw_context_destroy)(context);
                (lib.pw_main_loop_destroy)(main_loop);
                return Err(CapabilityError::ServerUnavailable);
            }
            let mut state = ProbeState {
                loop_ptr: main_loop,
                server_version: lib.version.clone(),
                server_name: "pipewire-0".to_owned(),
                default_sink: None,
                failed: false,
                sync_seq: 0,
            };
            let events = PwCoreEvents {
                version: 0,
                info: Some(on_core_info),
                done: Some(on_core_done),
                ping: None,
                error: Some(on_core_error),
                remove_id: None,
                bound_id: None,
                add_mem: None,
                remove_mem: None,
                bound_props: None,
            };
            let mut hook = SpaHook::default();
            lib.core_add_listener(
                core,
                &mut hook,
                &events,
                (&mut state as *mut ProbeState).cast(),
            );
            state.sync_seq = lib.core_sync(core, 0, 1);
            let _ = state.loop_ptr;
            (lib.pw_core_disconnect)(core);
            (lib.pw_context_destroy)(context);
            (lib.pw_main_loop_destroy)(main_loop);
            if state.failed {
                return Err(CapabilityError::ServerUnavailable);
            }
            Ok(PipeWireServerIdentity {
                library_version: lib.version,
                server_version: state.server_version,
                server_name: state.server_name,
                default_sink_name: state.default_sink,
                default_source_name: None,
            })
        }
    }

    pub fn probe_pulse_server(
        paths: &LinuxClientPaths,
    ) -> Result<PulseServerIdentity, CapabilityError> {
        let lib = PulseLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PulseAudio, paths.pulse_min_version)
        })?;

        struct PulseProbeCtx {
            mainloop: *mut c_void,
            signal_fn: unsafe extern "C" fn(*mut c_void, c_int),
            server_name: Option<String>,
            server_version: Option<String>,
            default_sink_name: Option<String>,
            default_source_name: Option<String>,
            done: std::sync::atomic::AtomicBool,
        }

        unsafe extern "C" fn on_ctx_state(ctx: *mut c_void, userdata: *mut c_void) {
            // SAFETY: `userdata` points to `PulseProbeCtx`.
            unsafe {
                let _ = ctx;
                let probe = &*(userdata as *const PulseProbeCtx);
                (probe.signal_fn)(probe.mainloop, 0);
            }
        }

        unsafe extern "C" fn on_server_info(
            _ctx: *mut c_void,
            info: *const PaServerInfo,
            userdata: *mut c_void,
        ) {
            // SAFETY: `userdata` points to `PulseProbeCtx` and `info` is valid during the callback.
            unsafe {
                let probe = &mut *(userdata as *mut PulseProbeCtx);
                if !info.is_null() {
                    let inf = &*info;
                    if !inf.server_name.is_null() {
                        probe.server_name = Some(
                            CStr::from_ptr(inf.server_name)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                    if !inf.server_version.is_null() {
                        probe.server_version = Some(
                            CStr::from_ptr(inf.server_version)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                    if !inf.default_sink_name.is_null() {
                        probe.default_sink_name = Some(
                            CStr::from_ptr(inf.default_sink_name)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                    if !inf.default_source_name.is_null() {
                        probe.default_source_name = Some(
                            CStr::from_ptr(inf.default_source_name)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
                probe.done.store(true, std::sync::atomic::Ordering::SeqCst);
                (probe.signal_fn)(probe.mainloop, 0);
            }
        }

        // SAFETY: `pa_threaded_mainloop` and `pa_context` follow standard locking and unref rules.
        unsafe {
            let mainloop = (lib.pa_threaded_mainloop_new)();
            if mainloop.is_null() {
                return Err(CapabilityError::ServerUnavailable);
            }
            let api = (lib.pa_threaded_mainloop_get_api)(mainloop);
            let ctx = (lib.pa_context_new)(api, c"cspa-probe".as_ptr());
            if ctx.is_null() {
                (lib.pa_threaded_mainloop_free)(mainloop);
                return Err(CapabilityError::ServerUnavailable);
            }
            let mut probe = Box::new(PulseProbeCtx {
                mainloop,
                signal_fn: lib.pa_threaded_mainloop_signal,
                server_name: None,
                server_version: None,
                default_sink_name: None,
                default_source_name: None,
                done: std::sync::atomic::AtomicBool::new(false),
            });
            let probe_ptr: *mut c_void = (&mut *probe as *mut PulseProbeCtx).cast();
            (lib.pa_context_set_state_callback)(ctx, Some(on_ctx_state), probe_ptr);
            if (lib.pa_context_connect)(
                ctx,
                std::ptr::null(),
                PA_CONTEXT_NOAUTOSPAWN,
                std::ptr::null(),
            ) < 0
            {
                (lib.pa_context_set_state_callback)(ctx, None, std::ptr::null_mut());
                (lib.pa_context_unref)(ctx);
                (lib.pa_threaded_mainloop_free)(mainloop);
                return Err(CapabilityError::ServerUnavailable);
            }
            if (lib.pa_threaded_mainloop_start)(mainloop) < 0 {
                (lib.pa_context_set_state_callback)(ctx, None, std::ptr::null_mut());
                (lib.pa_context_disconnect)(ctx);
                (lib.pa_context_unref)(ctx);
                (lib.pa_threaded_mainloop_free)(mainloop);
                return Err(CapabilityError::ServerUnavailable);
            }
            (lib.pa_threaded_mainloop_lock)(mainloop);
            loop {
                let state = (lib.pa_context_get_state)(ctx);
                if state == PA_CONTEXT_READY {
                    break;
                }
                if state == PA_CONTEXT_FAILED || state == PA_CONTEXT_TERMINATED {
                    (lib.pa_context_set_state_callback)(ctx, None, std::ptr::null_mut());
                    (lib.pa_threaded_mainloop_unlock)(mainloop);
                    (lib.pa_threaded_mainloop_stop)(mainloop);
                    (lib.pa_context_disconnect)(ctx);
                    (lib.pa_context_unref)(ctx);
                    (lib.pa_threaded_mainloop_free)(mainloop);
                    return Err(CapabilityError::ServerUnavailable);
                }
                (lib.pa_threaded_mainloop_wait)(mainloop);
            }
            let op = (lib.pa_context_get_server_info)(ctx, Some(on_server_info), probe_ptr);
            if !op.is_null() {
                while !probe.done.load(std::sync::atomic::Ordering::SeqCst) {
                    (lib.pa_threaded_mainloop_wait)(mainloop);
                }
                (lib.pa_operation_unref)(op);
            }
            (lib.pa_context_set_state_callback)(ctx, None, std::ptr::null_mut());
            (lib.pa_context_disconnect)(ctx);
            (lib.pa_threaded_mainloop_unlock)(mainloop);
            (lib.pa_threaded_mainloop_stop)(mainloop);
            (lib.pa_context_unref)(ctx);
            (lib.pa_threaded_mainloop_free)(mainloop);

            let server_name = probe
                .server_name
                .ok_or(CapabilityError::ServerUnavailable)?;
            let server_version = probe.server_version.unwrap_or_default();
            let server_kind = PulseServerKind::classify(&server_name);
            Ok(PulseServerIdentity {
                library_version: lib.version,
                server_name,
                server_version,
                server_kind,
                default_sink_name: probe.default_sink_name,
                default_source_name: probe.default_source_name,
            })
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::{PipeWireLibrary, PulseLibrary, probe_pipewire_server, probe_pulse_server};

#[cfg(not(target_os = "linux"))]
pub fn probe_pipewire_server(
    paths: &LinuxClientPaths,
) -> Result<PipeWireServerIdentity, CapabilityError> {
    Err(ClientLoadError::LibraryAbsent(paths.pipewire_so.clone())
        .into_capability_error(BackendId::PipeWire, paths.pipewire_min_version))
}

#[cfg(not(target_os = "linux"))]
pub fn probe_pulse_server(
    paths: &LinuxClientPaths,
) -> Result<PulseServerIdentity, CapabilityError> {
    Err(ClientLoadError::LibraryAbsent(paths.pulse_so.clone())
        .into_capability_error(BackendId::PulseAudio, paths.pulse_min_version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_floor_and_spa_pod_builder_invariants() {
        assert_eq!(
            validate_version_floor("1.6.2", MIN_PIPEWIRE_VERSION, "0.3.65"),
            Ok((1, 6, 2))
        );
        assert_eq!(
            validate_version_floor("17.0.0", MIN_PULSEAUDIO_VERSION, "15.0.0"),
            Ok((17, 0, 0))
        );
        assert!(validate_version_floor("0.3.20", MIN_PIPEWIRE_VERSION, "0.3.65").is_err());
        assert!(validate_version_floor("12.0", MIN_PULSEAUDIO_VERSION, "15.0.0").is_err());

        let pod_48k = build_spa_f32_le_stereo_pod(48_000).0;
        assert_eq!(u32::from_le_bytes(pod_48k[0..4].try_into().unwrap()), 160);
        assert_eq!(u32::from_le_bytes(pod_48k[4..8].try_into().unwrap()), 15);
        assert_eq!(
            u32::from_le_bytes(pod_48k[80..84].try_into().unwrap()),
            SPA_AUDIO_FORMAT_F32_LE
        );
        assert_eq!(
            u32::from_le_bytes(pod_48k[104..108].try_into().unwrap()),
            48_000
        );
        assert_eq!(u32::from_le_bytes(pod_48k[128..132].try_into().unwrap()), 2);

        let pod_16k_mono_i16 = build_spa_audio_raw_pod(SampleType::I16, 16_000, 1)
            .unwrap()
            .0;
        assert_eq!(
            u32::from_le_bytes(pod_16k_mono_i16[80..84].try_into().unwrap()),
            SPA_AUDIO_FORMAT_S16_LE
        );
        assert_eq!(
            u32::from_le_bytes(pod_16k_mono_i16[104..108].try_into().unwrap()),
            16_000
        );
        assert_eq!(
            u32::from_le_bytes(pod_16k_mono_i16[128..132].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(pod_16k_mono_i16[144..148].try_into().unwrap()),
            12
        );
        assert_eq!(
            u32::from_le_bytes(pod_16k_mono_i16[160..164].try_into().unwrap()),
            SPA_AUDIO_CHANNEL_MONO
        );
        assert!(build_spa_audio_raw_pod(SampleType::F32, 48_000, 3).is_err());
        assert!(build_spa_audio_raw_pod(SampleType::F32, 0, 2).is_err());
    }
}
