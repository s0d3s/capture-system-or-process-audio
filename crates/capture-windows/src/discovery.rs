use crate::{
    capabilities::map_windows_error,
    core::{
        discovery::{
            BackendRawInventory, DeviceDirection, DiscoveryFilter, DiscoveryQuery,
            DiscoveryService, DiscoverySubscription, RawBackendDeviceRecord,
            RawBackendProcessRecord, RawProcessRecord, TargetCatalog,
        },
        schema::{BackendId, Backends, CaptureTarget, OnRootExit, PlatformOs, ProcessScope},
        traits::{CapabilityError, DefaultEndpointObserver, DiscoveryBackend},
    },
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering::SeqCst},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, PROPERTYKEY, RPC_E_CHANGED_MODE},
        Media::Audio::*,
        System::{
            Com::{StructuredStorage::PropVariantClear, *},
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Variant::VT_LPWSTR,
        },
        UI::Shell::PropertiesSystem::IPropertyStore,
    },
    core::{GUID, Interface, PCWSTR, PWSTR, implement},
};

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

const PKEY_DEVICE_DESC: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 2,
};

struct Apartment {
    uninit_on_drop: bool,
}
impl Apartment {
    fn new() -> Result<Self, CapabilityError> {
        // SAFETY: COM apartment initialized on the current thread and balanced on drop when S_OK/S_FALSE.
        // If the thread is already initialized as STA (RPC_E_CHANGED_MODE), COM is already active on this
        // thread and must NOT be uninitialized by this guard.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr == RPC_E_CHANGED_MODE {
                return Ok(Self {
                    uninit_on_drop: false,
                });
            }
            hr.ok().map_err(map_windows_error)?;
        }
        Ok(Self {
            uninit_on_drop: true,
        })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.uninit_on_drop {
            // SAFETY: balances successful CoInitializeEx on the same thread.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

unsafe fn read_property_wstring(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: GetValue initializes PROPVARIANT which is cleared via PropVariantClear before return.
    unsafe {
        let mut prop = store.GetValue(key).ok()?;
        let out = if prop.Anonymous.Anonymous.vt == VT_LPWSTR {
            let wide_ptr: PWSTR = prop.Anonymous.Anonymous.Anonymous.pwszVal;
            if !wide_ptr.is_null() {
                wide_ptr
                    .to_string()
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        } else {
            None
        };
        let _ = PropVariantClear(&mut prop);
        out
    }
}

/// Query the human-readable endpoint name (`PKEY_Device_FriendlyName`, fallback `PKEY_Device_DeviceDesc`,
/// then `fallback_id`). `device_id` remains the canonical capture identity.
unsafe fn query_endpoint_friendly_name(device: &IMMDevice, fallback_id: &str) -> String {
    // SAFETY: OpenPropertyStore(STGM_READ) reads endpoint metadata without mutating device state.
    unsafe {
        if let Ok(store) = device.OpenPropertyStore(STGM_READ) {
            if let Some(friendly) = read_property_wstring(&store, &PKEY_DEVICE_FRIENDLY_NAME) {
                return friendly;
            }
            if let Some(desc) = read_property_wstring(&store, &PKEY_DEVICE_DESC) {
                return desc;
            }
        }
    }
    fallback_id.to_owned()
}

/// Query the current OS default render endpoint ID (`eRender`, `eConsole`).
pub fn query_default_render_endpoint_id() -> Result<Option<String>, CapabilityError> {
    query_default_endpoint_id_for_flow(eRender)
}

/// Query the current OS default capture (microphone) endpoint ID (`eCapture`, `eConsole`).
pub fn query_default_capture_endpoint_id() -> Result<Option<String>, CapabilityError> {
    query_default_endpoint_id_for_flow(eCapture)
}

fn query_default_endpoint_id_for_flow(flow: EDataFlow) -> Result<Option<String>, CapabilityError> {
    let _apt = Apartment::new()?;
    // SAFETY: standard COM enumerator call within an initialized apartment; frees GetId allocation.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(map_windows_error)?;
        let default_id = enumerator
            .GetDefaultAudioEndpoint(flow, eConsole)
            .ok()
            .and_then(|dev| {
                let raw = dev.GetId().ok()?;
                let text = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0.cast()));
                text
            });
        Ok(default_id)
    }
}

type WasapiEndpointScan = (Vec<RawBackendDeviceRecord>, BTreeMap<u32, bool>);

fn collect_wasapi_all_endpoints_and_sessions(
    enumerator: Option<&IMMDeviceEnumerator>,
) -> Result<WasapiEndpointScan, CapabilityError> {
    let local_apt;
    let local_enum;
    let enum_ref = match enumerator {
        Some(r) => r,
        None => {
            local_apt = Apartment::new()?;
            let _ = &local_apt;
            // SAFETY: standard COM enumerator call within an initialized apartment.
            local_enum = unsafe {
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(map_windows_error)?
            };
            &local_enum
        }
    };
    collect_wasapi_endpoints_with_enumerator(enum_ref)
}

fn collect_wasapi_endpoints_with_enumerator(
    enumerator: &IMMDeviceEnumerator,
) -> Result<WasapiEndpointScan, CapabilityError> {
    let mut all_backend_devices = Vec::new();
    let mut pid_audio: BTreeMap<u32, bool> = BTreeMap::new();

    // SAFETY: standard COM enumerator calls within an initialized apartment.
    unsafe {
        let default_render_id = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|dev| {
                let raw = dev.GetId().ok()?;
                let text = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0.cast()));
                text
            });

        let render_collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(map_windows_error)?;
        let render_count = render_collection.GetCount().map_err(map_windows_error)?;
        for idx in 0..render_count {
            let Ok(device) = render_collection.Item(idx) else {
                continue;
            };
            let Ok(raw_id) = device.GetId() else {
                continue;
            };
            let id_str = raw_id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(raw_id.0.cast()));
            if id_str.is_empty() {
                continue;
            }
            let is_default = default_render_id.as_deref() == Some(id_str.as_str());
            let friendly_name: Arc<str> = query_endpoint_friendly_name(&device, &id_str).into();
            all_backend_devices.push(RawBackendDeviceRecord {
                backend: BackendId::Wasapi,
                direction: DeviceDirection::Output,
                device_id: id_str.into(),
                name: Some(friendly_name),
                is_default,
                is_monitor_sink: false,
            });

            let Ok(manager2) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(session_enum) = manager2.GetSessionEnumerator() else {
                continue;
            };
            let Ok(session_count) = session_enum.GetCount() else {
                continue;
            };
            for s_idx in 0..session_count {
                let Ok(control) = session_enum.GetSession(s_idx) else {
                    continue;
                };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                if control2.IsSystemSoundsSession().is_ok()
                    && control2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK
                {
                    continue;
                }
                let Ok(pid) = control2.GetProcessId() else {
                    continue;
                };
                if pid == 0 {
                    continue;
                }
                let Ok(state) = control.GetState() else {
                    continue;
                };
                if state == AudioSessionStateExpired {
                    continue;
                }
                let is_active = state == AudioSessionStateActive;
                let entry = pid_audio.entry(pid).or_insert(false);
                if is_active {
                    *entry = true;
                }
            }
        }

        let default_capture_id = enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|dev| {
                let raw = dev.GetId().ok()?;
                let text = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0.cast()));
                text
            });

        if let Ok(capture_collection) = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            && let Ok(capture_count) = capture_collection.GetCount()
        {
            for idx in 0..capture_count {
                let Ok(device) = capture_collection.Item(idx) else {
                    continue;
                };
                let Ok(raw_id) = device.GetId() else {
                    continue;
                };
                let id_str = raw_id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(raw_id.0.cast()));
                if id_str.is_empty() {
                    continue;
                };
                let is_default = default_capture_id.as_deref() == Some(id_str.as_str());
                let friendly_name: Arc<str> = query_endpoint_friendly_name(&device, &id_str).into();
                all_backend_devices.push(RawBackendDeviceRecord {
                    backend: BackendId::Wasapi,
                    direction: DeviceDirection::Input,
                    device_id: id_str.into(),
                    name: Some(friendly_name),
                    is_default,
                    is_monitor_sink: false,
                });
            }
        }
    }

    Ok((all_backend_devices, pid_audio))
}

/// Enumerate active WASAPI `eRender` and `eCapture` endpoints plus OS processes via
/// `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)` into a unified `BackendRawInventory`.
pub fn collect_windows_backend_inventory() -> Result<BackendRawInventory, CapabilityError> {
    collect_windows_backend_inventory_with_enumerator(None)
}

fn collect_windows_backend_inventory_with_enumerator(
    enumerator: Option<&IMMDeviceEnumerator>,
) -> Result<BackendRawInventory, CapabilityError> {
    let (backend_devices, pid_audio) = collect_wasapi_all_endpoints_and_sessions(enumerator)?;
    let mut backend_processes = Vec::new();

    // SAFETY: CreateToolhelp32Snapshot returns an owned snapshot handle closed on exit.
    unsafe {
        let snapshot =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(map_windows_error)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            let pid = entry.th32ProcessID;
            let parent_pid = (entry.th32ParentProcessID != 0 && entry.th32ParentProcessID != pid)
                .then_some(entry.th32ParentProcessID);
            if pid != 0
                && let Ok((identity, exe_path)) = crate::process::inspect_process_executable(pid)
            {
                let nul_pos = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe_name = String::from_utf16_lossy(&entry.szExeFile[..nul_pos]);
                let active_audio = pid_audio.get(&pid).copied();
                let name_arc: Arc<str> = if exe_name.is_empty() {
                    exe_path.clone().into()
                } else {
                    exe_name.into()
                };
                let exe_arc: Arc<str> = exe_path.into();
                let rec = RawProcessRecord {
                    identity,
                    parent_pid,
                    app_id: Some(exe_arc.clone()),
                    name: Some(name_arc),
                    executable_path: Some(exe_arc),
                    active_audio,
                };
                backend_processes.push(RawBackendProcessRecord {
                    backend: BackendId::Wasapi,
                    process: rec,
                });
            }
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }

    Ok(BackendRawInventory {
        processes: backend_processes,
        devices: backend_devices,
    })
}

struct WatcherWorker {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WatcherWorker {
    fn stop_and_join(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.join.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

#[implement(IMMNotificationClient)]
struct EndpointNotificationClient {
    target_flow: Option<EDataFlow>,
    latest_default_id: Option<Arc<Mutex<Option<String>>>>,
    dirty: Option<Arc<AtomicBool>>,
    wake_thread: Thread,
}

impl IMMNotificationClient_Impl for EndpointNotificationClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _dwnewstate: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        if let Some(ref dirty) = self.dirty {
            dirty.store(true, SeqCst);
        }
        self.wake_thread.unpark();
        Ok(())
    }

    fn OnDeviceAdded(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        if let Some(ref dirty) = self.dirty {
            dirty.store(true, SeqCst);
        }
        self.wake_thread.unpark();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        if let Some(ref dirty) = self.dirty {
            dirty.store(true, SeqCst);
        }
        self.wake_thread.unpark();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        pwstrdefaultdeviceid: &PCWSTR,
    ) -> windows::core::Result<()> {
        let matches_flow = match self.target_flow {
            Some(expected) => flow == expected,
            None => flow == eRender || flow == eCapture,
        };
        if matches_flow && role == eConsole {
            let new_id = if pwstrdefaultdeviceid.is_null() {
                None
            } else {
                // SAFETY: `pwstrdefaultdeviceid` is a valid null-terminated wide string during callback.
                unsafe { pwstrdefaultdeviceid.to_string().ok() }.filter(|s| !s.is_empty())
            };
            if let Some(ref latest) = self.latest_default_id {
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = new_id;
            }
            if let Some(ref dirty) = self.dirty {
                dirty.store(true, SeqCst);
            }
            self.wake_thread.unpark();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        if let Some(ref dirty) = self.dirty {
            dirty.store(true, SeqCst);
        }
        self.wake_thread.unpark();
        Ok(())
    }
}

impl Drop for WatcherWorker {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// One-shot snapshots never leave background observer threads running.
#[derive(Clone)]
pub struct PlatformDiscovery {
    service: DiscoveryService,
    backends: Backends,
    worker: Arc<Mutex<Option<WatcherWorker>>>,
}

impl Default for PlatformDiscovery {
    fn default() -> Self {
        <Self as DiscoveryBackend>::new()
    }
}

impl PlatformDiscovery {
    fn validate_windows_backends(backends: &Backends) -> Result<(), CapabilityError> {
        backends
            .validate()
            .map_err(|_| CapabilityError::InvalidBackend)?;
        if !backends
            .for_os(PlatformOs::Windows)
            .contains(&BackendId::Wasapi)
        {
            return Err(CapabilityError::InvalidBackend);
        }
        Ok(())
    }

    fn reap_idle_watcher(&self) {
        if self.service.active_subscriber_count() == 0 {
            let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut worker) = guard.take() {
                worker.stop_and_join();
            }
        }
    }

    fn ensure_watcher_started(&self) {
        let mut guard = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let service = self.service.clone();
        let backends = self.backends.clone();
        let join = thread::spawn(move || {
            let _apt = Apartment::new().ok();
            let cached_enum: Option<IMMDeviceEnumerator> =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
            let dirty = Arc::new(AtomicBool::new(true));
            let client: IMMNotificationClient = EndpointNotificationClient {
                target_flow: None,
                latest_default_id: None,
                dirty: Some(dirty.clone()),
                wake_thread: thread::current(),
            }
            .into();
            let registered = if let Some(ref enum_ref) = cached_enum {
                // SAFETY: `client` is a valid COM object kept alive across the loop and unregistered before drop.
                unsafe {
                    enum_ref
                        .RegisterEndpointNotificationCallback(&client)
                        .is_ok()
                }
            } else {
                false
            };

            let mut last_scan = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            let mut win_query = DiscoveryQuery::all_for_os(PlatformOs::Windows);
            win_query.backends = backends;

            while !stop_clone.load(SeqCst) {
                if service.active_subscriber_count() == 0 {
                    break;
                }
                let is_dirty = dirty.swap(false, SeqCst);
                if is_dirty || last_scan.elapsed() >= Duration::from_millis(500) {
                    if let Ok(backend_inv) =
                        collect_windows_backend_inventory_with_enumerator(cached_enum.as_ref())
                    {
                        let _ = service.ingest_backend_inventory(backend_inv, &win_query);
                    }
                    last_scan = Instant::now();
                }
                if !dirty.load(SeqCst) && !stop_clone.load(SeqCst) {
                    thread::park_timeout(Duration::from_millis(50));
                }
            }

            if registered && let Some(ref enum_ref) = cached_enum {
                // SAFETY: unregisters the callback on the owning thread outside any callback invocation.
                unsafe {
                    let _ = enum_ref.UnregisterEndpointNotificationCallback(&client);
                }
            }
        });
        *guard = Some(WatcherWorker {
            stop,
            join: Some(join),
        });
    }
}

