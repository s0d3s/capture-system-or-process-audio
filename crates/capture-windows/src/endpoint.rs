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

