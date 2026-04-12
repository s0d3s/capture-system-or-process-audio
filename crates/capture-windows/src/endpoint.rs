use crate::{
    capabilities::{check_capture_target, map_windows_error},
    core::{
        StorageLimits,
        manager::{StorageBudget, StorageReservation},
        measurements::Trace,
        ordering::{self, ControlHandle, Failure, Offer, Overflow, Stop},
        reads::{Cancellation, ReadError, ReadResult, ReadStream},
        schema::{
            BackendId, Backends, ByteOrder, CaptureTarget, Channel, Format, Layout,
            PermissionPolicy, ProcessIdentity, ProcessScope, ProcessTreeRange, SampleType,
            TargetKind,
        },
        storage::Pool,
        stream_runtime::{
            EndReason, ReadySignal, StartupCleanup, StreamReadContext, endpoint_clock_domain,
            poll_read_stream, poll_read_stream_into,
        },
        traits::{AudioStreamBackend, CapabilityError, OpenableStreamBackend, TimingRequirement},
    },
    packets::{Packet, Packets},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering::SeqCst},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::Audio::*,
        System::{
            Com::*,
            Threading::{
                AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
                SetEvent, WaitForMultipleObjects, WaitForSingleObject,
            },
        },
    },
    core::{GUID, PCWSTR},
};

struct Event(usize);
impl Event {
    fn new(manual: bool) -> Result<Self, CapabilityError> {
        // SAFETY: creates an unnamed owned event without borrowing security attributes.
        Ok(Self(
            unsafe { CreateEventW(None, manual, false, None).map_err(map_windows_error)? }.0
                as usize,
        ))
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut _)
    }
    fn signal(&self) -> Result<(), CapabilityError> {
        // SAFETY: the event remains owned by this object for the call.
        unsafe {
            SetEvent(self.handle()).map_err(map_windows_error)?;
        }
        Ok(())
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: sole handle owner; Arc borrows have ended before destruction.
        unsafe {
            let _ = CloseHandle(self.handle());
        }
    }
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, CapabilityError> {
        // SAFETY: constructed and destroyed inside the same dedicated worker.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(map_windows_error)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances successful initialization after all COM owners are dropped.
        unsafe {
            CoUninitialize();
        }
    }
}

struct CaptureTask {
    handle: HANDLE,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl CaptureTask {
    fn new() -> Result<Self, CapabilityError> {
        let mut index = 0;
        // SAFETY: valid task name and output index; guard remains on this thread.
        let handle = unsafe {
            AvSetMmThreadCharacteristicsW(windows::core::w!("Capture"), &mut index)
                .map_err(map_windows_error)?
        };
        Ok(Self {
            handle,
            _thread_bound: std::marker::PhantomData,
        })
    }
}

impl Drop for CaptureTask {
    fn drop(&mut self) {
        // SAFETY: thread-bound guard uniquely owns this thread's MMCSS registration.
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.handle);
        }
    }
}

struct Mix(*mut WAVEFORMATEX);
impl Drop for Mix {
    fn drop(&mut self) {
        // SAFETY: GetMixFormat transfers this allocation to the caller.
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WasapiOpenTarget {
    DefaultRender,
    DefaultCapture,
    RenderEndpoint(Arc<str>),
    CaptureEndpoint(Arc<str>),
    ProcessTree(ProcessIdentity),
}

impl WasapiOpenTarget {
    pub(crate) fn from_capture_target(target: &CaptureTarget) -> Result<Self, CapabilityError> {
        target
            .validate()
            .map_err(|_| CapabilityError::InvalidTarget)?;
        if let Some(pinned) = target.backend()
            && pinned != BackendId::Wasapi
        {
            return Err(CapabilityError::InvalidBackend);
        }
        match target.kind() {
            TargetKind::DefaultOutput => Ok(Self::DefaultRender),
            TargetKind::DefaultInput => Ok(Self::DefaultCapture),
            TargetKind::Device => {
                let id = target
                    .clean_device_id()
                    .ok_or(CapabilityError::InvalidTarget)?;
                if id.is_empty() || id.contains('\0') {
                    return Err(CapabilityError::InvalidTarget);
                }
                Ok(Self::RenderEndpoint(id.into()))
            }
            TargetKind::InputDevice => {
                let id = target
                    .clean_device_id()
                    .ok_or(CapabilityError::InvalidTarget)?;
                if id.is_empty() || id.contains('\0') {
                    return Err(CapabilityError::InvalidTarget);
                }
                Ok(Self::CaptureEndpoint(id.into()))
            }
            TargetKind::Process | TargetKind::Application => {
                if target.scope() == ProcessScope::Exact {
                    return Err(CapabilityError::UnsupportedScope);
                }
                let identity = target
                    .process_identity()
                    .ok_or(CapabilityError::InvalidTarget)?;
                Ok(Self::ProcessTree(identity))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeFormat {
    pub target: CaptureTarget,
    pub native_mix_known: bool,
    pub endpoint: Arc<str>,
    pub pcm: Arc<Format>,
    pub buffer_frames: u32,
    pub channel_mask: Option<u32>,
}

struct Native {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: Event,
    device_positions: bool,
    clock_domain: u64,
    process: Option<crate::process::ProcessLease>,
}
impl Drop for Native {
    fn drop(&mut self) {
        // SAFETY: same worker as activation; no outstanding acquired packet.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

fn open(
    open_target: WasapiOpenTarget,
    display_target: CaptureTarget,
) -> Result<(Native, NativeFormat), CapabilityError> {
    if let WasapiOpenTarget::ProcessTree(root) = open_target {
        return open_process(root, display_target);
    }
    // SAFETY: caller owns initialized worker apartment. Allocations are freed by
    // their owners, and GetMixFormat describes the initialized format buffer.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(map_windows_error)?;
        let (device, is_capture_endpoint) = match open_target {
            WasapiOpenTarget::DefaultRender => (
                enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .map_err(map_windows_error)?,
                false,
            ),
            WasapiOpenTarget::DefaultCapture => (
                enumerator
                    .GetDefaultAudioEndpoint(eCapture, eConsole)
                    .map_err(map_windows_error)?,
                true,
            ),
            WasapiOpenTarget::RenderEndpoint(identity) => {
                if identity.is_empty() || identity.contains('\0') {
                    return Err(CapabilityError::InvalidTarget);
                }
                let identity_w: Vec<u16> = identity.encode_utf16().chain(Some(0)).collect();
                let dev = enumerator
                    .GetDevice(PCWSTR(identity_w.as_ptr()))
                    .map_err(map_windows_error)?;
                let flow = windows::core::Interface::cast::<IMMEndpoint>(&dev)
                    .map_err(map_windows_error)?
                    .GetDataFlow()
                    .map_err(map_windows_error)?;
                if flow != eRender {
                    return Err(CapabilityError::InvalidTarget);
                }
                (dev, false)
            }
            WasapiOpenTarget::CaptureEndpoint(identity) => {
                if identity.is_empty() || identity.contains('\0') {
                    return Err(CapabilityError::InvalidTarget);
                }
                let identity_w: Vec<u16> = identity.encode_utf16().chain(Some(0)).collect();
                let dev = enumerator
                    .GetDevice(PCWSTR(identity_w.as_ptr()))
                    .map_err(map_windows_error)?;
                let flow = windows::core::Interface::cast::<IMMEndpoint>(&dev)
                    .map_err(map_windows_error)?
                    .GetDataFlow()
                    .map_err(map_windows_error)?;
                if flow != eCapture {
                    return Err(CapabilityError::InvalidTarget);
                }
                (dev, true)
            }
            WasapiOpenTarget::ProcessTree(_) => unreachable!(),
        };
        let identifier = device.GetId().map_err(map_windows_error)?;
        let endpoint_res = identifier.to_string();
        CoTaskMemFree(Some(identifier.0.cast()));
        let endpoint: Arc<str> = endpoint_res
            .map_err(|_| CapabilityError::DeviceUnavailable)?
            .into();
        let event = Event::new(false)?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(map_windows_error)?;
        let mix = Mix(client.GetMixFormat().map_err(map_windows_error)?);
        if mix.0.is_null() {
            return Err(CapabilityError::UnsupportedFormat);
        }
        let wave = mix.0.read_unaligned();
        let mut tag = wave.wFormatTag;
        let mut channel_mask = None;
        if tag == 0xfffe {
            if wave.cbSize < 22 {
                return Err(CapabilityError::UnsupportedFormat);
            }
            let extended = mix.0.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned();
            let subtype = extended.SubFormat;
            tag = if subtype == GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71) {
                3
            } else if subtype == GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71) {
                1
            } else {
                return Err(CapabilityError::UnsupportedFormat);
            };
            let valid_bits = extended.Samples.wValidBitsPerSample;
            let allow_24_in_32 = tag == 1 && wave.wBitsPerSample == 32 && valid_bits == 24;
            if valid_bits != wave.wBitsPerSample && !allow_24_in_32 {
                return Err(CapabilityError::UnsupportedFormat);
            }
            channel_mask = Some(extended.dwChannelMask);
        }
        let sample = match (tag, wave.wBitsPerSample) {
            (3, 32) => SampleType::F32,
            (1, 16) => SampleType::I16,
            (1, 24) => SampleType::I24,
            (1, 32) => SampleType::I32,
            _ => return Err(CapabilityError::UnsupportedFormat),
        };
        let layout = match (wave.nChannels, channel_mask) {
            (1, None | Some(0) | Some(0x4)) => Layout::Known(Arc::from([Channel::Mono])),
            (2, None | Some(0) | Some(0x3)) => {
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight]))
            }
            (4, None | Some(0) | Some(0x33) | Some(0x603)) => Layout::Known(Arc::from([
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::RearLeft,
                Channel::RearRight,
            ])),
            (6, None | Some(0) | Some(0x3f) | Some(0x60f)) => Layout::Known(Arc::from([
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::Center,
                Channel::LowFrequency,
                Channel::RearLeft,
                Channel::RearRight,
            ])),
            _ => Layout::Unknown,
        };
        let format = Arc::new(
            Format::new(
                sample,
                ByteOrder::Little,
                wave.nSamplesPerSec,
                wave.nChannels,
                layout,
            )
            .map_err(|_| CapabilityError::UnsupportedFormat)?,
        );
        if format.bytes_per_frame() != wave.nBlockAlign as usize
            || wave.nAvgBytesPerSec as u64 != wave.nSamplesPerSec as u64 * wave.nBlockAlign as u64
        {
            return Err(CapabilityError::UnsupportedFormat);
        }
        let stream_flags = if is_capture_endpoint {
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        } else {
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        };
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, stream_flags, 0, 0, mix.0, None)
            .map_err(map_windows_error)?;
        client
            .SetEventHandle(event.handle())
            .map_err(map_windows_error)?;
        let buffer_frames = client.GetBufferSize().map_err(map_windows_error)?;
        let capture = client.GetService().map_err(map_windows_error)?;
        let clock_domain = if is_capture_endpoint {
            endpoint_clock_domain(&endpoint, 2)
        } else {
            1
        };
        Ok((
            Native {
                capture,
                client,
                event,
                device_positions: true,
                clock_domain,
                process: None,
            },
            NativeFormat {
                target: display_target,
                native_mix_known: true,
                endpoint,
                pcm: format,
                buffer_frames,
                channel_mask,
            },
        ))
    }
}

fn open_process(
    root: ProcessIdentity,
    display_target: CaptureTarget,
) -> Result<(Native, NativeFormat), CapabilityError> {
    let (client, lease) = crate::process::activate(root)?;
    let event = Event::new(false)?;
    let wave = WAVEFORMATEX {
        wFormatTag: 3,
        nChannels: 2,
        nSamplesPerSec: 48000,
        nAvgBytesPerSec: 384000,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    // SAFETY: activated client and format are owned by this worker; no capture buffer is outstanding.
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                0,
                0,
                &wave,
                None,
            )
            .map_err(map_windows_error)?;
        client
            .SetEventHandle(event.handle())
            .map_err(map_windows_error)?;
        let buffer_frames = client.GetBufferSize().map_err(map_windows_error)?;
        let capture = client.GetService().map_err(map_windows_error)?;
        let pcm = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .map_err(|_| CapabilityError::UnsupportedFormat)?,
        );
        Ok((
            Native {
                capture,
                client,
                event,
                device_positions: false,
                clock_domain: 1,
                process: Some(lease),
            },
            NativeFormat {
                target: display_target,
                native_mix_known: false,
                endpoint: "VAD\\Process_Loopback".into(),
                pcm,
                buffer_frames,
                channel_mask: None,
            },
        ))
    }
}

#[cfg(any(test, feature = "native-tests"))]
pub struct RawProcessCapture {
    native: Native,
    _apartment: Apartment,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
    pub samples: Vec<f32>,
    pub packets: Vec<(usize, u32, u32, u64)>,
}

#[cfg(any(test, feature = "native-tests"))]
impl RawProcessCapture {
    pub fn open(
        root: ProcessIdentity,
    ) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let apartment = Apartment::new()?;
        let disp = CaptureTarget::process(
            root,
            ProcessScope::Tree,
            crate::core::schema::OnRootExit::Stop,
        )?;
        let (native, _) = open_process(root, disp)?;
        let result = Self {
            native,
            _apartment: apartment,
            _thread_bound: std::marker::PhantomData,
            samples: Vec::with_capacity(1_440_000),
            packets: Vec::with_capacity(2048),
        };
        // SAFETY: this thread owns the initialized client and its COM apartment.
        unsafe { result.native.client.Start()? };
        Ok(result)
    }

    pub fn collect(&mut self) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // SAFETY: sole same-thread consumer; native PCM is copied before every
        // acquired buffer is released, including capacity and pointer failures.
        unsafe {
            for _turn in 0..32 {
                if self.native.capture.GetNextPacketSize()? == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                let mut qpc = 0;
                self.native.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    None,
                    Some(&mut qpc),
                )?;
                let outcome =
                    (|| -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
                        let count = frames as usize * 2;
                        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                        if frames == 0
                            || frames > 4096
                            || self.samples.len() + count > self.samples.capacity()
                            || self.packets.len() == self.packets.capacity()
                            || (!silent && data.is_null())
                        {
                            return Err("invalid or excessive raw comparison packet".into());
                        }
                        self.packets
                            .push((self.samples.len() / 2, frames, flags, qpc));
                        if silent {
                            self.samples.resize(self.samples.len() + count, 0.0);
                        } else {
                            self.samples.extend_from_slice(std::slice::from_raw_parts(
                                data.cast::<f32>(),
                                count,
                            ));
                        }
                        Ok(())
                    })();
                let released = self.native.capture.ReleaseBuffer(frames);
                outcome?;
                released?;
            }
        }
        Ok(())
    }

    pub fn stop(&self) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // SAFETY: same-thread client owner; collect leaves no acquired buffer.
        unsafe { self.native.client.Stop()? };
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum DrainState {
    Consumed,
    Empty,
    Terminal,
}

fn drain_turn(
    mut consume: impl FnMut() -> Result<DrainState, CapabilityError>,
) -> Result<DrainState, CapabilityError> {
    for _turn in 0..32 {
        match consume()? {
            DrainState::Consumed => (),
            outcome => return Ok(outcome),
        }
    }
    Ok(DrainState::Consumed)
}

fn acquire(
    native: &Native,
    producer: &mut ordering::Producer,
    stop: &Event,
    stop_flag: &AtomicBool,
    ready: &ReadySignal,
    policy: Overflow,
    ended: &AtomicU32,
) -> Result<(), CapabilityError> {
    let mut packets = Packets::with_clock_domain(native.clock_domain, native.device_positions);
    let stride = producer.pool().format().bytes_per_frame();
    let handles = [
        stop.handle(),
        native.event.handle(),
        native
            .process
            .as_ref()
            .map_or(HANDLE::default(), |process| process.handle()),
    ];
    let handles = &handles[..if native.process.is_some() { 3 } else { 2 }];
    let mut pending = false;
    // SAFETY: client and events live through this worker loop. Every successful
    // GetBuffer is paired with ReleaseBuffer before inspecting errors or waiting.
    unsafe {
        while !producer.control_handle().stopping() && !stop_flag.load(SeqCst) {
            if root_ended(native.process.as_ref(), producer, ended)? {
                break;
            }
            if !pending {
                let wait = WaitForMultipleObjects(handles, false, 50);
                if wait == WAIT_OBJECT_0 || stop_flag.load(SeqCst) {
                    break;
                }
                if wait.0 == WAIT_OBJECT_0.0 + 2 && native.process.is_some() {
                    root_ended(native.process.as_ref(), producer, ended)?;
                    break;
                }
                if wait == WAIT_TIMEOUT {
                    continue;
                }
                if wait.0 != WAIT_OBJECT_0.0 + 1 {
                    return Err(CapabilityError::BackendFailure {
                        backend: BackendId::Wasapi,
                        code: wait.0 as i32,
                    });
                }
            }
            let outcome = drain_turn(|| {
                if producer.control_handle().stopping()
                    || stop_flag.load(SeqCst)
                    || root_ended(native.process.as_ref(), producer, ended)?
                {
                    return Ok(DrainState::Terminal);
                }
                if native
                    .capture
                    .GetNextPacketSize()
                    .map_err(map_windows_error)?
                    == 0
                {
                    return Ok(DrainState::Empty);
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                let mut position = 0;
                let mut qpc = 0;
                native
                    .capture
                    .GetBuffer(
                        &mut data,
                        &mut frames,
                        &mut flags,
                        Some(&mut position),
                        Some(&mut qpc),
                    )
                    .map_err(map_windows_error)?;
                let acquired = producer.trace().map(|_| Instant::now());
                let count = (frames as usize).checked_mul(stride);
                let too_large = frames as usize > producer.pool().max_frames();
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let invalid = !too_large && (!silent && data.is_null() || count.is_none());
                let outcome = if invalid {
                    let _ = producer.reject_uncertain(u64::MAX - 1, frames as usize);
                    Offer::Failed(Failure::Backend)
                } else {
                    let bytes = if too_large || silent {
                        &[]
                    } else {
                        std::slice::from_raw_parts(data, count.unwrap())
                    };
                    let packet = Packet {
                        bytes,
                        silent,
                        frames: frames as usize,
                        discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
                        timestamp_error: flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0,
                        position,
                        qpc_100ns: qpc,
                    };
                    if acquired.is_some() {
                        packets.publish_observed(producer, packet, policy, acquired)
                    } else {
                        packets.publish(producer, packet, policy)
                    }
                };
                let released = native.capture.ReleaseBuffer(frames);
                ready.notify();
                released.map_err(map_windows_error)?;
                Ok(if matches!(outcome, Offer::Failed(_) | Offer::Stopped) {
                    DrainState::Terminal
                } else {
                    DrainState::Consumed
                })
            })?;
            pending = match outcome {
                DrainState::Consumed => true,
                DrainState::Empty => false,
                DrainState::Terminal => return Ok(()),
            };
        }
    }
    Ok(())
}

pub(crate) fn root_ended(
    process: Option<&crate::process::ProcessLease>,
    producer: &ordering::Producer,
    ended: &AtomicU32,
) -> Result<bool, CapabilityError> {
    if let Some(process) = process
        && process.exited()?
    {
        let _ = ended.compare_exchange(0, 2, SeqCst, SeqCst);
        producer.control_handle().stop(Stop::Drain);
        return Ok(true);
    }
    Ok(false)
}

pub struct PlatformStream {
    pub(crate) native_format: NativeFormat,
    trace: Option<Arc<Trace>>,
    reads: ReadStream,
    control: ControlHandle,
    default_cancel: Cancellation,
    stop_event: Arc<Event>,
    stop_flag: Arc<AtomicBool>,
    ready: Arc<ReadySignal>,
    ended: Arc<AtomicU32>,
    worker: Mutex<Option<JoinHandle<()>>>,
    _reservation: StorageReservation,
}

impl PlatformStream {
    #[inline]
    pub fn native_format(&self) -> &NativeFormat {
        &self.native_format
    }

    pub fn open(target: &CaptureTarget, policy: Overflow) -> Result<Self, CapabilityError> {
        Self::open_with_options(
            target,
            ProcessTreeRange::default(),
            &Backends::default(),
            policy,
            TimingRequirement::Raw,
        )
    }

    pub fn open_with_options(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        policy: Overflow,
        timing: TimingRequirement,
    ) -> Result<Self, CapabilityError> {
        Self::open_with_permission_policy(
            target,
            tree_range,
            backends,
            policy,
            timing,
            PermissionPolicy::AutoRequest,
        )
    }

    pub fn open_with_permission_policy(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        policy: Overflow,
        timing: TimingRequirement,
        _permission_policy: PermissionPolicy,
    ) -> Result<Self, CapabilityError> {
        check_capture_target(target, tree_range, backends, timing)?;
        crate::permissions::ensure_windows_target_permission(target)?;
        let open_target = WasapiOpenTarget::from_capture_target(target)?;
        Self::open_internal(
            open_target,
            target.clone(),
            policy,
            None,
            None,
            Duration::from_secs(5),
        )
        .map_err(|(_, err)| err)
    }

    pub fn open_observed(
        target: &CaptureTarget,
        policy: Overflow,
        trace: Arc<Trace>,
    ) -> Result<Self, CapabilityError> {
        check_capture_target(
            target,
            ProcessTreeRange::default(),
            &Backends::default(),
            TimingRequirement::Raw,
        )?;
        let open_target = WasapiOpenTarget::from_capture_target(target)?;
        Self::open_internal(
            open_target,
            target.clone(),
            policy,
            Some(trace),
            None,
            Duration::from_secs(5),
        )
        .map_err(|(_, err)| err)
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn diagnostic_process_tree_observed(
        root: ProcessIdentity,
        policy: Overflow,
        trace: Arc<Trace>,
    ) -> Result<Self, CapabilityError> {
        let target = CaptureTarget::process(
            root,
            ProcessScope::Tree,
            crate::core::schema::OnRootExit::Stop,
        )
        .map_err(CapabilityError::from)?;
        Self::open_internal(
            WasapiOpenTarget::ProcessTree(root),
            target,
            policy,
            Some(trace),
            None,
            Duration::from_secs(5),
        )
        .map_err(|(_, err)| err)
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn diagnostic_process_tree(
        root: ProcessIdentity,
        policy: Overflow,
    ) -> Result<Self, CapabilityError> {
        let target = CaptureTarget::process(
            root,
            ProcessScope::Tree,
            crate::core::schema::OnRootExit::Stop,
        )
        .map_err(CapabilityError::from)?;
        Self::open_internal(
            WasapiOpenTarget::ProcessTree(root),
            target,
            policy,
            None,
            None,
            Duration::from_secs(5),
        )
        .map_err(|(_, err)| err)
    }

    pub(crate) fn open_reserved_target(
        open_target: WasapiOpenTarget,
        display_target: CaptureTarget,
        policy: Overflow,
        budget: StorageBudget,
        startup_timeout: Duration,
    ) -> Result<Self, (Option<StartupCleanup>, CapabilityError)> {
        Self::open_internal(
            open_target,
            display_target,
            policy,
            None,
            Some(budget),
            startup_timeout,
        )
    }

    fn open_internal(
        open_target: WasapiOpenTarget,
        display_target: CaptureTarget,
        policy: Overflow,
        trace: Option<Arc<Trace>>,
        budget: Option<StorageBudget>,
        startup_timeout: Duration,
    ) -> Result<Self, (Option<StartupCleanup>, CapabilityError)> {
        let stop_event = Arc::new(Event::new(true).map_err(|err| (None, err))?);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(ReadySignal::new());
        let worker_stop_event = stop_event.clone();
        let worker_stop_flag = stop_flag.clone();
        let worker_ready = ready.clone();
        let worker_trace = trace.clone();
        let ended = Arc::new(AtomicU32::new(0));
        let worker_ended = ended.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("wasapi-acquisition".into())
            .spawn(move || {
                let initialized = (|| -> Result<_, CapabilityError> {
                    let apartment = Apartment::new()?;
                    let scheduling = CaptureTask::new()?;
                    let (native, format) = open(open_target, display_target)?;
                    let stride = format.pcm.bytes_per_frame();
                    let limits = StorageLimits::new(8, 4096, stride, 8 * 4096 * stride)
                        .map_err(|_| CapabilityError::CapacityExceeded)?;
                    let active_budget = match budget {
                        Some(provided) => provided,
                        None => StorageBudget::new(1, limits.total_bytes() + 16384)
                            .map_err(|_| CapabilityError::CapacityExceeded)?,
                    };
                    let metadata_bytes = worker_trace.as_ref().map_or(6024usize, |tr| {
                        6024usize.saturating_add(tr.storage_bytes()).min(16384)
                    });
                    let reservation = active_budget
                        .reserve(limits, metadata_bytes)
                        .map_err(|_| CapabilityError::CapacityExceeded)?;
                    let retained_reservation = reservation.retain();
                    let pool = Pool::new_reserved(limits, 4096, format.pcm.clone(), reservation)
                        .map_err(|_| CapabilityError::CapacityExceeded)?;
                    let (producer, reader) = if let Some(tr) = worker_trace {
                        ordering::channel_observed(pool, 4, tr)
                    } else {
                        ordering::channel(pool, 4)
                    }
                    .map_err(|_| CapabilityError::CapacityExceeded)?;
                    // SAFETY: worker owns the initialized native client and its event.
                    unsafe {
                        if worker_stop_flag.load(SeqCst)
                            || WaitForSingleObject(worker_stop_event.handle(), 0) == WAIT_OBJECT_0
                        {
                            return Err(CapabilityError::Stopped);
                        }
                        native.client.Start().map_err(map_windows_error)?;
                    }
                    Ok((
                        apartment,
                        scheduling,
                        native,
                        format,
                        producer,
                        reader,
                        retained_reservation,
                    ))
                })();
                match initialized {
                    Err(error) => {
                        let _ = sender.send(Err(error));
                    }
                    Ok((
                        apartment,
                        scheduling,
                        native,
                        format,
                        mut producer,
                        reader,
                        retained_reservation,
                    )) => {
                        if sender
                            .send(Ok((format, reader, retained_reservation)))
                            .is_ok()
                            && acquire(
                                &native,
                                &mut producer,
                                &worker_stop_event,
                                &worker_stop_flag,
                                &worker_ready,
                                policy,
                                &worker_ended,
                            )
                            .is_err()
                        {
                            let _ = worker_ended.compare_exchange(0, 3, SeqCst, SeqCst);
                            producer.fail_capture(Failure::Backend);
                        }
                        let _ = worker_ended.compare_exchange(0, 1, SeqCst, SeqCst);
                        drop(native);
                        drop(scheduling);
                        drop(apartment);
                        producer.close();
                        drop(producer);
                        worker_ready.notify();
                    }
                }
            })
            .map_err(|_| (None, CapabilityError::CapacityExceeded))?;
        match receiver.recv_timeout(startup_timeout) {
            Ok(Ok((native_format, reader, reservation))) => {
                let control = reader.control_handle();
                Ok(Self {
                    native_format,
                    trace,
                    reads: ReadStream::new(reader),
                    control,
                    default_cancel: Cancellation::default(),
                    stop_event,
                    stop_flag,
                    ready,
                    ended,
                    worker: Mutex::new(Some(worker)),
                    _reservation: reservation,
                })
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                Err((None, error))
            }
            Err(_) => {
                stop_flag.store(true, SeqCst);
                let _ = stop_event.signal();
                let cleanup = StartupCleanup::new(stop_flag, worker);
                Err((Some(cleanup), CapabilityError::CleanupTimeout))
            }
        }
    }

    pub(crate) fn stop_checked(&self, mode: Stop) -> Result<(), CapabilityError> {
        let _ = self.ended.compare_exchange(0, 1, SeqCst, SeqCst);
        self.control.stop(mode);
        self.stop_flag.store(true, SeqCst);
        let res = self.stop_event.signal();
        self.ready.notify();
        res
    }

    pub(crate) fn poll_read_with_cancel(
        &self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        poll_read_stream(
            StreamReadContext {
                reads: &self.reads,
                ready: &self.ready,
                trace: self.trace.as_ref(),
            },
            frames,
            timeout,
            cancel,
        )
    }

    pub(crate) fn poll_read_into_with_cancel(
        &self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        poll_read_stream_into(
            StreamReadContext {
                reads: &self.reads,
                ready: &self.ready,
                trace: self.trace.as_ref(),
            },
            self.native_format.pcm.clone(),
            destination,
            frames,
            timeout,
            cancel,
        )
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn from_synthetic_packets(
        native_format: NativeFormat,
        clock_domain: u64,
        packets_bytes: Vec<(Vec<u8>, usize, u64)>,
        budget: StorageBudget,
    ) -> Result<Self, CapabilityError> {
        let stop_event = Arc::new(Event::new(true)?);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(ReadySignal::new());
        let worker_stop_flag = stop_flag.clone();
        let worker_ready = ready.clone();
        let ended = Arc::new(AtomicU32::new(0));
        let worker_ended = ended.clone();
        let stride = native_format.pcm.bytes_per_frame();
        let slot_count = (packets_bytes.len() + 8).clamp(16, 256);
        let limits = StorageLimits::new(slot_count, 1024, stride, slot_count * 1024 * stride)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let reservation = budget
            .reserve(limits, 6024)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let retained_reservation = reservation.retain();
        let pool = Pool::new_reserved(limits, 1024, native_format.pcm.clone(), reservation)
            .map_err(|_| CapabilityError::CapacityExceeded)?;
        let (mut producer, reader) =
            ordering::channel(pool, slot_count).map_err(|_| CapabilityError::CapacityExceeded)?;
        let control = reader.control_handle();
        let worker = thread::spawn(move || {
            let mut adapter = Packets::with_clock_domain(clock_domain, true);
            let mut pos = 0u64;
            for (bytes, frames, qpc_100ns) in packets_bytes {
                if producer.control_handle().stopping() || worker_stop_flag.load(SeqCst) {
                    break;
                }
                while !producer.control_handle().stopping() && !worker_stop_flag.load(SeqCst) {
                    let pkt = Packet {
                        bytes: &bytes,
                        silent: false,
                        frames,
                        discontinuity: false,
                        timestamp_error: false,
                        position: pos,
                        qpc_100ns,
                    };
                    match adapter.publish(&mut producer, pkt, Overflow::DropNewest) {
                        Offer::Queued => {
                            pos = pos.saturating_add(frames as u64);
                            worker_ready.notify();
                            break;
                        }
                        Offer::QueueDropped | Offer::PoolDropped => {
                            thread::park_timeout(Duration::from_micros(500));
                        }
                        Offer::Failed(_) | Offer::Stopped => break,
                    }
                }
            }
            let _ = worker_ended.compare_exchange(0, 2, SeqCst, SeqCst);
            producer.close();
            drop(producer);
            worker_ready.notify();
        });
        Ok(Self {
            native_format,
            trace: None,
            reads: ReadStream::new(reader),
            control,
            default_cancel: Cancellation::default(),
            stop_event,
            stop_flag,
            ready,
            ended,
            worker: Mutex::new(Some(worker)),
            _reservation: retained_reservation,
        })
    }
}

impl AudioStreamBackend for PlatformStream {
    #[inline]
    fn format(&self) -> &Arc<Format> {
        &self.native_format.pcm
    }

    #[inline]
    fn trace(&self) -> Option<&Arc<Trace>> {
        self.trace.as_ref()
    }

    #[inline]
    fn control_handle(&self) -> ControlHandle {
        self.control.clone()
    }

    #[inline]
    fn default_cancel(&self) -> Cancellation {
        self.default_cancel.clone()
    }

    fn end_reason(&self) -> Option<EndReason> {
        if let Some(failure) = self.control.snapshot().failure {
            return Some(EndReason::Failed(failure));
        }
        match self.ended.load(SeqCst) {
            1 => Some(EndReason::Stopped),
            2 => Some(EndReason::TargetExited),
            3 => Some(EndReason::Failed(Failure::Backend)),
            _ => None,
        }
    }

    #[inline]
    fn read_with_cancel(
        &mut self,
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.poll_read_with_cancel(frames, timeout, cancel)
    }

    #[inline]
    fn read_into_with_cancel(
        &mut self,
        destination: &mut [u8],
        frames: usize,
        timeout: Duration,
        cancel: Cancellation,
    ) -> Result<ReadResult, ReadError> {
        self.poll_read_into_with_cancel(destination, frames, timeout, cancel)
    }

    fn stop(&self, mode: Stop) {
        let _ = self.stop_checked(mode);
    }

    fn resources_done(&self) -> bool {
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(JoinHandle::is_finished)
            && guard.take().unwrap().join().is_err()
        {
            self.control.fail(Failure::Backend);
        }
        guard.is_none() && self.control.resources_done()
    }

    fn wait_cleanup(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        while !self.resources_done() && started.elapsed() < timeout {
            thread::park_timeout(Duration::from_millis(1));
        }
        self.resources_done()
    }
}

impl OpenableStreamBackend for PlatformStream {
    #[inline]
    fn open_with_permission_policy(
        target: &CaptureTarget,
        tree_range: ProcessTreeRange,
        backends: &Backends,
        overflow: Overflow,
        timing: TimingRequirement,
        permission_policy: PermissionPolicy,
    ) -> Result<Self, CapabilityError> {
        Self::open_with_permission_policy(
            target,
            tree_range,
            backends,
            overflow,
            timing,
            permission_policy,
        )
    }
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        self.stop(Stop::Abort);
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;

    #[test]
    fn backlog_does_not_need_another_event() -> Result<(), CapabilityError> {
        let mut remaining = 35;
        let mut consume = || {
            if remaining == 0 {
                return Ok(DrainState::Empty);
            }
            remaining -= 1;
            Ok(DrainState::Consumed)
        };
        assert_eq!(drain_turn(&mut consume)?, DrainState::Consumed);
        assert_eq!(drain_turn(&mut consume)?, DrainState::Empty);
        assert_eq!(remaining, 0);
        Ok(())
    }

    #[test]
    fn stop_and_error_end_the_turn() -> Result<(), CapabilityError> {
        let mut calls = 0;
        assert_eq!(
            drain_turn(|| {
                calls += 1;
                Ok(DrainState::Terminal)
            })?,
            DrainState::Terminal
        );
        assert_eq!(calls, 1);
        assert_eq!(
            drain_turn(|| Err(CapabilityError::DeviceUnavailable)),
            Err(CapabilityError::DeviceUnavailable)
        );
        Ok(())
    }

    #[test]
    fn startup_cleanup_preserves_worker_ownership_on_timeout() -> Result<(), CapabilityError> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_flag.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(SeqCst) {
                thread::park_timeout(Duration::from_millis(5));
            }
        });
        let cleanup = StartupCleanup::new(stop_flag, worker);
        assert!(!cleanup.is_complete());
        assert!(cleanup.wait(Duration::from_millis(500)));
        assert!(cleanup.is_complete());
        Ok(())
    }

    fn read_into_via_generic_stream<B: AudioStreamBackend>(
        stream: &mut crate::core::traits::AudioStream<B>,
        dst: &mut [u8],
        frames: usize,
        timeout: Duration,
    ) -> Result<ReadResult, ReadError> {
        stream.read_into(dst, frames, timeout)
    }

    #[test]
    fn endpoint_stream_read_into_is_zero_allocation_and_zero_timeout_nonblocking()
    -> Result<(), CapabilityError> {
        let fmt = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .unwrap(),
        );
        let budget = StorageBudget::new(4, 4 * 1024 * 1024).unwrap();
        let pkts: Vec<(Vec<u8>, usize, u64)> = (0..12usize)
            .map(|p| (vec![0u8; 256 * 8], 256, 1_000_000 + (p as u64) * 53_333))
            .collect();
        let mut stream = crate::AudioStream::new(PlatformStream::from_synthetic_packets(
            NativeFormat {
                target: CaptureTarget::device("synth-dev").unwrap(),
                native_mix_known: true,
                endpoint: "synth-dev".into(),
                pcm: fmt,
                buffer_frames: 2560,
                channel_mask: Some(0x3),
            },
            1,
            pkts,
            budget,
        )?);
        let mut dst = [0u8; 256 * 8];
        let _ = read_into_via_generic_stream(&mut stream, &mut dst, 256, Duration::from_millis(50))
            .unwrap();
        let allocs = allocation_counter::measure(|| {
            for _ in 0..8 {
                let _ = read_into_via_generic_stream(
                    &mut stream,
                    &mut dst,
                    256,
                    Duration::from_millis(20),
                )
                .unwrap();
            }
        });
        assert_eq!(allocs.count_total, 0);
        stream.stop(Stop::Abort);
        assert!(stream.wait_cleanup(Duration::from_secs(2)));
        Ok(())
    }
}
