use crate::{
    CSPA_ACTIVE_AUDIO_ACTIVE, CSPA_ACTIVE_AUDIO_INACTIVE, CSPA_ACTIVE_AUDIO_UNKNOWN,
    CSPA_BACKEND_COREAUDIO, CSPA_BACKEND_NONE, CSPA_BACKEND_PIPEWIRE, CSPA_BACKEND_PULSEAUDIO,
    CSPA_BACKEND_SCREENCAPTUREKIT, CSPA_BACKEND_WASAPI, CSPA_BYTE_ORDER_BIG,
    CSPA_BYTE_ORDER_LITTLE, CSPA_COMBINE_MODE_AUTO, CSPA_COMBINE_MODE_CSPA_MIXER,
    CSPA_COMBINE_MODE_NATIVE_ONLY, CSPA_COMBINE_RESOLUTION_CSPA_MIXER,
    CSPA_COMBINE_RESOLUTION_PASS_THROUGH, CSPA_COMBINE_RESOLUTION_SHARED_NATIVE_GRAPH_MIX,
    CSPA_DEFAULT_DEVICE_FOLLOW, CSPA_DEFAULT_DEVICE_PIN, CSPA_DISCOVERY_DEDUPE_BY_LOGICAL_TARGET,
    CSPA_DISCOVERY_DEDUPE_PER_BACKEND, CSPA_DISCOVERY_ORDER_BACKEND_THEN_KIND,
    CSPA_DISCOVERY_ORDER_KIND_THEN_BACKEND, CSPA_DITHER_NONE, CSPA_DITHER_TPDF,
    CSPA_EXPERIMENTAL_SCHEMA_REVISION, CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING,
    CSPA_INPUT_CONVERSION_STRICT_NO_RESAMPLE, CSPA_ON_FORMAT_CHANGE_ADAPT,
    CSPA_ON_FORMAT_CHANGE_FAIL, CSPA_ON_INPUT_FAILURE_DEGRADE, CSPA_ON_INPUT_FAILURE_FAIL,
    CSPA_ON_ROOT_EXIT_STOP, CSPA_ON_ROOT_EXIT_WAIT, CSPA_OVERFLOW_DROP_NEWEST,
    CSPA_OVERFLOW_DROP_OLDEST, CSPA_OVERFLOW_FAIL, CSPA_OVERLAP_NATIVE_SET_UNION,
    CSPA_OVERLAP_SINGLE_TARGET, CSPA_OVERLAP_SOFTWARE_SUMMATION, CSPA_PERMISSION_KIND_COUNT,
    CSPA_PERMISSION_KIND_MICROPHONE, CSPA_PERMISSION_KIND_SCREEN_RECORDING,
    CSPA_PERMISSION_KIND_SYSTEM_AUDIO, CSPA_PERMISSION_POLICY_AUTO_REQUEST,
    CSPA_PERMISSION_POLICY_CHECK_ONLY, CSPA_PERMISSION_STATE_DENIED, CSPA_PERMISSION_STATE_GRANTED,
    CSPA_PERMISSION_STATE_NOT_DETERMINED, CSPA_PERMISSION_STATE_RESTRICTED,
    CSPA_PERMISSION_STATE_UNKNOWN, CSPA_PLATFORM_OS_LINUX, CSPA_PLATFORM_OS_MACOS,
    CSPA_PLATFORM_OS_WINDOWS, CSPA_SAMPLE_FORMAT_F32, CSPA_SAMPLE_FORMAT_I16,
    CSPA_SAMPLE_FORMAT_I24, CSPA_SAMPLE_FORMAT_I32, CSPA_SCOPE_EXACT, CSPA_SCOPE_TREE,
    CSPA_STATUS_ABI_MISMATCH, CSPA_STATUS_INVALID_ARGUMENT, CSPA_STATUS_NO_MEMORY,
    CSPA_TARGET_KIND_APPLICATION, CSPA_TARGET_KIND_DEFAULT_INPUT, CSPA_TARGET_KIND_DEFAULT_OUTPUT,
    CSPA_TARGET_KIND_DEVICE, CSPA_TARGET_KIND_INPUT_DEVICE, CSPA_TARGET_KIND_PROCESS,
    CSPA_TREE_STRATEGY_NATIVE_GRAPH_MIX, CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE,
    CSPA_TREE_STRATEGY_SOFTWARE_STREAM_MIX, CspaOwnerHandle,
};
use capture_system_or_process_audio::{
    AggregateManagerBudgets, BackendId, Backends, ByteOrder, CapturePlanReport, CaptureRequest,
    CaptureTarget, Channel, CombineMode, CombineResolution, DefaultDeviceMode, DiscoveryDedupe,
    DiscoveryOrder, DiscoveryQuery, DitherMode, Format, Header, InputConversionPolicy, Layout,
    MixedBlockMetadata, MixerConfig, OnFormatChange, OnInputFailure, OnRootExit,
    OutputConverterConfig, Overflow, OverlapCombinationSemantics, PermissionEntry, PermissionKind,
    PermissionPolicy, PermissionReport, PermissionState, PlatformOs, ProcessIdentity, ProcessScope,
    ProcessTreeRange, ProcessTreeStrategy, Provenance, SampleType, TargetKind,
    core::schema::TargetAcquisition,
};
use std::{mem::size_of, sync::Arc};

pub const CSPA_DEVICE_ID_MAX_BYTES: usize = 256;
pub const CSPA_APP_ID_MAX_BYTES: usize = 128;
pub const CSPA_NAME_MAX_BYTES: usize = 128;
pub const CSPA_EXECUTABLE_PATH_MAX_BYTES: usize = 256;
pub const CSPA_EVENT_SUBJECT_MAX_BYTES: usize = 256;

#[inline]
pub fn encode_backend_opt(backend: Option<BackendId>) -> u32 {
    match backend {
        None => CSPA_BACKEND_NONE,
        Some(b) => encode_backend(b),
    }
}

#[inline]
pub fn encode_backend(backend: BackendId) -> u32 {
    match backend {
        BackendId::Wasapi => CSPA_BACKEND_WASAPI,
        BackendId::PipeWire => CSPA_BACKEND_PIPEWIRE,
        BackendId::PulseAudio => CSPA_BACKEND_PULSEAUDIO,
        BackendId::CoreAudio => CSPA_BACKEND_COREAUDIO,
        BackendId::ScreenCaptureKit => CSPA_BACKEND_SCREENCAPTUREKIT,
    }
}

#[inline]
pub fn decode_backend(code: u32) -> Result<BackendId, u32> {
    match code {
        CSPA_BACKEND_WASAPI => Ok(BackendId::Wasapi),
        CSPA_BACKEND_PIPEWIRE => Ok(BackendId::PipeWire),
        CSPA_BACKEND_PULSEAUDIO => Ok(BackendId::PulseAudio),
        CSPA_BACKEND_COREAUDIO => Ok(BackendId::CoreAudio),
        CSPA_BACKEND_SCREENCAPTUREKIT => Ok(BackendId::ScreenCaptureKit),
        _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
    }
}

#[inline]
pub fn decode_backend_opt(code: u32) -> Result<Option<BackendId>, u32> {
    if code == CSPA_BACKEND_NONE {
        Ok(None)
    } else {
        decode_backend(code).map(Some)
    }
}

pub fn encode_backend_list(list: &[BackendId]) -> u32 {
    let mut packed = 0u32;
    for (idx, id) in list.iter().take(4).enumerate() {
        packed |= encode_backend(*id) << (idx * 8);
    }
    packed
}

pub fn decode_backend_list(mut packed: u32) -> Result<Vec<BackendId>, u32> {
    let mut out = Vec::new();
    while packed != 0 {
        let code = packed & 0xff;
        out.push(decode_backend(code)?);
        packed >>= 8;
    }
    Ok(out)
}

#[inline]
pub fn encode_sample_type(sample: SampleType) -> u32 {
    match sample {
        SampleType::I16 => CSPA_SAMPLE_FORMAT_I16,
        SampleType::I24 => CSPA_SAMPLE_FORMAT_I24,
        SampleType::F32 => CSPA_SAMPLE_FORMAT_F32,
        SampleType::I32 => CSPA_SAMPLE_FORMAT_I32,
    }
}

#[inline]
pub fn decode_sample_type(code: u32) -> Result<SampleType, u32> {
    match code {
        CSPA_SAMPLE_FORMAT_I16 => Ok(SampleType::I16),
        CSPA_SAMPLE_FORMAT_I24 => Ok(SampleType::I24),
        CSPA_SAMPLE_FORMAT_F32 => Ok(SampleType::F32),
        CSPA_SAMPLE_FORMAT_I32 => Ok(SampleType::I32),
        _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
    }
}

#[inline]
pub fn encode_byte_order(order: ByteOrder) -> u32 {
    match order {
        ByteOrder::Little => CSPA_BYTE_ORDER_LITTLE,
        ByteOrder::Big => CSPA_BYTE_ORDER_BIG,
    }
}

#[inline]
pub fn decode_byte_order(code: u32) -> Result<ByteOrder, u32> {
    match code {
        CSPA_BYTE_ORDER_LITTLE => Ok(ByteOrder::Little),
        CSPA_BYTE_ORDER_BIG => Ok(ByteOrder::Big),
        _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
    }
}

#[inline]
pub fn decode_overflow(code: u32) -> Result<Overflow, u32> {
    match code {
        CSPA_OVERFLOW_DROP_OLDEST => Ok(Overflow::DropOldest),
        CSPA_OVERFLOW_DROP_NEWEST => Ok(Overflow::DropNewest),
        CSPA_OVERFLOW_FAIL => Ok(Overflow::Fail),
        _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
    }
}

#[inline]
pub fn encode_permission_policy(policy: PermissionPolicy) -> u32 {
    match policy {
        PermissionPolicy::AutoRequest => CSPA_PERMISSION_POLICY_AUTO_REQUEST,
        PermissionPolicy::CheckOnly => CSPA_PERMISSION_POLICY_CHECK_ONLY,
    }
}

#[inline]
pub fn decode_permission_policy(code: u32) -> Result<PermissionPolicy, u32> {
    match code {
        CSPA_PERMISSION_POLICY_AUTO_REQUEST => Ok(PermissionPolicy::AutoRequest),
        CSPA_PERMISSION_POLICY_CHECK_ONLY => Ok(PermissionPolicy::CheckOnly),
        _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
    }
}

#[inline]
pub fn encode_permission_kind(kind: PermissionKind) -> u32 {
    match kind {
        PermissionKind::Microphone => CSPA_PERMISSION_KIND_MICROPHONE,
        PermissionKind::SystemAudio => CSPA_PERMISSION_KIND_SYSTEM_AUDIO,
        PermissionKind::ScreenRecording => CSPA_PERMISSION_KIND_SCREEN_RECORDING,
    }
}

#[inline]
pub fn encode_permission_state(state: PermissionState) -> u32 {
    match state {
        PermissionState::Granted => CSPA_PERMISSION_STATE_GRANTED,
        PermissionState::NotDetermined => CSPA_PERMISSION_STATE_NOT_DETERMINED,
        PermissionState::Denied => CSPA_PERMISSION_STATE_DENIED,
        PermissionState::Restricted => CSPA_PERMISSION_STATE_RESTRICTED,
        PermissionState::Unknown => CSPA_PERMISSION_STATE_UNKNOWN,
    }
}

#[inline]
pub fn encode_platform_os(os: PlatformOs) -> u32 {
    match os {
        PlatformOs::Windows => CSPA_PLATFORM_OS_WINDOWS,
        PlatformOs::Linux => CSPA_PLATFORM_OS_LINUX,
        PlatformOs::MacOs => CSPA_PLATFORM_OS_MACOS,
    }
}

#[inline]
pub fn decode_platform_os(code: u32) -> Result<PlatformOs, u32> {
    match code {
        CSPA_PLATFORM_OS_WINDOWS => Ok(PlatformOs::Windows),
        CSPA_PLATFORM_OS_LINUX => Ok(PlatformOs::Linux),
        CSPA_PLATFORM_OS_MACOS => Ok(PlatformOs::MacOs),
        _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
    }
}

pub fn write_truncating_cstr<const N: usize>(buf: &mut [u8; N], text: Option<&str>) {
    buf.fill(0);
    let Some(s) = text else {
        return;
    };
    let cap = N.saturating_sub(1);
    let mut end = s.len().min(cap);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    buf[..end].copy_from_slice(&s.as_bytes()[..end]);
}

pub fn read_bounded_cstr(buf: &[u8]) -> Result<Option<&str>, u32> {
    let Some(nul_pos) = buf.iter().position(|b| *b == 0) else {
        return Err(CSPA_STATUS_INVALID_ARGUMENT);
    };
    if nul_pos == 0 {
        return Ok(None);
    }
    let s = std::str::from_utf8(&buf[..nul_pos]).map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
    Ok(Some(s))
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentityRecord {
    pub pid: u32,
    pub reserved: u32,
    pub generation: u64,
}

impl From<ProcessIdentity> for ProcessIdentityRecord {
    fn from(identity: ProcessIdentity) -> Self {
        Self {
            pid: identity.pid,
            reserved: 0,
            generation: identity.generation,
        }
    }
}

impl TryFrom<ProcessIdentityRecord> for ProcessIdentity {
    type Error = u32;
    fn try_from(record: ProcessIdentityRecord) -> Result<Self, u32> {
        if record.reserved != 0 || record.pid == 0 {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            pid: record.pid,
            generation: record.generation,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessTreeRangeRecord {
    pub start_level: u32,
    pub fallback_limit: u32,
}

impl Default for ProcessTreeRangeRecord {
    fn default() -> Self {
        Self::from(ProcessTreeRange::default())
    }
}

impl From<ProcessTreeRange> for ProcessTreeRangeRecord {
    fn from(range: ProcessTreeRange) -> Self {
        Self {
            start_level: range.start_level as u32,
            fallback_limit: range.fallback_limit.map_or(0, |level| level as u32),
        }
    }
}

impl TryFrom<ProcessTreeRangeRecord> for ProcessTreeRange {
    type Error = u32;
    fn try_from(record: ProcessTreeRangeRecord) -> Result<Self, u32> {
        let map_level = |code: u32| match code {
            CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE => {
                Ok(ProcessTreeStrategy::NativeKernelCapture)
            }
            CSPA_TREE_STRATEGY_NATIVE_GRAPH_MIX => Ok(ProcessTreeStrategy::NativeGraphMix),
            CSPA_TREE_STRATEGY_SOFTWARE_STREAM_MIX => Ok(ProcessTreeStrategy::SoftwareStreamMix),
            _ => Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let start_level = map_level(record.start_level)?;
        let fallback_limit = if record.fallback_limit == 0 {
            None
        } else {
            Some(map_level(record.fallback_limit)?)
        };
        let range = ProcessTreeRange {
            start_level,
            fallback_limit,
        };
        range.validate().map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
        Ok(range)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendsRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub windows_mask: u32,
    pub linux_mask: u32,
    pub macos_mask: u32,
    pub reserved: u32,
}

impl Default for BackendsRecord {
    fn default() -> Self {
        Self::from(&Backends::default())
    }
}

impl From<&Backends> for BackendsRecord {
    fn from(backends: &Backends) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            windows_mask: encode_backend_list(&backends.windows),
            linux_mask: encode_backend_list(&backends.linux),
            macos_mask: encode_backend_list(&backends.macos),
            reserved: 0,
        }
    }
}

impl BackendsRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn decode(&self) -> Result<Backends, u32> {
        self.check_abi()?;
        if self.reserved != 0 {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        Backends::new(
            Some(decode_backend_list(self.windows_mask)?),
            Some(decode_backend_list(self.linux_mask)?),
            Some(decode_backend_list(self.macos_mask)?),
        )
        .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CspaPcmFormatRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub sample_format: u32,
    pub byte_order: u32,
    pub rate_hz: u32,
    pub channels: u32,
}

impl CspaPcmFormatRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }
}

impl From<&Format> for CspaPcmFormatRecord {
    fn from(format: &Format) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            sample_format: encode_sample_type(format.sample()),
            byte_order: encode_byte_order(format.order()),
            rate_hz: format.rate(),
            channels: u32::from(format.channels()),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureTargetRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub kind: u32,
    pub scope: u32,
    pub on_root_exit: u32,
    pub default_device_mode: u32,
    pub backend: u32,
    pub available_backends_mask: u32,
    pub pid: u32,
    pub gain_bits: u32,
    pub active_audio: u32,
    pub reserved: u32,
    pub process_start_time: u64,
    pub wait_timeout_ns: u64,
    pub device_id: [u8; CSPA_DEVICE_ID_MAX_BYTES],
    pub app_id: [u8; CSPA_APP_ID_MAX_BYTES],
    pub name: [u8; CSPA_NAME_MAX_BYTES],
    pub executable_path: [u8; CSPA_EXECUTABLE_PATH_MAX_BYTES],
}

impl Default for CaptureTargetRecord {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            kind: 0,
            scope: 0,
            on_root_exit: 0,
            default_device_mode: 0,
            backend: 0,
            available_backends_mask: 0,
            pid: 0,
            gain_bits: 0,
            active_audio: 0,
            reserved: 0,
            process_start_time: 0,
            wait_timeout_ns: 0,
            device_id: [0; CSPA_DEVICE_ID_MAX_BYTES],
            app_id: [0; CSPA_APP_ID_MAX_BYTES],
            name: [0; CSPA_NAME_MAX_BYTES],
            executable_path: [0; CSPA_EXECUTABLE_PATH_MAX_BYTES],
        }
    }
}

impl From<&CaptureTarget> for CaptureTargetRecord {
    fn from(target: &CaptureTarget) -> Self {
        let (on_root_exit, wait_timeout_ns) = match target.on_root_exit() {
            OnRootExit::Stop => (CSPA_ON_ROOT_EXIT_STOP, 0),
            OnRootExit::Wait { wait_timeout_ns } => (CSPA_ON_ROOT_EXIT_WAIT, wait_timeout_ns),
        };
        let mut rec = Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            kind: match target.kind() {
                TargetKind::Process => CSPA_TARGET_KIND_PROCESS,
                TargetKind::Application => CSPA_TARGET_KIND_APPLICATION,
                TargetKind::Device => CSPA_TARGET_KIND_DEVICE,
                TargetKind::DefaultOutput => CSPA_TARGET_KIND_DEFAULT_OUTPUT,
                TargetKind::InputDevice => CSPA_TARGET_KIND_INPUT_DEVICE,
                TargetKind::DefaultInput => CSPA_TARGET_KIND_DEFAULT_INPUT,
            },
            scope: match target.scope() {
                ProcessScope::Tree => CSPA_SCOPE_TREE,
                ProcessScope::Exact => CSPA_SCOPE_EXACT,
            },
            on_root_exit,
            default_device_mode: match target.default_device_mode() {
                DefaultDeviceMode::Follow => CSPA_DEFAULT_DEVICE_FOLLOW,
                DefaultDeviceMode::Pin => CSPA_DEFAULT_DEVICE_PIN,
            },
            backend: encode_backend_opt(target.backend()),
            available_backends_mask: encode_backend_list(target.available_backends()),
            pid: target.pid().unwrap_or(0),
            gain_bits: target.gain().to_bits(),
            active_audio: match target.active_audio() {
                None => CSPA_ACTIVE_AUDIO_UNKNOWN,
                Some(false) => CSPA_ACTIVE_AUDIO_INACTIVE,
                Some(true) => CSPA_ACTIVE_AUDIO_ACTIVE,
            },
            reserved: 0,
            process_start_time: target.process_start_time().unwrap_or(0),
            wait_timeout_ns,
            device_id: [0; CSPA_DEVICE_ID_MAX_BYTES],
            app_id: [0; CSPA_APP_ID_MAX_BYTES],
            name: [0; CSPA_NAME_MAX_BYTES],
            executable_path: [0; CSPA_EXECUTABLE_PATH_MAX_BYTES],
        };
        write_truncating_cstr(&mut rec.device_id, target.clean_device_id());
        write_truncating_cstr(&mut rec.app_id, target.app_id().map(|s| s.as_ref()));
        write_truncating_cstr(&mut rec.name, target.name().map(|s| s.as_ref()));
        write_truncating_cstr(
            &mut rec.executable_path,
            target.executable_path().map(|s| s.as_ref()),
        );
        rec
    }
}

impl CaptureTargetRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn decode(&self) -> Result<CaptureTarget, u32> {
        self.check_abi()?;
        if self.reserved != 0 {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let scope = match self.scope {
            CSPA_SCOPE_TREE => ProcessScope::Tree,
            CSPA_SCOPE_EXACT => ProcessScope::Exact,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let on_root_exit = match (self.on_root_exit, self.wait_timeout_ns) {
            (CSPA_ON_ROOT_EXIT_STOP, 0) => OnRootExit::Stop,
            (CSPA_ON_ROOT_EXIT_WAIT, timeout) if timeout > 0 => OnRootExit::Wait {
                wait_timeout_ns: timeout,
            },
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let default_device_mode = match self.default_device_mode {
            CSPA_DEFAULT_DEVICE_FOLLOW => DefaultDeviceMode::Follow,
            CSPA_DEFAULT_DEVICE_PIN => DefaultDeviceMode::Pin,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let backend = decode_backend_opt(self.backend)?;
        let avail_vec = decode_backend_list(self.available_backends_mask)?;
        let gain = f32::from_bits(self.gain_bits);
        if !gain.is_finite() || gain < 0.0 {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let active_audio = match self.active_audio {
            CSPA_ACTIVE_AUDIO_UNKNOWN => None,
            CSPA_ACTIVE_AUDIO_INACTIVE => Some(false),
            CSPA_ACTIVE_AUDIO_ACTIVE => Some(true),
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let dev_id = read_bounded_cstr(&self.device_id)?;
        let app_id = read_bounded_cstr(&self.app_id)?.map(Arc::<str>::from);
        let name = read_bounded_cstr(&self.name)?.map(Arc::<str>::from);
        let exe_path = read_bounded_cstr(&self.executable_path)?.map(Arc::<str>::from);

        let mut target = match self.kind {
            CSPA_TARGET_KIND_PROCESS => {
                if self.pid == 0 || dev_id.is_some() {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                }
                CaptureTarget::process(
                    ProcessIdentity {
                        pid: self.pid,
                        generation: self.process_start_time,
                    },
                    scope,
                    on_root_exit,
                )
                .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?
                .with_metadata(app_id, name, exe_path, active_audio)
            }
            CSPA_TARGET_KIND_APPLICATION => {
                if self.pid == 0 || dev_id.is_some() {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                }
                CaptureTarget::application(
                    ProcessIdentity {
                        pid: self.pid,
                        generation: self.process_start_time,
                    },
                    app_id,
                    name,
                    exe_path,
                    scope,
                    on_root_exit,
                )
                .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?
                .with_active_audio(active_audio)
            }
            CSPA_TARGET_KIND_DEVICE => {
                if self.pid != 0 || self.process_start_time != 0 {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                }
                let Some(id_str) = dev_id else {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                };
                CaptureTarget::device(id_str)
                    .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?
                    .with_name(name)
                    .with_active_audio(active_audio)
            }
            CSPA_TARGET_KIND_INPUT_DEVICE => {
                if self.pid != 0 || self.process_start_time != 0 {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                }
                let Some(id_str) = dev_id else {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                };
                CaptureTarget::input_device(id_str)
                    .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?
                    .with_name(name)
                    .with_active_audio(active_audio)
            }
            CSPA_TARGET_KIND_DEFAULT_OUTPUT => {
                if self.pid != 0 || self.process_start_time != 0 || dev_id.is_some() {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                }
                CaptureTarget::default_output(default_device_mode)
            }
            CSPA_TARGET_KIND_DEFAULT_INPUT => {
                if self.pid != 0 || self.process_start_time != 0 || dev_id.is_some() {
                    return Err(CSPA_STATUS_INVALID_ARGUMENT);
                }
                CaptureTarget::microphone(default_device_mode)
            }
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };

        if !avail_vec.is_empty() {
            target = target.with_available_backends(avail_vec);
        }
        target = target.with_optional_backend(backend);
        target = target
            .with_gain(gain)
            .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
        target
            .validate()
            .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
        Ok(target)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CspaDiscoveryQueryRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub has_backends: u32,
    pub dedupe: u32,
    pub order: u32,
    pub active_only: u32,
    pub include_non_audio: u32,
    pub include_applications: u32,
    pub include_processes: u32,
    pub include_output_devices: u32,
    pub include_input_devices: u32,
    pub reserved: u32,
    pub backends: BackendsRecord,
}

impl Default for CspaDiscoveryQueryRecord {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            has_backends: 0,
            dedupe: CSPA_DISCOVERY_DEDUPE_BY_LOGICAL_TARGET,
            order: CSPA_DISCOVERY_ORDER_KIND_THEN_BACKEND,
            active_only: 0,
            include_non_audio: 0,
            include_applications: 1,
            include_processes: 1,
            include_output_devices: 1,
            include_input_devices: 1,
            reserved: 0,
            backends: BackendsRecord::default(),
        }
    }
}

impl CspaDiscoveryQueryRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn decode_for_os(&self, os: PlatformOs) -> Result<DiscoveryQuery, u32> {
        self.check_abi()?;
        if self.reserved != 0
            || self.has_backends > 1
            || self.active_only > 1
            || self.include_non_audio > 1
            || self.include_applications > 1
            || self.include_processes > 1
            || self.include_output_devices > 1
            || self.include_input_devices > 1
        {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let dedupe = match self.dedupe {
            CSPA_DISCOVERY_DEDUPE_BY_LOGICAL_TARGET => DiscoveryDedupe::ByLogicalTarget,
            CSPA_DISCOVERY_DEDUPE_PER_BACKEND => DiscoveryDedupe::PerBackend,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let order = match self.order {
            CSPA_DISCOVERY_ORDER_KIND_THEN_BACKEND => DiscoveryOrder::KindThenBackend,
            CSPA_DISCOVERY_ORDER_BACKEND_THEN_KIND => DiscoveryOrder::BackendThenKind,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let backends = if self.has_backends != 0 {
            self.backends.decode()?
        } else {
            Backends::default()
        };
        Ok(DiscoveryQuery {
            os,
            backends,
            dedupe,
            order,
            active_only: self.active_only != 0,
            include_non_audio: self.include_non_audio != 0,
            include_applications: self.include_applications != 0,
            include_processes: self.include_processes != 0,
            include_output_devices: self.include_output_devices != 0,
            include_input_devices: self.include_input_devices != 0,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CspaMixerConfigRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub max_input_slots: u32,
    pub output_queue_slots: u32,
    pub clamp_f32: u32,
    pub silence_on_idle: u32,
    pub on_input_failure: u32,
    pub drift_crossfade_frames: u32,
    pub alignment_window_ns: u64,
    pub max_input_buffer_ns: u64,
    pub drift_threshold_ns: u64,
    pub drift_hysteresis_ns: u64,
    pub drift_hold_ns: u64,
}

impl Default for CspaMixerConfigRecord {
    fn default() -> Self {
        Self::from(MixerConfig::default())
    }
}

impl From<MixerConfig> for CspaMixerConfigRecord {
    fn from(cfg: MixerConfig) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            max_input_slots: cfg.max_input_slots as u32,
            output_queue_slots: cfg.output_queue_slots as u32,
            clamp_f32: u32::from(cfg.clamp_f32),
            silence_on_idle: u32::from(cfg.silence_on_idle),
            on_input_failure: match cfg.on_input_failure {
                OnInputFailure::Degrade => CSPA_ON_INPUT_FAILURE_DEGRADE,
                OnInputFailure::Fail => CSPA_ON_INPUT_FAILURE_FAIL,
            },
            drift_crossfade_frames: cfg.drift_crossfade_frames as u32,
            alignment_window_ns: cfg.alignment_window_ns,
            max_input_buffer_ns: cfg.max_input_buffer_ns,
            drift_threshold_ns: cfg.drift_threshold_ns,
            drift_hysteresis_ns: cfg.drift_hysteresis_ns,
            drift_hold_ns: cfg.drift_hold_ns,
        }
    }
}

impl CspaMixerConfigRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn decode(&self) -> Result<MixerConfig, u32> {
        self.check_abi()?;
        if self.clamp_f32 > 1 || self.silence_on_idle > 1 {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let on_input_failure = match self.on_input_failure {
            CSPA_ON_INPUT_FAILURE_DEGRADE => OnInputFailure::Degrade,
            CSPA_ON_INPUT_FAILURE_FAIL => OnInputFailure::Fail,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let cfg = MixerConfig {
            alignment_window_ns: self.alignment_window_ns,
            max_input_buffer_ns: self.max_input_buffer_ns,
            max_input_slots: self.max_input_slots as usize,
            output_queue_slots: self.output_queue_slots as usize,
            clamp_f32: self.clamp_f32 != 0,
            drift_threshold_ns: self.drift_threshold_ns,
            drift_hysteresis_ns: self.drift_hysteresis_ns,
            drift_hold_ns: self.drift_hold_ns,
            drift_crossfade_frames: self.drift_crossfade_frames as usize,
            silence_on_idle: self.silence_on_idle != 0,
            on_input_failure,
        };
        cfg.validate().map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
        Ok(cfg)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CspaOutputConverterRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub rate_hz: u32,
    pub channels: u32,
    pub sample_format: u32,
    pub byte_order: u32,
    pub dither: u32,
    pub reserved: u32,
    pub seed: u64,
}

impl CspaOutputConverterRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn decode(&self) -> Result<OutputConverterConfig, u32> {
        self.check_abi()?;
        if self.reserved != 0
            || self.rate_hz == 0
            || self.channels == 0
            || self.channels > u16::MAX as u32
        {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let sample = decode_sample_type(self.sample_format)?;
        let order = decode_byte_order(self.byte_order)?;
        let dither = match self.dither {
            CSPA_DITHER_TPDF => DitherMode::Tpdf,
            CSPA_DITHER_NONE => DitherMode::None,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let layout = match self.channels {
            1 => Layout::Known(Arc::from([Channel::Mono])),
            2 => Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            _ => Layout::Unknown,
        };
        let fmt = Format::new(sample, order, self.rate_hz, self.channels as u16, layout)
            .map_err(|_| CSPA_STATUS_INVALID_ARGUMENT)?;
        let mut cfg = OutputConverterConfig::new(fmt, dither);
        if self.seed != 0 {
            cfg = cfg.with_seed(self.seed);
        }
        Ok(cfg)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AggregateBudgetsRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub max_targets: u32,
    pub max_active_streams: u32,
    pub max_pending_opens: u32,
    pub max_native_graph_links: u32,
    pub max_discovery_queue: u32,
    pub reserved: u32,
    pub max_total_pcm_memory: u64,
    pub cleanup_timeout_ns: u64,
}

impl Default for AggregateBudgetsRecord {
    fn default() -> Self {
        Self::from(AggregateManagerBudgets::default())
    }
}

impl From<AggregateManagerBudgets> for AggregateBudgetsRecord {
    fn from(budgets: AggregateManagerBudgets) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            max_targets: budgets.max_targets as u32,
            max_active_streams: budgets.max_active_streams as u32,
            max_pending_opens: budgets.max_pending_opens as u32,
            max_native_graph_links: budgets.max_native_graph_links as u32,
            max_discovery_queue: budgets.max_discovery_queue as u32,
            reserved: 0,
            max_total_pcm_memory: budgets.max_total_pcm_memory as u64,
            cleanup_timeout_ns: budgets.cleanup_timeout_ns,
        }
    }
}

impl AggregateBudgetsRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn decode(&self) -> Result<AggregateManagerBudgets, u32> {
        self.check_abi()?;
        if self.reserved != 0 {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let budgets = AggregateManagerBudgets {
            max_targets: self.max_targets as usize,
            max_active_streams: self.max_active_streams as usize,
            max_pending_opens: self.max_pending_opens as usize,
            max_native_graph_links: self.max_native_graph_links as usize,
            max_discovery_queue: self.max_discovery_queue as usize,
            max_total_pcm_memory: usize::try_from(self.max_total_pcm_memory)
                .map_err(|_| CSPA_STATUS_NO_MEMORY)?,
            cleanup_timeout_ns: self.cleanup_timeout_ns,
        };
        budgets.validate().map_err(|_| CSPA_STATUS_NO_MEMORY)?;
        Ok(budgets)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CspaCaptureRequestRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub tree_strategy: ProcessTreeRangeRecord,
    pub combine_mode: u32,
    pub input_conversion: u32,
    pub on_format_change: u32,
    pub allow_reroute: u32,
    pub require_precise_timing: u32,
    pub overflow: u32,
    pub permission_policy: u32,
    pub reserved: u32,
    pub targets: *const CaptureTargetRecord,
    pub target_count: u64,
    pub backends: *const BackendsRecord,
    pub converter: *const CspaOutputConverterRecord,
    pub mixer_config: *const CspaMixerConfigRecord,
    pub budgets: *const AggregateBudgetsRecord,
}

impl CspaCaptureRequestRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub unsafe fn decode(&self) -> Result<(CaptureRequest, Overflow), u32> {
        self.check_abi()?;
        if self.targets.is_null()
            || self.target_count == 0
            || self.target_count > 1024
            || self.allow_reroute > 1
            || self.require_precise_timing > 1
            || self.reserved != 0
        {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        let tree_strategy = ProcessTreeRange::try_from(self.tree_strategy)?;
        let combine_mode = match self.combine_mode {
            CSPA_COMBINE_MODE_AUTO => CombineMode::Auto,
            CSPA_COMBINE_MODE_NATIVE_ONLY => CombineMode::NativeOnly,
            CSPA_COMBINE_MODE_CSPA_MIXER => CombineMode::CspaMixer,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let input_conversion = match self.input_conversion {
            CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING => InputConversionPolicy::AutoWhenMixing,
            CSPA_INPUT_CONVERSION_STRICT_NO_RESAMPLE => InputConversionPolicy::StrictNoResample,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let on_format_change = match self.on_format_change {
            CSPA_ON_FORMAT_CHANGE_ADAPT => OnFormatChange::Adapt,
            CSPA_ON_FORMAT_CHANGE_FAIL => OnFormatChange::Fail,
            _ => return Err(CSPA_STATUS_INVALID_ARGUMENT),
        };
        let overflow = decode_overflow(self.overflow)?;
        let permission_policy = decode_permission_policy(self.permission_policy)?;

        let count = self.target_count as usize;
        // SAFETY: `self.targets` was checked non-null and `count` is bounded (`1..=1024`);
        // caller upholds pointer validity for `target_count` contiguous `CaptureTargetRecord`s.
        let raw_targets = unsafe { std::slice::from_raw_parts(self.targets, count) };
        let mut decoded_targets = Vec::with_capacity(count);
        for rec in raw_targets {
            decoded_targets.push(rec.decode()?);
        }

        let resolved_backends = if self.backends.is_null() {
            Backends::default()
        } else {
            // SAFETY: `self.backends` is non-null; caller provides a valid `BackendsRecord`.
            unsafe { (&*self.backends).decode()? }
        };
        let resolved_converter = if self.converter.is_null() {
            None
        } else {
            // SAFETY: `self.converter` is non-null; caller provides a valid `CspaOutputConverterRecord`.
            Some(unsafe { (&*self.converter).decode()? })
        };
        let resolved_mixer = if self.mixer_config.is_null() {
            MixerConfig::default()
        } else {
            // SAFETY: `self.mixer_config` is non-null; caller provides a valid `CspaMixerConfigRecord`.
            unsafe { (&*self.mixer_config).decode()? }
        };
        let resolved_budgets = if self.budgets.is_null() {
            AggregateManagerBudgets::default()
        } else {
            // SAFETY: `self.budgets` is non-null; caller provides a valid `AggregateBudgetsRecord`.
            unsafe { (&*self.budgets).decode()? }
        };

        let request = CaptureRequest::new(decoded_targets)
            .with_tree_strategy(tree_strategy)
            .with_combine_mode(combine_mode)
            .with_input_conversion(input_conversion)
            .with_backends(resolved_backends)
            .with_output_converter(resolved_converter)
            .with_on_format_change(on_format_change)
            .with_mixer_config(resolved_mixer)
            .with_budgets(resolved_budgets)
            .with_allow_reroute(self.allow_reroute != 0)
            .with_require_precise_timing(self.require_precise_timing != 0)
            .with_permission_policy(permission_policy);
        Ok((request, overflow))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CspaPermissionEntryRecord {
    pub kind: u32,
    pub state: u32,
    pub required: u32,
    pub can_prompt: u32,
    pub requires_relaunch: u32,
    pub reserved: u32,
}

impl From<PermissionEntry> for CspaPermissionEntryRecord {
    fn from(entry: PermissionEntry) -> Self {
        Self {
            kind: encode_permission_kind(entry.kind),
            state: encode_permission_state(entry.state),
            required: u32::from(entry.required),
            can_prompt: u32::from(entry.can_prompt),
            requires_relaunch: u32::from(entry.requires_relaunch),
            reserved: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CspaPermissionReportRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub platform_os: u32,
    pub is_granted: u32,
    pub has_denial: u32,
    pub requires_relaunch: u32,
    pub entry_count: u32,
    pub reserved: u32,
    pub entries: [CspaPermissionEntryRecord; CSPA_PERMISSION_KIND_COUNT],
}

impl Default for CspaPermissionReportRecord {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            platform_os: 0,
            is_granted: 0,
            has_denial: 0,
            requires_relaunch: 0,
            entry_count: 0,
            reserved: 0,
            entries: [CspaPermissionEntryRecord::default(); CSPA_PERMISSION_KIND_COUNT],
        }
    }
}

impl CspaPermissionReportRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub fn populate_from(&mut self, report: &PermissionReport) {
        self.platform_os = encode_platform_os(report.os);
        self.is_granted = u32::from(report.is_granted());
        self.has_denial = u32::from(report.has_denial());
        self.requires_relaunch = u32::from(report.requires_relaunch());
        self.entry_count = CSPA_PERMISSION_KIND_COUNT as u32;
        self.reserved = 0;
        for (slot, entry) in self.entries.iter_mut().zip(report.entries().iter()) {
            *slot = CspaPermissionEntryRecord::from(*entry);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CspaTargetPlanRecord {
    pub target_index: u32,
    pub resolved_backend: u32,
    pub acquisition_kind: u32,
    pub tree_strategy: u32,
    pub attempt_count: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CspaPlanReportRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub platform_os: u32,
    pub selected_backend: u32,
    pub combine_mode: u32,
    pub combine_resolution: u32,
    pub input_conversion: u32,
    pub uses_software_mixer: u32,
    pub uses_output_converter: u32,
    pub requires_discovery_subscription: u32,
    pub overlap_semantics: u32,
    pub allow_reroute: u32,
    pub permission_policy: u32,
    pub reserved: u32,
    pub tree_strategy: ProcessTreeRangeRecord,
    pub target_plan_count: u64,
    pub duplicate_target_count: u64,
    pub estimated_active_streams: u64,
    pub estimated_pcm_bytes: u64,
    pub target_plans: *mut CspaTargetPlanRecord,
    pub target_plans_capacity: u64,
}

impl CspaPlanReportRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }

    pub unsafe fn populate_from(&mut self, report: &CapturePlanReport) {
        self.platform_os = encode_platform_os(report.os);
        self.selected_backend = encode_backend_opt(report.selected_backend);
        self.combine_mode = match report.combine_mode {
            CombineMode::Auto => CSPA_COMBINE_MODE_AUTO,
            CombineMode::NativeOnly => CSPA_COMBINE_MODE_NATIVE_ONLY,
            CombineMode::CspaMixer => CSPA_COMBINE_MODE_CSPA_MIXER,
        };
        self.combine_resolution = match report.combine_resolution {
            CombineResolution::PassThrough(_) => CSPA_COMBINE_RESOLUTION_PASS_THROUGH,
            CombineResolution::SharedNativeGraphMix(_) => {
                CSPA_COMBINE_RESOLUTION_SHARED_NATIVE_GRAPH_MIX
            }
            CombineResolution::CspaMixer => CSPA_COMBINE_RESOLUTION_CSPA_MIXER,
        };
        self.input_conversion = match report.input_conversion {
            InputConversionPolicy::AutoWhenMixing => CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING,
            InputConversionPolicy::StrictNoResample => CSPA_INPUT_CONVERSION_STRICT_NO_RESAMPLE,
        };
        self.uses_software_mixer = u32::from(report.uses_software_mixer);
        self.uses_output_converter = u32::from(report.uses_output_converter);
        self.requires_discovery_subscription = u32::from(report.requires_discovery_subscription);
        self.overlap_semantics = match report.overlap_semantics {
            OverlapCombinationSemantics::SingleTarget => CSPA_OVERLAP_SINGLE_TARGET,
            OverlapCombinationSemantics::SoftwareSummation => CSPA_OVERLAP_SOFTWARE_SUMMATION,
            OverlapCombinationSemantics::NativeSetUnion => CSPA_OVERLAP_NATIVE_SET_UNION,
        };
        self.allow_reroute = u32::from(report.allow_reroute);
        self.permission_policy = encode_permission_policy(report.permission_policy);
        self.reserved = 0;
        self.tree_strategy = ProcessTreeRangeRecord::from(report.tree_strategy);
        self.target_plan_count = report.target_plans.len() as u64;
        self.duplicate_target_count = report.duplicate_target_ids.len() as u64;
        self.estimated_active_streams = report.estimated_active_streams as u64;
        self.estimated_pcm_bytes = report.estimated_pcm_bytes as u64;

        if !self.target_plans.is_null() && self.target_plans_capacity > 0 {
            let write_len = (self.target_plans_capacity as usize).min(report.target_plans.len());
            // SAFETY: `self.target_plans` is non-null and caller declared `target_plans_capacity` slots.
            let dst = unsafe { std::slice::from_raw_parts_mut(self.target_plans, write_len) };
            for (slot, plan) in dst.iter_mut().zip(report.target_plans.iter()) {
                let (acq_kind, tree_code) = match plan.acquisition {
                    TargetAcquisition::DirectEndpoint => {
                        (crate::CSPA_ACQUISITION_DIRECT_ENDPOINT, 0)
                    }
                    TargetAcquisition::ProcessTree(s) => {
                        (crate::CSPA_ACQUISITION_PROCESS_TREE, s as u32)
                    }
                };
                *slot = CspaTargetPlanRecord {
                    target_index: plan.target_index as u32,
                    resolved_backend: encode_backend(plan.resolved_backend),
                    acquisition_kind: acq_kind,
                    tree_strategy: tree_code,
                    attempt_count: plan.attempts.len() as u32,
                    reserved: 0,
                };
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CspaDiscoveryEventRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub kind: u32,
    pub visibility: u32,
    pub revision: u64,
    pub minimum_revision: u64,
    pub subject: [u8; CSPA_EVENT_SUBJECT_MAX_BYTES],
    pub target: CaptureTargetRecord,
}

impl CspaDiscoveryEventRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CspaInputStatusRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub target_index: u32,
    pub backend: u32,
    pub participation_state: u32,
    pub failure_code: u32,
    pub native_rate_hz: u32,
    pub native_channels: u32,
    pub native_sample_format: u32,
    pub mixer_rate_hz: u32,
    pub mixer_channels: u32,
    pub is_resampled: u32,
    pub is_identity_bypass: u32,
    pub reserved: u32,
    pub queued_packets: u64,
    pub dropped_frames: u64,
    pub clipped_samples: u64,
    pub nonfinite_samples: u64,
    pub drift_slips: u64,
}

impl CspaInputStatusRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CspaHeaderRecord {
    pub sequence: u64,
    pub segment: u64,
    pub generation: u64,
    pub frames: u64,
    pub sample_position: u64,
    pub ticks: u64,
    pub ticks_per_second: u64,
    pub clock_domain: u64,
    pub uncertainty_ns: u64,
    pub valid: u32,
    pub provenance: u32,
}

impl CspaHeaderRecord {
    #[inline]
    pub fn from_header(hdr: Header, actual_frames: usize) -> Self {
        let mut valid = u32::from(hdr.sample_position.is_some());
        let mut ticks = 0;
        let mut ticks_per_second = 0;
        let mut clock_domain = 0;
        let mut provenance = 0;
        let mut uncertainty_ns = 0;
        if let Some(ts) = hdr.timestamp {
            valid |= 2;
            ticks = ts.ticks;
            ticks_per_second = ts.ticks_per_second;
            clock_domain = ts.clock_domain;
            provenance = match ts.provenance {
                Provenance::Native => 1,
                Provenance::Derived => 2,
                Provenance::HostArrival => 3,
                Provenance::Synthetic => 4,
            };
            if let Some(unc) = ts.uncertainty_ns {
                valid |= 4;
                uncertainty_ns = unc;
            }
        } else {
            valid |= 1;
        }
        Self {
            sequence: hdr.sequence,
            segment: hdr.segment,
            generation: hdr.generation,
            frames: actual_frames as u64,
            sample_position: hdr.sample_position.unwrap_or(0),
            ticks,
            ticks_per_second,
            clock_domain,
            uncertainty_ns,
            valid,
            provenance,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CspaBlockRecord {
    pub header: CspaHeaderRecord,
    pub bytes: *const u8,
    pub byte_length: u64,
    pub retained_owner: *mut CspaOwnerHandle,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixedBlockMetadataRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub synthesized_silence: u32,
    pub skew_valid: u32,
    pub membership_version: u64,
    pub contributing_mask: u64,
    pub late_inputs: u64,
    pub failed_inputs: u64,
    pub nonfinite_inputs: u64,
    pub clipped_samples: u64,
    pub nonfinite_samples: u64,
    pub drift_slips: u64,
    pub skew_estimate_ns: u64,
    pub skew_uncertainty_ns: u64,
}

impl From<MixedBlockMetadata> for MixedBlockMetadataRecord {
    fn from(meta: MixedBlockMetadata) -> Self {
        let mut skew_valid = 0u32;
        if meta.skew_estimate_ns.is_some() {
            skew_valid |= 1;
        }
        if meta.skew_uncertainty_ns.is_some() {
            skew_valid |= 2;
        }
        Self {
            struct_size: size_of::<Self>() as u32,
            schema_revision: CSPA_EXPERIMENTAL_SCHEMA_REVISION,
            synthesized_silence: u32::from(meta.synthesized_silence),
            skew_valid,
            membership_version: meta.membership_version,
            contributing_mask: meta.contributing_mask,
            late_inputs: meta.late_inputs,
            failed_inputs: meta.failed_inputs,
            nonfinite_inputs: meta.nonfinite_inputs,
            clipped_samples: meta.clipped_samples,
            nonfinite_samples: meta.nonfinite_samples,
            drift_slips: meta.drift_slips,
            skew_estimate_ns: meta.skew_estimate_ns.unwrap_or(0),
            skew_uncertainty_ns: meta.skew_uncertainty_ns.unwrap_or(0),
        }
    }
}

impl TryFrom<MixedBlockMetadataRecord> for MixedBlockMetadata {
    type Error = u32;
    fn try_from(record: MixedBlockMetadataRecord) -> Result<Self, u32> {
        if record.struct_size as usize != size_of::<MixedBlockMetadataRecord>()
            || record.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        if record.synthesized_silence > 1
            || record.skew_valid & !3 != 0
            || (record.skew_valid & 2 != 0 && record.skew_valid & 1 == 0)
        {
            return Err(CSPA_STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            membership_version: record.membership_version,
            contributing_mask: record.contributing_mask,
            late_inputs: record.late_inputs,
            failed_inputs: record.failed_inputs,
            nonfinite_inputs: record.nonfinite_inputs,
            synthesized_silence: record.synthesized_silence != 0,
            clipped_samples: record.clipped_samples,
            nonfinite_samples: record.nonfinite_samples,
            drift_slips: record.drift_slips,
            skew_estimate_ns: (record.skew_valid & 1 != 0).then_some(record.skew_estimate_ns),
            skew_uncertainty_ns: (record.skew_valid & 2 != 0).then_some(record.skew_uncertainty_ns),
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CspaManagedReadOutcomeRecord {
    pub struct_size: u32,
    pub schema_revision: u32,
    pub partial_reason: u32,
    pub boundary_kind: u32,
    pub has_mixed_metadata: u32,
    pub reserved: u32,
    pub session: u64,
    pub segment: u64,
    pub actual_frames: u64,
    pub copy_bytes: u64,
    pub copy_destinations: u64,
    pub block: CspaBlockRecord,
    pub mixed_metadata: MixedBlockMetadataRecord,
}

impl CspaManagedReadOutcomeRecord {
    pub fn check_abi(&self) -> Result<(), u32> {
        if self.struct_size as usize != size_of::<Self>()
            || self.schema_revision != CSPA_EXPERIMENTAL_SCHEMA_REVISION
        {
            return Err(CSPA_STATUS_ABI_MISMATCH);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn records_sizes_offsets_roundtrips_and_abi_mismatch_rejections() {
        assert_eq!(size_of::<ProcessIdentityRecord>(), 16);
        assert_eq!(size_of::<ProcessTreeRangeRecord>(), 8);
        assert_eq!(size_of::<BackendsRecord>(), 24);
        assert_eq!(size_of::<CspaPcmFormatRecord>(), 24);
        assert_eq!(size_of::<CaptureTargetRecord>(), 832);
        assert_eq!(offset_of!(CaptureTargetRecord, process_start_time), 48);
        assert_eq!(offset_of!(CaptureTargetRecord, device_id), 64);
        assert_eq!(offset_of!(CaptureTargetRecord, app_id), 320);
        assert_eq!(offset_of!(CaptureTargetRecord, name), 448);
        assert_eq!(offset_of!(CaptureTargetRecord, executable_path), 576);
        assert_eq!(size_of::<CspaDiscoveryQueryRecord>(), 72);
        assert_eq!(size_of::<CspaMixerConfigRecord>(), 72);
        assert_eq!(size_of::<CspaOutputConverterRecord>(), 40);
        assert_eq!(size_of::<AggregateBudgetsRecord>(), 48);
        assert_eq!(size_of::<CspaCaptureRequestRecord>(), 96);
        assert_eq!(offset_of!(CspaCaptureRequestRecord, permission_policy), 40);
        assert_eq!(offset_of!(CspaCaptureRequestRecord, targets), 48);
        assert_eq!(size_of::<CspaPermissionEntryRecord>(), 24);
        assert_eq!(size_of::<CspaPermissionReportRecord>(), 104);
        assert_eq!(offset_of!(CspaPermissionReportRecord, entries), 32);
        assert_eq!(size_of::<CspaTargetPlanRecord>(), 24);
        assert_eq!(size_of::<CspaPlanReportRecord>(), 112);
        assert_eq!(offset_of!(CspaPlanReportRecord, permission_policy), 48);
        assert_eq!(size_of::<CspaDiscoveryEventRecord>(), 1120);
        assert_eq!(size_of::<CspaInputStatusRecord>(), 96);
        assert_eq!(size_of::<CspaHeaderRecord>(), 80);
        assert_eq!(size_of::<CspaBlockRecord>(), 104);
        assert_eq!(size_of::<MixedBlockMetadataRecord>(), 96);
        assert_eq!(size_of::<CspaManagedReadOutcomeRecord>(), 264);

        // Schema revision 0 must reject with ABI_MISMATCH
        let mut stale_target =
            CaptureTargetRecord::from(&CaptureTarget::microphone(DefaultDeviceMode::Follow));
        stale_target.schema_revision = 0;
        assert_eq!(stale_target.decode(), Err(CSPA_STATUS_ABI_MISMATCH));

        // Full target roundtrip including input_device + backend pin + available_backends + gain
        let in_dev = CaptureTarget::input_device("alsa_input.usb-mic")
            .unwrap()
            .with_name(Some("USB Studio Mic".into()))
            .with_available_backends([BackendId::PipeWire, BackendId::PulseAudio])
            .with_backend(BackendId::PulseAudio)
            .with_gain(0.75)
            .unwrap();
        let rec = CaptureTargetRecord::from(&in_dev);
        let decoded = rec.decode().unwrap();
        assert_eq!(decoded, in_dev);

        // Application target roundtrip
        let app = CaptureTarget::application(
            ProcessIdentity {
                pid: 4242,
                generation: 99_000,
            },
            Some("com.example.studio".into()),
            Some("StudioApp".into()),
            Some("/Applications/StudioApp.app/Contents/MacOS/StudioApp".into()),
            ProcessScope::Exact,
            OnRootExit::Wait {
                wait_timeout_ns: 2_500_000_000,
            },
        )
        .unwrap()
        .with_available_backends([BackendId::ScreenCaptureKit, BackendId::CoreAudio])
        .with_backend(BackendId::ScreenCaptureKit);
        assert_eq!(CaptureTargetRecord::from(&app).decode().unwrap(), app);

        // DiscoveryQuery, MixerConfig, Backends, Budgets, MixedBlockMetadata roundtrips
        let q = CspaDiscoveryQueryRecord::default();
        let dq = q.decode_for_os(PlatformOs::Linux).unwrap();
        assert_eq!(dq.dedupe, DiscoveryDedupe::ByLogicalTarget);
        assert_eq!(dq.order, DiscoveryOrder::KindThenBackend);

        let mcfg = MixerConfig::default();
        assert_eq!(CspaMixerConfigRecord::from(mcfg).decode(), Ok(mcfg));
        let bk = Backends::default();
        assert_eq!(BackendsRecord::from(&bk).decode(), Ok(bk));
        let bud = AggregateManagerBudgets::default();
        assert_eq!(AggregateBudgetsRecord::from(bud).decode(), Ok(bud));
        let mmeta = MixedBlockMetadata {
            membership_version: 2,
            contributing_mask: 0b11,
            late_inputs: 0,
            failed_inputs: 0,
            nonfinite_inputs: 1,
            synthesized_silence: true,
            clipped_samples: 4,
            nonfinite_samples: 2,
            drift_slips: 1,
            skew_estimate_ns: Some(1_000_000),
            skew_uncertainty_ns: Some(250_000),
        };
        assert_eq!(
            MixedBlockMetadata::try_from(MixedBlockMetadataRecord::from(mmeta)),
            Ok(mmeta)
        );
    }
}
