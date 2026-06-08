#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::capabilities::{
    MIN_COREAUDIO_INPUT_OS_VERSION, MIN_COREAUDIO_TAP_OS_VERSION,
    MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION,
};
use crate::core::{
    discovery::{
        BackendRawInventory, DeviceDirection, DiscoveryFilter, DiscoveryQuery, DiscoveryService,
        DiscoverySubscription, RawBackendDeviceRecord, RawBackendProcessRecord, RawProcessRecord,
        TargetCatalog, build_catalog_from_backend_inventory,
    },
    schema::{BackendId, Backends, CaptureTarget, OnRootExit, PlatformOs, ProcessScope},
    traits::{CapabilityError, DefaultEndpointObserver, DiscoveryBackend},
};
use crate::proc_info::{BsdProcRecord, collect_bsd_proc_table, inspect_process_executable};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering::SeqCst},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreAudioProcessObjectRecord {
    pub object_id: u32,
    pub pid: u32,
    pub start_time_us: u64,
    pub running_output: bool,
    pub bundle_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SckShareableAppRecord {
    pub pid: u32,
    pub start_time_us: u64,
    pub bundle_id: String,
    pub app_name: String,
}

#[cfg(target_os = "macos")]
pub fn collect_live_coreaudio_process_objects() -> Result<
    (
        BTreeMap<u32, BsdProcRecord>,
        Vec<CoreAudioProcessObjectRecord>,
    ),
    CapabilityError,
> {
    let proc_table = collect_bsd_proc_table()?;
    let raw = crate::native_bridge::list_coreaudio_process_objects()?;
    let mut raw_tuples = Vec::with_capacity(raw.len());
    let mut bundle_map = BTreeMap::new();
    for (obj_id, pid, running, bundle) in raw {
        raw_tuples.push((obj_id, pid, running));
        if let Some(b) = bundle {
            bundle_map.insert(obj_id, b);
        }
    }
    let live = crate::proc_info::filter_live_audio_process_objects(&proc_table, &raw_tuples);
    let records = live
        .into_iter()
        .map(
            |(object_id, pid, running_output, start_time_us)| CoreAudioProcessObjectRecord {
                object_id,
                pid,
                start_time_us,
                running_output,
                bundle_id: bundle_map.remove(&object_id),
            },
        )
        .collect();
    Ok((proc_table, records))
}

#[cfg(not(target_os = "macos"))]
pub fn collect_live_coreaudio_process_objects() -> Result<
    (
        BTreeMap<u32, BsdProcRecord>,
        Vec<CoreAudioProcessObjectRecord>,
    ),
    CapabilityError,
> {
    let proc_table = collect_bsd_proc_table()?;
    Ok((proc_table, Vec::new()))
}

#[cfg(target_os = "macos")]
pub fn collect_live_sck_applications(
    proc_table: &BTreeMap<u32, BsdProcRecord>,
) -> Result<Vec<SckShareableAppRecord>, CapabilityError> {
    let raw_apps = crate::native_bridge::list_sck_shareable_applications()?;
    let mut out = Vec::with_capacity(raw_apps.len());
    for (pid, bundle_id, app_name) in raw_apps {
        if let Some(proc_rec) = proc_table.get(&pid) {
            out.push(SckShareableAppRecord {
                pid,
                start_time_us: proc_rec.start_time_us,
                bundle_id,
                app_name,
            });
        }
    }
    Ok(out)
}

#[cfg(not(target_os = "macos"))]
pub fn collect_live_sck_applications(
    _proc_table: &BTreeMap<u32, BsdProcRecord>,
) -> Result<Vec<SckShareableAppRecord>, CapabilityError> {
    Err(CapabilityError::ServerUnavailable)
}

pub fn is_private_macos_endpoint_uid(uid: &str) -> bool {
    uid.is_empty()
        || uid.starts_with("CADefaultDeviceAggregate")
        || uid.starts_with("com.apple.audio.CoreAudioTap")
        || uid.starts_with("cspa-private-agg-")
}

#[allow(clippy::too_many_arguments)]
pub fn collect_macos_backend_inventory_with_state(
    proc_table: &BTreeMap<u32, BsdProcRecord>,
    ca_objs: &[CoreAudioProcessObjectRecord],
    running_apps: &[(u32, String, String, String)],
    sck_shareable_apps: &[SckShareableAppRecord],
    ca_output_devs: &[(String, String, bool)],
    ca_input_devs: &[(String, String, bool)],
    os_version: (u32, u32, u32),
    sck_granted: bool,
    backends: &Backends,
) -> BackendRawInventory {
    let mut pid_audio: BTreeMap<u32, bool> = BTreeMap::new();
    let mut pid_bundle: BTreeMap<u32, String> = BTreeMap::new();
    let mut pid_app_name: BTreeMap<u32, String> = BTreeMap::new();
    let mut pid_exe_override: BTreeMap<u32, String> = BTreeMap::new();
    let mut sck_pids: BTreeSet<u32> = BTreeSet::new();

    for obj in ca_objs {
        let entry = pid_audio.entry(obj.pid).or_insert(false);
        if obj.running_output {
            *entry = true;
        }
        if let Some(ref b) = obj.bundle_id {
            pid_bundle.entry(obj.pid).or_insert_with(|| b.clone());
        }
    }

    for (pid, bundle_id, app_name, exe_path) in running_apps {
        if !bundle_id.is_empty() {
            pid_bundle.insert(*pid, bundle_id.clone());
        }
        if !app_name.is_empty() {
            pid_app_name.insert(*pid, app_name.clone());
        }
        if !exe_path.is_empty() && !exe_path.contains(".code_sign_clone/") {
            pid_exe_override.insert(*pid, exe_path.clone());
        }
    }

    for app in sck_shareable_apps {
        sck_pids.insert(app.pid);
        if !app.bundle_id.is_empty() {
            pid_bundle
                .entry(app.pid)
                .or_insert_with(|| app.bundle_id.clone());
        }
        if !app.app_name.is_empty() {
            pid_app_name
                .entry(app.pid)
                .or_insert_with(|| app.app_name.clone());
        }
    }

    let mut all_procs = Vec::with_capacity(proc_table.len());
    for (&pid, rec) in proc_table {
        let parent_pid = (rec.ppid > 0 && rec.ppid != pid).then_some(rec.ppid);
        let exe_str = pid_exe_override
            .remove(&pid)
            .or_else(|| rec.exe_path.clone())
            .unwrap_or_else(|| rec.comm.clone());
        let app_id_str = pid_bundle.remove(&pid).unwrap_or_else(|| exe_str.clone());
        let display_name = pid_app_name
            .remove(&pid)
            .unwrap_or_else(|| rec.comm.clone());
        let active_audio = pid_audio.get(&pid).copied();
        all_procs.push(RawProcessRecord {
            identity: crate::core::schema::ProcessIdentity {
                pid,
                generation: rec.start_time_us,
            },
            parent_pid,
            app_id: Some(app_id_str.into()),
            name: Some(display_name.into()),
            executable_path: Some(exe_str.into()),
            active_audio,
        });
    }

    let mut rolled_app_audio: BTreeMap<u32, Option<bool>> = BTreeMap::new();
    if let Ok(ca_only_backends) = Backends::new(None, None, Some(vec![BackendId::CoreAudio])) {
        let roll_inv = BackendRawInventory {
            processes: all_procs
                .iter()
                .cloned()
                .map(|process| RawBackendProcessRecord {
                    backend: BackendId::CoreAudio,
                    process,
                })
                .collect(),
            devices: Vec::new(),
        };
        let roll_query = DiscoveryQuery::from_filter_for_os(
            PlatformOs::MacOs,
            &DiscoveryFilter::applications(false, true),
        )
        .with_backends(ca_only_backends);
        if let Ok(rolled_cat) = build_catalog_from_backend_inventory(&roll_inv, &roll_query, 1) {
            for app in rolled_cat.applications {
                if let Some(pid) = app.pid() {
                    rolled_app_audio.insert(pid, app.active_audio());
                }
            }
        }
    }

    let mut out = BackendRawInventory::default();
    for &backend in backends.for_os(PlatformOs::MacOs) {
        match backend {
            BackendId::CoreAudio => {
                if os_version >= MIN_COREAUDIO_TAP_OS_VERSION {
                    for proc_rec in &all_procs {
                        out.processes.push(RawBackendProcessRecord {
                            backend: BackendId::CoreAudio,
                            process: proc_rec.clone(),
                        });
                    }
                    for (uid, name, is_default) in ca_output_devs {
                        if !is_private_macos_endpoint_uid(uid) {
                            out.devices.push(RawBackendDeviceRecord {
                                backend: BackendId::CoreAudio,
                                direction: DeviceDirection::Output,
                                device_id: uid.as_str().into(),
                                name: Some(name.as_str().into()),
                                is_default: *is_default,
                                is_monitor_sink: false,
                            });
                        }
                    }
                }
                if os_version >= MIN_COREAUDIO_INPUT_OS_VERSION {
                    for (uid, name, is_default) in ca_input_devs {
                        if !is_private_macos_endpoint_uid(uid) {
                            out.devices.push(RawBackendDeviceRecord {
                                backend: BackendId::CoreAudio,
                                direction: DeviceDirection::Input,
                                device_id: uid.as_str().into(),
                                name: Some(name.as_str().into()),
                                is_default: *is_default,
                                is_monitor_sink: false,
                            });
                        }
                    }
                }
            }
            BackendId::ScreenCaptureKit
                if os_version >= MIN_SCREENCAPTUREKIT_AUDIO_OS_VERSION && sck_granted =>
            {
                for proc_rec in &all_procs {
                    if sck_pids.contains(&proc_rec.identity.pid) {
                        let mut sck_proc = proc_rec.clone();
                        sck_proc.parent_pid = None;
                        if let Some(&rolled) = rolled_app_audio.get(&proc_rec.identity.pid) {
                            sck_proc.active_audio = rolled;
                        }
                        out.processes.push(RawBackendProcessRecord {
                            backend: BackendId::ScreenCaptureKit,
                            process: sck_proc,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn collect_macos_backend_inventory(
    backends: &Backends,
) -> Result<BackendRawInventory, CapabilityError> {
    let (proc_table, ca_objs) = collect_live_coreaudio_process_objects()?;
    #[cfg(target_os = "macos")]
    {
        let os_ver = crate::native_bridge::os_version();
        let sck_granted = crate::native_bridge::sck_preflight_granted();
        let running_apps = crate::native_bridge::list_running_applications().unwrap_or_default();
        let want_sck = backends
            .for_os(PlatformOs::MacOs)
            .contains(&BackendId::ScreenCaptureKit);
        let sck_apps = if want_sck && sck_granted {
            collect_live_sck_applications(&proc_table).unwrap_or_default()
        } else {
            Vec::new()
        };
        let out_devs: Vec<(String, String, bool)> =
            crate::native_bridge::list_coreaudio_output_devices()
                .unwrap_or_default()
                .into_iter()
                .map(|(_id, uid, name, _rate, _ch, is_def)| (uid, name, is_def))
                .collect();
        let in_devs: Vec<(String, String, bool)> =
            crate::native_bridge::list_coreaudio_input_devices()
                .unwrap_or_default()
                .into_iter()
                .map(|(_id, uid, name, _rate, _ch, _transport, is_def)| (uid, name, is_def))
                .collect();
        Ok(collect_macos_backend_inventory_with_state(
            &proc_table,
            &ca_objs,
            &running_apps,
            &sck_apps,
            &out_devs,
            &in_devs,
            os_ver,
            sck_granted,
            backends,
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let out_devs = vec![(
            "BuiltInSpeakerDevice".to_owned(),
            "MacBook Pro Speakers".to_owned(),
            true,
        )];
        let in_devs = vec![(
            "BuiltInMicrophoneDevice".to_owned(),
            "MacBook Pro Microphone".to_owned(),
            true,
        )];
        Ok(collect_macos_backend_inventory_with_state(
            &proc_table,
            &ca_objs,
            &[],
            &[],
            &out_devs,
            &in_devs,
            (15, 7, 9),
            false,
            backends,
        ))
    }
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

impl Drop for WatcherWorker {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// One-shot queries never spawn background watcher threads.
#[derive(Clone)]
pub struct PlatformDiscovery {
    service: DiscoveryService,
    backends: Backends,
    watcher: Arc<Mutex<Option<WatcherWorker>>>,
}

impl Default for PlatformDiscovery {
    fn default() -> Self {
        <Self as DiscoveryBackend>::new()
    }
}

impl PlatformDiscovery {
    fn ensure_watcher_started(&self) {
        let mut guard = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let service = self.service.clone();
        let backends = self.backends.clone();

        let join = thread::spawn(move || {
            let dirty = Arc::new(AtomicBool::new(true));
            #[cfg(target_os = "macos")]
            let _listener_guard = {
                struct ListenerCtx {
                    dirty: Arc<AtomicBool>,
                    wake: thread::Thread,
                }
                #[allow(unsafe_code)]
                unsafe extern "C" fn on_device_prop_changed(user_data: *mut std::ffi::c_void) {
                    if !user_data.is_null() {
                        // SAFETY: `user_data` points to `ListenerCtx` kept alive across `_listener`.
                        let ctx = unsafe { &*(user_data as *const ListenerCtx) };
                        ctx.dirty.store(true, SeqCst);
                        ctx.wake.unpark();
                    }
                }
                let ctx = Box::new(ListenerCtx {
                    dirty: dirty.clone(),
                    wake: thread::current(),
                });
                let ctx_ptr = &*ctx as *const ListenerCtx as *mut std::ffi::c_void;
                let listener = crate::native_bridge::NativeDeviceListenerHandle::register(
                    on_device_prop_changed,
                    ctx_ptr,
                );
                (listener, ctx)
            };

            let mut last_scan = std::time::Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now);

            let mac_query =
                DiscoveryQuery::all_for_os(PlatformOs::MacOs).with_backends(backends.clone());

            while !stop_bg.load(SeqCst) {
                if service.active_subscriber_count() == 0 {
                    break;
                }
                let is_dirty = dirty.swap(false, SeqCst);
                if is_dirty || last_scan.elapsed() >= Duration::from_millis(500) {
                    if let Ok(inv) = collect_macos_backend_inventory(&backends) {
                        let _ = service.ingest_backend_inventory(inv, &mac_query);
                    }
                    last_scan = std::time::Instant::now();
                }
                if !dirty.load(SeqCst) && !stop_bg.load(SeqCst) {
                    thread::park_timeout(Duration::from_millis(50));
                }
            }
        });

        *guard = Some(WatcherWorker {
            stop,
            join: Some(join),
        });
    }
}

impl DiscoveryBackend for PlatformDiscovery {
    fn new() -> Self {
        Self {
            service: DiscoveryService::new_for_os(PlatformOs::MacOs),
            backends: Backends::default(),
            watcher: Arc::new(Mutex::new(None)),
        }
    }

    fn with_backends(backends: Backends) -> Result<Self, CapabilityError> {
        backends.validate()?;
        Ok(Self {
            service: DiscoveryService::new_for_os(PlatformOs::MacOs),
            backends,
            watcher: Arc::new(Mutex::new(None)),
        })
    }

    #[inline]
    fn service(&self) -> &DiscoveryService {
        &self.service
    }

    #[inline]
    fn backends(&self) -> &Backends {
        &self.backends
    }

    fn is_watcher_running(&self) -> bool {
        let mut guard = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if self.service.active_subscriber_count() == 0
            && let Some(mut worker) = guard.take()
        {
            worker.stop_and_join();
        }
        guard.is_some()
    }

    fn refresh_once(&self) -> Result<u64, CapabilityError> {
        let query =
            DiscoveryQuery::all_for_os(PlatformOs::MacOs).with_backends(self.backends.clone());
        let inv = collect_macos_backend_inventory(&self.backends)?;
        self.service
            .update_backend_inventory(inv, &query)
            .map_err(CapabilityError::from)
    }

    fn default_output_device_id(&self) -> Result<Option<Arc<str>>, CapabilityError> {
        if !self.is_watcher_running() {
            self.refresh_once()?;
        }
        Ok(self.service.default_output_device_id())
    }

    fn default_input_device_id(&self) -> Result<Option<Arc<str>>, CapabilityError> {
        if !self.is_watcher_running() {
            self.refresh_once()?;
        }
        Ok(self.service.default_input_device_id())
    }

    fn process(
        &self,
        pid: u32,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, CapabilityError> {
        let (identity, exe) = inspect_process_executable(pid)?;
        let name = exe.rsplit('/').next().unwrap_or(exe.as_str()).to_owned();
        let target = CaptureTarget::process(identity, scope, on_root_exit)?;
        Ok(target.with_metadata(None, Some(name.into()), Some(exe.into()), None))
    }

    fn discover_query(&self, query: &DiscoveryQuery) -> Result<TargetCatalog, CapabilityError> {
        query.backends.validate()?;
        let inv = collect_macos_backend_inventory(&query.backends)?;
        self.service
            .update_backend_inventory(inv, query)
            .map_err(CapabilityError::from)?;
        self.service
            .discover_query(query)
            .map_err(CapabilityError::from)
    }

    fn subscribe(
        &self,
        filter: DiscoveryFilter,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, CapabilityError> {
        let _ = self.refresh_once();
        let sub = self.service.subscribe(filter, max_discovery_queue)?;
        self.ensure_watcher_started();
        Ok(sub)
    }

    fn subscribe_query(
        &self,
        query: DiscoveryQuery,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, CapabilityError> {
        query.backends.validate()?;
        if let Ok(inv) = collect_macos_backend_inventory(&query.backends) {
            let _ = self.service.update_backend_inventory(inv, &query);
        }
        let sub = self.service.subscribe_query(query, max_discovery_queue)?;
        self.ensure_watcher_started();
        Ok(sub)
    }
}

struct DefaultEndpointObserverState {
    ref_count: usize,
    latest_uid: Arc<Mutex<Option<String>>>,
    worker: Option<WatcherWorker>,
}

static MAC_DEFAULT_OUTPUT_OBSERVER: OnceLock<Mutex<DefaultEndpointObserverState>> = OnceLock::new();
static MAC_DEFAULT_INPUT_OBSERVER: OnceLock<Mutex<DefaultEndpointObserverState>> = OnceLock::new();

fn mac_output_observer_state() -> &'static Mutex<DefaultEndpointObserverState> {
    MAC_DEFAULT_OUTPUT_OBSERVER.get_or_init(|| {
        Mutex::new(DefaultEndpointObserverState {
            ref_count: 0,
            latest_uid: Arc::new(Mutex::new(None)),
            worker: None,
        })
    })
}

fn mac_input_observer_state() -> &'static Mutex<DefaultEndpointObserverState> {
    MAC_DEFAULT_INPUT_OBSERVER.get_or_init(|| {
        Mutex::new(DefaultEndpointObserverState {
            ref_count: 0,
            latest_uid: Arc::new(Mutex::new(None)),
            worker: None,
        })
    })
}

pub fn query_macos_default_output_uid_now() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::native_bridge::list_coreaudio_output_devices()
            .ok()
            .and_then(|devs| {
                devs.iter()
                    .find(|d| d.5)
                    .or_else(|| devs.first())
                    .map(|d| d.1.clone())
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some("BuiltInSpeakerDevice".to_owned())
    }
}

pub fn query_macos_default_input_uid_now() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::native_bridge::list_coreaudio_input_devices()
            .ok()
            .and_then(|devs| {
                devs.iter()
                    .find(|d| d.6)
                    .or_else(|| devs.first())
                    .map(|d| d.1.clone())
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some("BuiltInMicrophoneDevice".to_owned())
    }
}

/// Shared background observer for the macOS default output device UID.
pub struct PlatformDefaultOutputObserver {
    latest_uid: Arc<Mutex<Option<String>>>,
}

impl DefaultEndpointObserver for PlatformDefaultOutputObserver {
    fn acquire() -> Self {
        let mut guard = mac_output_observer_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.ref_count == 0 {
            let initial = query_macos_default_output_uid_now();
            *guard.latest_uid.lock().unwrap_or_else(|e| e.into_inner()) = initial;
            let stop = Arc::new(AtomicBool::new(false));
            let stop_bg = stop.clone();
            let latest_bg = guard.latest_uid.clone();
            let join = thread::spawn(move || {
                let dirty = Arc::new(AtomicBool::new(false));
                #[cfg(target_os = "macos")]
                let _listener_guard = {
                    struct ListenerCtx {
                        dirty: Arc<AtomicBool>,
                        wake: thread::Thread,
                    }
                    #[allow(unsafe_code)]
                    unsafe extern "C" fn on_default_prop_changed(user_data: *mut std::ffi::c_void) {
                        if !user_data.is_null() {
                            // SAFETY: `user_data` points to `ListenerCtx` kept alive across `_listener`.
                            let ctx = unsafe { &*(user_data as *const ListenerCtx) };
                            ctx.dirty.store(true, SeqCst);
                            ctx.wake.unpark();
                        }
                    }
                    let ctx = Box::new(ListenerCtx {
                        dirty: dirty.clone(),
                        wake: thread::current(),
                    });
                    let ctx_ptr = &*ctx as *const ListenerCtx as *mut std::ffi::c_void;
                    let listener = crate::native_bridge::NativeDeviceListenerHandle::register(
                        on_default_prop_changed,
                        ctx_ptr,
                    );
                    (listener, ctx)
                };

                while !stop_bg.load(SeqCst) {
                    if dirty.swap(false, SeqCst)
                        && let Some(uid) = query_macos_default_output_uid_now()
                    {
                        *latest_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(uid);
                    }
                    if !dirty.load(SeqCst) && !stop_bg.load(SeqCst) {
                        thread::park_timeout(Duration::from_millis(500));
                    }
                }
            });
            guard.worker = Some(WatcherWorker {
                stop,
                join: Some(join),
            });
        }
        guard.ref_count += 1;
        Self {
            latest_uid: guard.latest_uid.clone(),
        }
    }

    fn latest_endpoint_id(&self) -> Option<String> {
        self.latest_uid
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn changed_from(&self, expected_id: Option<&str>) -> Option<String> {
        let guard = self.latest_uid.lock().unwrap_or_else(|e| e.into_inner());
        let latest = guard.as_deref()?;
        if expected_id != Some(latest) {
            Some(latest.to_owned())
        } else {
            None
        }
    }
}

impl Drop for PlatformDefaultOutputObserver {
    fn drop(&mut self) {
        let mut guard = mac_output_observer_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.ref_count = guard.ref_count.saturating_sub(1);
        if guard.ref_count == 0
            && let Some(mut worker) = guard.worker.take()
        {
            worker.stop_and_join();
        }
    }
}

/// Shared background observer for the macOS default input device UID.
pub struct PlatformDefaultInputObserver {
    latest_uid: Arc<Mutex<Option<String>>>,
}

impl DefaultEndpointObserver for PlatformDefaultInputObserver {
    fn acquire() -> Self {
        let mut guard = mac_input_observer_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.ref_count == 0 {
            let initial = query_macos_default_input_uid_now();
            *guard.latest_uid.lock().unwrap_or_else(|e| e.into_inner()) = initial;
            let stop = Arc::new(AtomicBool::new(false));
            let stop_bg = stop.clone();
            let latest_bg = guard.latest_uid.clone();
            let join = thread::spawn(move || {
                let dirty = Arc::new(AtomicBool::new(false));
                #[cfg(target_os = "macos")]
                let _listener_guard = {
                    struct ListenerCtx {
                        dirty: Arc<AtomicBool>,
                        wake: thread::Thread,
                    }
                    #[allow(unsafe_code)]
                    unsafe extern "C" fn on_default_in_changed(user_data: *mut std::ffi::c_void) {
                        if !user_data.is_null() {
                            // SAFETY: `user_data` points to `ListenerCtx` kept alive across `_listener`.
                            let ctx = unsafe { &*(user_data as *const ListenerCtx) };
                            ctx.dirty.store(true, SeqCst);
                            ctx.wake.unpark();
                        }
                    }
                    let ctx = Box::new(ListenerCtx {
                        dirty: dirty.clone(),
                        wake: thread::current(),
                    });
                    let ctx_ptr = &*ctx as *const ListenerCtx as *mut std::ffi::c_void;
                    let listener = crate::native_bridge::NativeDeviceListenerHandle::register(
                        on_default_in_changed,
                        ctx_ptr,
                    );
                    (listener, ctx)
                };

                while !stop_bg.load(SeqCst) {
                    if dirty.swap(false, SeqCst)
                        && let Some(uid) = query_macos_default_input_uid_now()
                    {
                        *latest_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(uid);
                    }
                    if !dirty.load(SeqCst) && !stop_bg.load(SeqCst) {
                        thread::park_timeout(Duration::from_millis(500));
                    }
                }
            });
            guard.worker = Some(WatcherWorker {
                stop,
                join: Some(join),
            });
        }
        guard.ref_count += 1;
        Self {
            latest_uid: guard.latest_uid.clone(),
        }
    }

    fn latest_endpoint_id(&self) -> Option<String> {
        self.latest_uid
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn changed_from(&self, expected_id: Option<&str>) -> Option<String> {
        let guard = self.latest_uid.lock().unwrap_or_else(|e| e.into_inner());
        let latest = guard.as_deref()?;
        if expected_id != Some(latest) {
            Some(latest.to_owned())
        } else {
            None
        }
    }
}

impl Drop for PlatformDefaultInputObserver {
    fn drop(&mut self) {
        let mut guard = mac_input_observer_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.ref_count = guard.ref_count.saturating_sub(1);
        if guard.ref_count == 0
            && let Some(mut worker) = guard.worker.take()
        {
            worker.stop_and_join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::discovery::{DiscoveryDedupe, DiscoveryOrder};

    #[test]
    fn backend_inventory_tracks_available_backends_filters_private_aggregates_and_honors_os_floors()
    {
        let mut proc_table = BTreeMap::new();
        proc_table.insert(
            100,
            BsdProcRecord {
                pid: 100,
                ppid: 1,
                start_time_us: 10_000,
                comm: "Safari".into(),
                exe_path: Some("/Applications/Safari.app/Contents/MacOS/Safari".into()),
            },
        );
        proc_table.insert(
            101,
            BsdProcRecord {
                pid: 101,
                ppid: 100,
                start_time_us: 10_050,
                comm: "Safari WebContent".into(),
                exe_path: Some(
                    "/Applications/Safari.app/Contents/Helpers/Safari WebContent".into(),
                ),
            },
        );
        proc_table.insert(
            200,
            BsdProcRecord {
                pid: 200,
                ppid: 1,
                start_time_us: 20_000,
                comm: "ffplay".into(),
                exe_path: Some("/opt/homebrew/bin/ffplay".into()),
            },
        );

        let ca_objs = vec![
            CoreAudioProcessObjectRecord {
                object_id: 501,
                pid: 101,
                start_time_us: 10_050,
                running_output: true,
                bundle_id: Some("com.apple.Safari.helper".into()),
            },
            CoreAudioProcessObjectRecord {
                object_id: 502,
                pid: 200,
                start_time_us: 20_000,
                running_output: true,
                bundle_id: None,
            },
        ];
        let running_apps = vec![(
            100,
            "com.apple.Safari".into(),
            "Safari".into(),
            "/Applications/Safari.app/Contents/MacOS/Safari".into(),
        )];
        let sck_apps = vec![SckShareableAppRecord {
            pid: 100,
            start_time_us: 10_000,
            bundle_id: "com.apple.Safari".into(),
            app_name: "Safari".into(),
        }];
        let out_devs = vec![
            (
                "BuiltInSpeakerDevice".into(),
                "MacBook Pro Speakers".into(),
                true,
            ),
            (
                "cspa-private-agg-999".into(),
                "CSPA Private Aggregate".into(),
                false,
            ),
        ];
        let in_devs = vec![
            (
                "BuiltInMicrophoneDevice".into(),
                "MacBook Pro Microphone".into(),
                true,
            ),
            (
                "com.apple.audio.CoreAudioTap-77".into(),
                "Private Tap".into(),
                false,
            ),
            (
                "CADefaultDeviceAggregate-123".into(),
                "Default Aggregate".into(),
                false,
            ),
        ];

        let inv_15 = collect_macos_backend_inventory_with_state(
            &proc_table,
            &ca_objs,
            &running_apps,
            &sck_apps,
            &out_devs,
            &in_devs,
            (15, 7, 9),
            true,
            &Backends::default(),
        );
        let q_default = DiscoveryQuery::all_for_os(PlatformOs::MacOs);
        let cat_15 = build_catalog_from_backend_inventory(&inv_15, &q_default, 1).unwrap();
        assert_eq!(cat_15.output_devices.len(), 1);
        assert_eq!(
            cat_15.output_devices[0].device_id().map(AsRef::as_ref),
            Some("BuiltInSpeakerDevice")
        );
        assert_eq!(cat_15.input_devices.len(), 1);
        assert_eq!(
            cat_15.input_devices[0].device_id().map(AsRef::as_ref),
            Some("BuiltInMicrophoneDevice")
        );
        assert_eq!(
            cat_15.input_devices[0].available_backends().as_ref(),
            &[BackendId::CoreAudio][..]
        );

        let safari_app = cat_15
            .applications
            .iter()
            .find(|a| a.pid() == Some(100))
            .expect("Safari application");
        assert_eq!(safari_app.active_audio(), Some(true));
        assert_eq!(
            safari_app.available_backends().as_ref(),
            &[BackendId::CoreAudio, BackendId::ScreenCaptureKit][..]
        );

        let ffplay_app = cat_15
            .applications
            .iter()
            .find(|a| a.pid() == Some(200))
            .expect("ffplay CLI application");
        assert_eq!(
            ffplay_app.available_backends().as_ref(),
            &[BackendId::CoreAudio][..]
        );

        let mut q_per = DiscoveryQuery::all_for_os(PlatformOs::MacOs);
        q_per.dedupe = DiscoveryDedupe::PerBackend;
        q_per.order = DiscoveryOrder::BackendThenKind;
        let cat_per = build_catalog_from_backend_inventory(&inv_15, &q_per, 2).unwrap();
        let sck_safari = cat_per
            .applications
            .iter()
            .find(|t| t.backend() == Some(BackendId::ScreenCaptureKit))
            .expect("SCK Safari");
        assert_eq!(sck_safari.scope(), ProcessScope::Exact);
        assert!(sck_safari.id().starts_with("screencapturekit:application:"));
        assert!(
            !cat_per
                .processes
                .iter()
                .any(|t| t.backend() == Some(BackendId::ScreenCaptureKit))
        );

        let inv_13 = collect_macos_backend_inventory_with_state(
            &proc_table,
            &ca_objs,
            &running_apps,
            &sck_apps,
            &out_devs,
            &in_devs,
            (13, 5, 0),
            true,
            &Backends::default(),
        );
        let cat_13 = build_catalog_from_backend_inventory(&inv_13, &q_default, 3).unwrap();
        assert_eq!(cat_13.output_devices.len(), 0);
        assert_eq!(cat_13.input_devices.len(), 1);
        assert_eq!(cat_13.applications.len(), 1);
        assert_eq!(cat_13.applications[0].active_audio(), Some(true));
        assert_eq!(
            cat_13.applications[0].available_backends().as_ref(),
            &[BackendId::ScreenCaptureKit][..]
        );
    }
}
