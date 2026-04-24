#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::core::{
    discovery::{
        BackendRawInventory, DeviceDirection, DiscoveryFilter, DiscoveryQuery, DiscoveryService,
        DiscoverySubscription, RawBackendDeviceRecord, RawBackendProcessRecord, RawProcessRecord,
        TargetCatalog,
    },
    schema::{BackendId, Backends, CaptureTarget, OnRootExit, PlatformOs, ProcessScope},
    traits::{CapabilityError, DefaultEndpointObserver, DiscoveryBackend},
};
use crate::loader::LinuxClientPaths;
use crate::procfs::{collect_proc_table, inspect_process_executable};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering::SeqCst},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PulseSinkRecord {
    pub index: u32,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PulseSinkInputRecord {
    pub index: u32,
    pub sink_index: u32,
    pub client_index: u32,
    pub pid: Option<u32>,
    pub rate_hz: u32,
    pub channels: u8,
    pub corked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeWireOutputNodeRecord {
    pub node_id: u32,
    pub client_id: Option<u32>,
    pub pid: Option<u32>,
    pub node_name: Option<String>,
    pub active: bool,
    pub fl_port_id: Option<u32>,
    pub fr_port_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PulseSourceRecord {
    pub index: u32,
    pub name: String,
    pub description: Option<String>,
    pub sample_format: i32,
    pub rate_hz: u32,
    pub channels: u8,
    pub is_monitor: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PipeWireGraphSnapshot {
    pub default_sink_name: Option<String>,
    pub default_source_name: Option<String>,
    pub sinks: Vec<(u32, String, Option<String>)>,
    pub sources: Vec<(u32, String, Option<String>)>,
    pub client_pids: BTreeMap<u32, u32>,
    pub output_nodes: Vec<PipeWireOutputNodeRecord>,
    pub link_pairs: Vec<(u32, u32, u32, u32, u32)>,
}

pub type PulseAudioStateSnapshot = (
    Vec<PulseSinkRecord>,
    BTreeMap<u32, bool>,
    Vec<PulseSinkInputRecord>,
);

pub type PulseAudioFullStateSnapshot = (
    Vec<PulseSinkRecord>,
    Vec<PulseSourceRecord>,
    BTreeMap<u32, bool>,
    Vec<PulseSinkInputRecord>,
);

#[cfg(any(target_os = "linux", test))]
pub(crate) fn is_internal_mixer_endpoint_name(name: &str) -> bool {
    name.starts_with("cspa-mix-")
}

/// Use the node or client `application.process.id`, never `pipewire.sec.pid`.
pub fn resolve_pipewire_node_pid(
    node_app_pid: Option<u32>,
    node_client_id: Option<u32>,
    client_app_pids: &BTreeMap<u32, u32>,
    _client_sec_pids_ignored: &BTreeMap<u32, u32>,
) -> Option<u32> {
    if let Some(pid) = node_app_pid.filter(|&p| p > 0) {
        return Some(pid);
    }
    if let Some(cid) = node_client_id
        && let Some(&pid) = client_app_pids.get(&cid)
        && pid > 0
    {
        return Some(pid);
    }
    None
}

/// Parse PipeWire `default` metadata property values (`{"name":"<sink_name>"}` SPA JSON or plain string).
pub fn parse_pipewire_default_sink_metadata(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.starts_with('{') {
        let plain = trimmed.trim_matches('"').trim();
        return (!plain.is_empty()).then(|| plain.to_owned());
    }
    for key_pat in ["\"name\"", "name"] {
        if let Some(pos) = trimmed.find(key_pat) {
            let after_key = &trimmed[pos + key_pat.len()..];
            if let Some(colon) = after_key.find(':') {
                let after_colon = after_key[colon + 1..].trim_start();
                if let Some(rest) = after_colon.strip_prefix('"') {
                    let mut out = String::new();
                    let mut escaped = false;
                    for ch in rest.chars() {
                        if escaped {
                            out.push(ch);
                            escaped = false;
                        } else if ch == '\\' {
                            escaped = true;
                        } else if ch == '"' {
                            return (!out.is_empty()).then_some(out);
                        } else {
                            out.push(ch);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    use crate::loader::{
        PA_CONTEXT_FAILED, PA_CONTEXT_NOAUTOSPAWN, PA_CONTEXT_READY, PA_CONTEXT_TERMINATED,
        PA_INVALID_INDEX, PaClientInfo, PaServerInfo, PaSinkInfo, PaSinkInputInfo, PaSourceInfo,
        PipeWireLibrary, PulseLibrary, PwClientEvents, PwClientInfo, PwCoreEvents, PwCoreInfo,
        PwNodeEvents, PwNodeInfo, PwRegistryEvents, SpaDict, SpaHook, probe_pulse_server,
    };
    use std::ffi::{CStr, c_char, c_int, c_void};

    #[repr(C)]
    struct PwMetadataEvents {
        version: u32,
        property: Option<
            unsafe extern "C" fn(
                data: *mut c_void,
                subject: u32,
                key: *const c_char,
                type_: *const c_char,
                value: *const c_char,
            ) -> c_int,
        >,
    }

    pub fn collect_pipewire_graph(
        paths: &LinuxClientPaths,
    ) -> Result<PipeWireGraphSnapshot, CapabilityError> {
        let lib = PipeWireLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PipeWire, paths.pipewire_min_version)
        })?;

        type RawNodeMap = BTreeMap<u32, (Option<u32>, Option<u32>, Option<String>, bool)>;

        struct BoundProxy {
            proxy: *mut c_void,
            _hook: Box<SpaHook>,
        }

        struct PwSnapState<'a> {
            lib: &'a PipeWireLibrary,
            core: *mut c_void,
            registry: *mut c_void,
            loop_ptr: *mut c_void,
            quit_fn: unsafe extern "C" fn(*mut c_void) -> c_int,
            sync_seq: c_int,
            phase: u8,
            failed: bool,
            metadata_default_sink: Option<String>,
            metadata_default_source: Option<String>,
            sinks: Vec<(u32, String, Option<String>)>,
            sources: Vec<(u32, String, Option<String>)>,
            client_app_pids: BTreeMap<u32, u32>,
            client_sec_pids: BTreeMap<u32, u32>,
            raw_nodes: RawNodeMap,
            fl_ports: BTreeMap<u32, u32>,
            fr_ports: BTreeMap<u32, u32>,
            links: Vec<(u32, u32, u32, u32, u32)>,
            bound_proxies: Vec<BoundProxy>,
        }

        static NODE_EVENTS: PwNodeEvents = PwNodeEvents {
            version: 0,
            info: Some(on_node_info),
            param: None,
        };

        static CLIENT_EVENTS: PwClientEvents = PwClientEvents {
            version: 0,
            info: Some(on_client_info),
            permissions: None,
        };

        static METADATA_EVENTS: PwMetadataEvents = PwMetadataEvents {
            version: 0,
            property: Some(on_metadata_property),
        };

        unsafe extern "C" fn on_metadata_property(
            data: *mut c_void,
            subject: u32,
            key: *const c_char,
            _type_: *const c_char,
            value: *const c_char,
        ) -> c_int {
            // SAFETY: `data` points to `PwSnapState` and `key`/`value` are valid during callback.
            unsafe {
                if subject != 0 || key.is_null() || value.is_null() {
                    return 0;
                }
                let Ok(key_str) = CStr::from_ptr(key).to_str() else {
                    return 0;
                };
                let is_sink_key =
                    key_str == "default.audio.sink" || key_str == "default.configured.audio.sink";
                let is_source_key = key_str == "default.audio.source"
                    || key_str == "default.configured.audio.source";
                if !is_sink_key && !is_source_key {
                    return 0;
                }
                let Ok(val_str) = CStr::from_ptr(value).to_str() else {
                    return 0;
                };
                if let Some(parsed) = parse_pipewire_default_sink_metadata(val_str) {
                    let st = &mut *(data as *mut PwSnapState<'_>);
                    if is_sink_key {
                        if key_str == "default.audio.sink" || st.metadata_default_sink.is_none() {
                            st.metadata_default_sink = Some(parsed);
                        }
                    } else if key_str == "default.audio.source"
                        || st.metadata_default_source.is_none()
                    {
                        st.metadata_default_source = Some(parsed);
                    }
                }
                0
            }
        }

        unsafe extern "C" fn on_node_info(data: *mut c_void, info: *const PwNodeInfo) {
            // SAFETY: `data` points to `PwSnapState` and `info` is valid during callback.
            unsafe {
                if info.is_null() {
                    return;
                }
                let st = &mut *(data as *mut PwSnapState<'_>);
                let inf = &*info;
                if inf.props.is_null() {
                    return;
                }
                let app_pid = SpaDict::lookup(inf.props, "application.process.id")
                    .and_then(|s| s.trim().parse::<u32>().ok());
                let client_id = SpaDict::lookup(inf.props, "client.id")
                    .and_then(|s| s.trim().parse::<u32>().ok());
                let app_name = SpaDict::lookup(inf.props, "application.name")
                    .or_else(|| SpaDict::lookup(inf.props, "node.name"));
                let pulse_corked = SpaDict::lookup(inf.props, "pulse.corked");
                if let Some(entry) = st.raw_nodes.get_mut(&inf.id) {
                    if app_pid.is_some() {
                        entry.1 = app_pid;
                    }
                    if client_id.is_some() {
                        entry.0 = client_id;
                    }
                    if app_name.is_some() {
                        entry.2 = app_name;
                    }
                    if let Some(corked_txt) = pulse_corked {
                        entry.3 = corked_txt.trim() != "true";
                    } else if inf.state == 2 {
                        entry.3 = false;
                    }
                }
            }
        }

        unsafe extern "C" fn on_client_info(data: *mut c_void, info: *const PwClientInfo) {
            // SAFETY: `data` points to `PwSnapState` and `info` is valid during callback.
            unsafe {
                if info.is_null() {
                    return;
                }
                let st = &mut *(data as *mut PwSnapState<'_>);
                let inf = &*info;
                if inf.props.is_null() {
                    return;
                }
                if let Some(pid_txt) = SpaDict::lookup(inf.props, "application.process.id")
                    && let Ok(pid) = pid_txt.trim().parse::<u32>()
                {
                    st.client_app_pids.insert(inf.id, pid);
                }
                if let Some(sec_txt) = SpaDict::lookup(inf.props, "pipewire.sec.pid")
                    && let Ok(sec_pid) = sec_txt.trim().parse::<u32>()
                {
                    st.client_sec_pids.insert(inf.id, sec_pid);
                }
            }
        }

        unsafe extern "C" fn on_core_done(data: *mut c_void, id: u32, seq: c_int) {
            // SAFETY: `data` points to `PwSnapState`.
            unsafe {
                let st = &mut *(data as *mut PwSnapState<'_>);
                if id == 0 && seq == st.sync_seq {
                    if st.phase == 0 {
                        st.phase = 1;
                        st.sync_seq = st.lib.core_sync(st.core, 0, 2);
                    } else {
                        (st.quit_fn)(st.loop_ptr);
                    }
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
            // SAFETY: `data` points to `PwSnapState`.
            unsafe {
                let st = &mut *(data as *mut PwSnapState<'_>);
                st.failed = true;
                (st.quit_fn)(st.loop_ptr);
            }
        }

        unsafe extern "C" fn on_global(
            data: *mut c_void,
            id: u32,
            _permissions: u32,
            type_ptr: *const c_char,
            _version: u32,
            props: *const SpaDict,
        ) {
            // SAFETY: `data` points to `PwSnapState` and `type_ptr`/`props` are valid during callback.
            unsafe {
                if type_ptr.is_null() || props.is_null() {
                    return;
                }
                let st = &mut *(data as *mut PwSnapState<'_>);
                let Ok(type_str) = CStr::from_ptr(type_ptr).to_str() else {
                    return;
                };
                match type_str {
                    "PipeWire:Interface:Client" => {
                        if let Some(pid_txt) = SpaDict::lookup(props, "application.process.id")
                            && let Ok(pid) = pid_txt.trim().parse::<u32>()
                        {
                            st.client_app_pids.insert(id, pid);
                        }
                        if let Some(sec_txt) = SpaDict::lookup(props, "pipewire.sec.pid")
                            && let Ok(sec_pid) = sec_txt.trim().parse::<u32>()
                        {
                            st.client_sec_pids.insert(id, sec_pid);
                        }
                        let proxy = st.lib.registry_bind(
                            st.registry,
                            id,
                            c"PipeWire:Interface:Client".as_ptr(),
                            3,
                            0,
                        );
                        if !proxy.is_null() {
                            let mut hook = Box::new(SpaHook::default());
                            (st.lib.pw_proxy_add_object_listener)(
                                proxy,
                                &mut *hook,
                                (&CLIENT_EVENTS as *const PwClientEvents).cast(),
                                data,
                            );
                            st.bound_proxies.push(BoundProxy { proxy, _hook: hook });
                        }
                    }
                    "PipeWire:Interface:Node" => {
                        let media_class = SpaDict::lookup(props, "media.class").unwrap_or_default();
                        let node_name = SpaDict::lookup(props, "node.name");
                        let node_desc = SpaDict::lookup(props, "node.description")
                            .or_else(|| SpaDict::lookup(props, "node.nick"));
                        if media_class == "Audio/Sink" {
                            if let Some(name) = node_name
                                && !is_internal_mixer_endpoint_name(&name)
                            {
                                st.sinks.push((id, name, node_desc));
                            }
                        } else if media_class == "Audio/Source"
                            || media_class == "Audio/Source/Virtual"
                        {
                            if let Some(name) = node_name
                                && !name.ends_with(".monitor")
                                && !is_internal_mixer_endpoint_name(&name)
                            {
                                st.sources.push((id, name, node_desc));
                            }
                        } else if media_class == "Stream/Output/Audio" {
                            let client_id = SpaDict::lookup(props, "client.id")
                                .and_then(|s| s.trim().parse::<u32>().ok());
                            let app_pid = SpaDict::lookup(props, "application.process.id")
                                .and_then(|s| s.trim().parse::<u32>().ok());
                            let app_or_node_name =
                                SpaDict::lookup(props, "application.name").or(node_name);
                            let is_corked = SpaDict::lookup(props, "pulse.corked")
                                .is_some_and(|v| v.trim() == "true");
                            st.raw_nodes
                                .insert(id, (client_id, app_pid, app_or_node_name, !is_corked));
                            let proxy = st.lib.registry_bind(
                                st.registry,
                                id,
                                c"PipeWire:Interface:Node".as_ptr(),
                                3,
                                0,
                            );
                            if !proxy.is_null() {
                                let mut hook = Box::new(SpaHook::default());
                                (st.lib.pw_proxy_add_object_listener)(
                                    proxy,
                                    &mut *hook,
                                    (&NODE_EVENTS as *const PwNodeEvents).cast(),
                                    data,
                                );
                                st.bound_proxies.push(BoundProxy { proxy, _hook: hook });
                            }
                        }
                    }
                    "PipeWire:Interface:Port" => {
                        let Some(node_id) = SpaDict::lookup(props, "node.id")
                            .and_then(|s| s.trim().parse::<u32>().ok())
                        else {
                            return;
                        };
                        let direction =
                            SpaDict::lookup(props, "port.direction").unwrap_or_default();
                        let channel = SpaDict::lookup(props, "audio.channel").unwrap_or_default();
                        if direction == "out" {
                            if channel == "FL" || channel == "MONO" {
                                st.fl_ports.insert(node_id, id);
                            } else if channel == "FR" {
                                st.fr_ports.insert(node_id, id);
                            }
                        }
                    }
                    "PipeWire:Interface:Link" => {
                        let out_node = SpaDict::lookup(props, "link.output.node")
                            .and_then(|s| s.trim().parse::<u32>().ok());
                        let out_port = SpaDict::lookup(props, "link.output.port")
                            .and_then(|s| s.trim().parse::<u32>().ok());
                        let in_node = SpaDict::lookup(props, "link.input.node")
                            .and_then(|s| s.trim().parse::<u32>().ok());
                        let in_port = SpaDict::lookup(props, "link.input.port")
                            .and_then(|s| s.trim().parse::<u32>().ok());
                        if let (Some(on), Some(op), Some(inn), Some(ip)) =
                            (out_node, out_port, in_node, in_port)
                        {
                            st.links.push((id, on, op, inn, ip));
                        }
                    }
                    "PipeWire:Interface:Metadata"
                        if SpaDict::lookup(props, "metadata.name").as_deref()
                            == Some("default") =>
                    {
                        let proxy = st.lib.registry_bind(
                            st.registry,
                            id,
                            c"PipeWire:Interface:Metadata".as_ptr(),
                            3,
                            0,
                        );
                        if !proxy.is_null() {
                            let mut hook = Box::new(SpaHook::default());
                            (st.lib.pw_proxy_add_object_listener)(
                                proxy,
                                &mut *hook,
                                (&METADATA_EVENTS as *const PwMetadataEvents).cast(),
                                data,
                            );
                            st.bound_proxies.push(BoundProxy { proxy, _hook: hook });
                        }
                    }
                    _ => {}
                }
            }
        }

        // SAFETY: creates, runs, and destroys a single-thread `pw_main_loop` snapshot.
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
            let registry = lib.core_get_registry(core, 3, 0);
            if registry.is_null() {
                (lib.pw_core_disconnect)(core);
                (lib.pw_context_destroy)(context);
                (lib.pw_main_loop_destroy)(main_loop);
                return Err(CapabilityError::ServerUnavailable);
            }

            let mut state = PwSnapState {
                lib: &lib,
                core,
                registry,
                loop_ptr: main_loop,
                quit_fn: lib.pw_main_loop_quit,
                sync_seq: 0,
                phase: 0,
                failed: false,
                metadata_default_sink: None,
                metadata_default_source: None,
                sinks: Vec::new(),
                sources: Vec::new(),
                client_app_pids: BTreeMap::new(),
                client_sec_pids: BTreeMap::new(),
                raw_nodes: BTreeMap::new(),
                fl_ports: BTreeMap::new(),
                fr_ports: BTreeMap::new(),
                links: Vec::new(),
                bound_proxies: Vec::new(),
            };
            let core_events = PwCoreEvents {
                version: 0,
                info: None::<unsafe extern "C" fn(*mut c_void, *const PwCoreInfo)>,
                done: Some(on_core_done),
                ping: None,
                error: Some(on_core_error),
                remove_id: None,
                bound_id: None,
                add_mem: None,
                remove_mem: None,
                bound_props: None,
            };
            let reg_events = PwRegistryEvents {
                version: 0,
                global: Some(on_global),
                global_remove: None,
            };
            let mut core_hook = SpaHook::default();
            let mut reg_hook = SpaHook::default();
            let state_ptr: *mut c_void = (&mut state as *mut PwSnapState<'_>).cast();
            lib.core_add_listener(core, &mut core_hook, &core_events, state_ptr);
            lib.registry_add_listener(registry, &mut reg_hook, &reg_events, state_ptr);
            state.sync_seq = lib.core_sync(core, 0, 1);
            (lib.pw_main_loop_run)(main_loop);

            for bp in state.bound_proxies {
                if !bp.proxy.is_null() {
                    (lib.pw_proxy_destroy)(bp.proxy);
                }
            }
            (lib.pw_proxy_destroy)(registry);
            (lib.pw_core_disconnect)(core);
            (lib.pw_context_destroy)(context);
            (lib.pw_main_loop_destroy)(main_loop);

            if state.failed {
                return Err(CapabilityError::ServerUnavailable);
            }

            let pa_probe = probe_pulse_server(paths).ok();
            let default_sink_name = state
                .metadata_default_sink
                .filter(|meta_name| state.sinks.iter().any(|(_, n, _)| n == meta_name))
                .or_else(|| {
                    pa_probe
                        .as_ref()
                        .and_then(|p| p.default_sink_name.clone())
                        .filter(|pa_name| state.sinks.iter().any(|(_, n, _)| n == pa_name))
                })
                .or_else(|| state.sinks.first().map(|(_, name, _)| name.clone()));
            let default_source_name = state
                .metadata_default_source
                .filter(|meta_name| state.sources.iter().any(|(_, n, _)| n == meta_name))
                .or_else(|| {
                    pa_probe
                        .and_then(|p| p.default_source_name)
                        .filter(|pa_name| state.sources.iter().any(|(_, n, _)| n == pa_name))
                })
                .or_else(|| state.sources.first().map(|(_, name, _)| name.clone()));
            let mut output_nodes = Vec::new();
            for (node_id, (client_id, node_app_pid, node_name, active)) in state.raw_nodes {
                let resolved_pid = resolve_pipewire_node_pid(
                    node_app_pid,
                    client_id,
                    &state.client_app_pids,
                    &state.client_sec_pids,
                );
                output_nodes.push(PipeWireOutputNodeRecord {
                    node_id,
                    client_id,
                    pid: resolved_pid,
                    node_name,
                    active,
                    fl_port_id: state.fl_ports.get(&node_id).copied(),
                    fr_port_id: state.fr_ports.get(&node_id).copied(),
                });
            }

            Ok(PipeWireGraphSnapshot {
                default_sink_name,
                default_source_name,
                sinks: state.sinks,
                sources: state.sources,
                client_pids: state.client_app_pids,
                output_nodes,
                link_pairs: state.links,
            })
        }
    }

    pub fn query_pipewire_default_sink_only(
        paths: &LinuxClientPaths,
    ) -> Result<Option<String>, CapabilityError> {
        query_pipewire_defaults_only(paths).map(|(sink, _)| sink)
    }

    pub fn query_pipewire_defaults_only(
        paths: &LinuxClientPaths,
    ) -> Result<(Option<String>, Option<String>), CapabilityError> {
        let lib = PipeWireLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PipeWire, paths.pipewire_min_version)
        })?;

        struct BoundProxy {
            proxy: *mut c_void,
            _hook: Box<SpaHook>,
        }

        struct PwDefaultsOnlyState<'a> {
            lib: &'a PipeWireLibrary,
            core: *mut c_void,
            registry: *mut c_void,
            loop_ptr: *mut c_void,
            quit_fn: unsafe extern "C" fn(*mut c_void) -> c_int,
            sync_seq: c_int,
            phase: u8,
            failed: bool,
            metadata_default_sink: Option<String>,
            metadata_default_source: Option<String>,
            sinks: Vec<String>,
            sources: Vec<String>,
            bound_proxies: Vec<BoundProxy>,
        }

        static METADATA_EVENTS: PwMetadataEvents = PwMetadataEvents {
            version: 0,
            property: Some(on_metadata_property),
        };

        unsafe extern "C" fn on_metadata_property(
            data: *mut c_void,
            subject: u32,
            key: *const c_char,
            _type_: *const c_char,
            value: *const c_char,
        ) -> c_int {
            // SAFETY: `data` points to `PwDefaultsOnlyState` and `key`/`value` are valid during callback.
            unsafe {
                if subject != 0 || key.is_null() || value.is_null() {
                    return 0;
                }
                let Ok(key_str) = CStr::from_ptr(key).to_str() else {
                    return 0;
                };
                let is_sink_key =
                    key_str == "default.audio.sink" || key_str == "default.configured.audio.sink";
                let is_source_key = key_str == "default.audio.source"
                    || key_str == "default.configured.audio.source";
                if !is_sink_key && !is_source_key {
                    return 0;
                }
                let Ok(val_str) = CStr::from_ptr(value).to_str() else {
                    return 0;
                };
                if let Some(parsed) = parse_pipewire_default_sink_metadata(val_str) {
                    let st = &mut *(data as *mut PwDefaultsOnlyState<'_>);
                    if is_sink_key {
                        if key_str == "default.audio.sink" || st.metadata_default_sink.is_none() {
                            st.metadata_default_sink = Some(parsed);
                        }
                    } else if key_str == "default.audio.source"
                        || st.metadata_default_source.is_none()
                    {
                        st.metadata_default_source = Some(parsed);
                    }
                }
                0
            }
        }

        unsafe extern "C" fn on_core_done(data: *mut c_void, id: u32, seq: c_int) {
            // SAFETY: `data` points to `PwDefaultsOnlyState`.
            unsafe {
                let st = &mut *(data as *mut PwDefaultsOnlyState<'_>);
                if id == 0 && seq == st.sync_seq {
                    if st.phase == 0 && !st.bound_proxies.is_empty() {
                        st.phase = 1;
                        st.sync_seq = st.lib.core_sync(st.core, 0, 2);
                    } else {
                        (st.quit_fn)(st.loop_ptr);
                    }
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
            // SAFETY: `data` points to `PwDefaultsOnlyState`.
            unsafe {
                let st = &mut *(data as *mut PwDefaultsOnlyState<'_>);
                st.failed = true;
                (st.quit_fn)(st.loop_ptr);
            }
        }

        unsafe extern "C" fn on_global(
            data: *mut c_void,
            id: u32,
            _permissions: u32,
            type_ptr: *const c_char,
            _version: u32,
            props: *const SpaDict,
        ) {
            // SAFETY: `data` points to `PwDefaultsOnlyState` and `type_ptr`/`props` are valid during callback.
            unsafe {
                if type_ptr.is_null() || props.is_null() {
                    return;
                }
                let st = &mut *(data as *mut PwDefaultsOnlyState<'_>);
                let Ok(type_str) = CStr::from_ptr(type_ptr).to_str() else {
                    return;
                };
                match type_str {
                    "PipeWire:Interface:Node" => {
                        let media_class = SpaDict::lookup(props, "media.class").unwrap_or_default();
                        if media_class == "Audio/Sink" {
                            if let Some(name) = SpaDict::lookup(props, "node.name")
                                && !is_internal_mixer_endpoint_name(&name)
                            {
                                st.sinks.push(name);
                            }
                        } else if (media_class == "Audio/Source"
                            || media_class == "Audio/Source/Virtual")
                            && let Some(name) = SpaDict::lookup(props, "node.name")
                            && !name.ends_with(".monitor")
                            && !is_internal_mixer_endpoint_name(&name)
                        {
                            st.sources.push(name);
                        }
                    }
                    "PipeWire:Interface:Metadata"
                        if SpaDict::lookup(props, "metadata.name").as_deref()
                            == Some("default") =>
                    {
                        let proxy = st.lib.registry_bind(
                            st.registry,
                            id,
                            c"PipeWire:Interface:Metadata".as_ptr(),
                            3,
                            0,
                        );
                        if !proxy.is_null() {
                            let mut hook = Box::new(SpaHook::default());
                            (st.lib.pw_proxy_add_object_listener)(
                                proxy,
                                &mut *hook,
                                (&METADATA_EVENTS as *const PwMetadataEvents).cast(),
                                data,
                            );
                            st.bound_proxies.push(BoundProxy { proxy, _hook: hook });
                        }
                    }
                    _ => {}
                }
            }
        }

        // SAFETY: creates, runs, and destroys a single-thread `pw_main_loop` metadata snapshot.
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
            let registry = lib.core_get_registry(core, 3, 0);
            if registry.is_null() {
                (lib.pw_core_disconnect)(core);
                (lib.pw_context_destroy)(context);
                (lib.pw_main_loop_destroy)(main_loop);
                return Err(CapabilityError::ServerUnavailable);
            }

            let mut state = PwDefaultsOnlyState {
                lib: &lib,
                core,
                registry,
                loop_ptr: main_loop,
                quit_fn: lib.pw_main_loop_quit,
                sync_seq: 0,
                phase: 0,
                failed: false,
                metadata_default_sink: None,
                metadata_default_source: None,
                sinks: Vec::new(),
                sources: Vec::new(),
                bound_proxies: Vec::new(),
            };
            let core_events = PwCoreEvents {
                version: 0,
                info: None::<unsafe extern "C" fn(*mut c_void, *const PwCoreInfo)>,
                done: Some(on_core_done),
                ping: None,
                error: Some(on_core_error),
                remove_id: None,
                bound_id: None,
                add_mem: None,
                remove_mem: None,
                bound_props: None,
            };
            let reg_events = PwRegistryEvents {
                version: 0,
                global: Some(on_global),
                global_remove: None,
            };
            let mut core_hook = SpaHook::default();
            let mut reg_hook = SpaHook::default();
            let state_ptr: *mut c_void = (&mut state as *mut PwDefaultsOnlyState<'_>).cast();
            lib.core_add_listener(core, &mut core_hook, &core_events, state_ptr);
            lib.registry_add_listener(registry, &mut reg_hook, &reg_events, state_ptr);
            state.sync_seq = lib.core_sync(core, 0, 1);
            (lib.pw_main_loop_run)(main_loop);

            for bp in state.bound_proxies {
                if !bp.proxy.is_null() {
                    (lib.pw_proxy_destroy)(bp.proxy);
                }
            }
            (lib.pw_proxy_destroy)(registry);
            (lib.pw_core_disconnect)(core);
            (lib.pw_context_destroy)(context);
            (lib.pw_main_loop_destroy)(main_loop);

            if state.failed {
                return Err(CapabilityError::ServerUnavailable);
            }

            let pa_probe = probe_pulse_server(paths).ok();
            let default_sink_name = state
                .metadata_default_sink
                .filter(|meta_name| state.sinks.iter().any(|n| n == meta_name))
                .or_else(|| {
                    pa_probe
                        .as_ref()
                        .and_then(|p| p.default_sink_name.clone())
                        .filter(|pa_name| state.sinks.iter().any(|n| n == pa_name))
                })
                .or_else(|| state.sinks.first().cloned());
            let default_source_name = state
                .metadata_default_source
                .filter(|meta_name| state.sources.iter().any(|n| n == meta_name))
                .or_else(|| {
                    pa_probe
                        .and_then(|p| p.default_source_name)
                        .filter(|pa_name| state.sources.iter().any(|n| n == pa_name))
                })
                .or_else(|| state.sources.first().cloned());

            Ok((default_sink_name, default_source_name))
        }
    }

    pub fn collect_pulse_state(
        paths: &LinuxClientPaths,
    ) -> Result<PulseAudioStateSnapshot, CapabilityError> {
        let (devs, _sources, pids, sink_inputs) = collect_pulse_full_state(paths)?;
        Ok((devs, pids, sink_inputs))
    }

    pub fn collect_pulse_full_state(
        paths: &LinuxClientPaths,
    ) -> Result<PulseAudioFullStateSnapshot, CapabilityError> {
        let lib = PulseLibrary::load(paths).map_err(|err| {
            err.into_capability_error(BackendId::PulseAudio, paths.pulse_min_version)
        })?;

        struct PulseState {
            mainloop: *mut c_void,
            signal_fn: unsafe extern "C" fn(*mut c_void, c_int),
            proplist_gets: unsafe extern "C" fn(*const c_void, *const c_char) -> *const c_char,
            default_sink: Option<String>,
            default_source: Option<String>,
            sinks: Vec<(u32, String, Option<String>)>,
            sources: Vec<PulseSourceRecord>,
            client_pids: BTreeMap<u32, u32>,
            sink_inputs: Vec<PulseSinkInputRecord>,
            pending_ops: std::sync::atomic::AtomicUsize,
        }

        unsafe extern "C" fn on_ctx_state(_ctx: *mut c_void, userdata: *mut c_void) {
            // SAFETY: `userdata` points to `PulseState`.
            unsafe {
                let st = &*(userdata as *const PulseState);
                (st.signal_fn)(st.mainloop, 0);
            }
        }

        unsafe extern "C" fn on_server_info(
            _ctx: *mut c_void,
            info: *const PaServerInfo,
            userdata: *mut c_void,
        ) {
            // SAFETY: `userdata` points to `PulseState`.
            unsafe {
                let st = &mut *(userdata as *mut PulseState);
                if !info.is_null() {
                    let inf = &*info;
                    if !inf.default_sink_name.is_null() {
                        st.default_sink = Some(
                            CStr::from_ptr(inf.default_sink_name)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                    if !inf.default_source_name.is_null() {
                        st.default_source = Some(
                            CStr::from_ptr(inf.default_source_name)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
                st.pending_ops
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                (st.signal_fn)(st.mainloop, 0);
            }
        }

        unsafe extern "C" fn on_sink_info(
            _ctx: *mut c_void,
            info: *const PaSinkInfo,
            eol: c_int,
            userdata: *mut c_void,
        ) {
            // SAFETY: `userdata` points to `PulseState`.
            unsafe {
                let st = &mut *(userdata as *mut PulseState);
                if eol != 0 {
                    st.pending_ops
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    (st.signal_fn)(st.mainloop, 0);
                    return;
                }
                if !info.is_null() {
                    let inf = &*info;
                    if !inf.name.is_null() {
                        let name = CStr::from_ptr(inf.name).to_string_lossy().into_owned();
                        if !is_internal_mixer_endpoint_name(&name) {
                            let desc = if inf.description.is_null() {
                                None
                            } else {
                                Some(
                                    CStr::from_ptr(inf.description)
                                        .to_string_lossy()
                                        .into_owned(),
                                )
                            };
                            st.sinks.push((inf.index, name, desc));
                        }
                    }
                }
            }
        }

        unsafe extern "C" fn on_source_info(
            _ctx: *mut c_void,
            info: *const PaSourceInfo,
            eol: c_int,
            userdata: *mut c_void,
        ) {
            // SAFETY: `userdata` points to `PulseState`.
            unsafe {
                let st = &mut *(userdata as *mut PulseState);
                if eol != 0 {
                    st.pending_ops
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    (st.signal_fn)(st.mainloop, 0);
                    return;
                }
                if !info.is_null() {
                    let inf = &*info;
                    if !inf.name.is_null() {
                        let name = CStr::from_ptr(inf.name).to_string_lossy().into_owned();
                        if !is_internal_mixer_endpoint_name(&name) {
                            let desc = if inf.description.is_null() {
                                None
                            } else {
                                Some(
                                    CStr::from_ptr(inf.description)
                                        .to_string_lossy()
                                        .into_owned(),
                                )
                            };
                            let is_monitor = inf.monitor_of_sink != PA_INVALID_INDEX
                                || name.ends_with(".monitor");
                            st.sources.push(PulseSourceRecord {
                                index: inf.index,
                                name,
                                description: desc,
                                sample_format: inf.sample_spec.format,
                                rate_hz: inf.sample_spec.rate,
                                channels: inf.sample_spec.channels,
                                is_monitor,
                                is_default: false,
                            });
                        }
                    }
                }
            }
        }

        unsafe extern "C" fn on_client_info(
            _ctx: *mut c_void,
            info: *const PaClientInfo,
            eol: c_int,
            userdata: *mut c_void,
        ) {
            // SAFETY: `userdata` points to `PulseState`.
            unsafe {
                let st = &mut *(userdata as *mut PulseState);
                if eol != 0 {
                    st.pending_ops
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    (st.signal_fn)(st.mainloop, 0);
                    return;
                }
                if !info.is_null() {
                    let inf = &*info;
                    if !inf.proplist.is_null() {
                        let pid_ptr =
                            (st.proplist_gets)(inf.proplist, c"application.process.id".as_ptr());
                        if !pid_ptr.is_null()
                            && let Ok(pid_txt) = CStr::from_ptr(pid_ptr).to_str()
                            && let Ok(pid) = pid_txt.trim().parse::<u32>()
                        {
                            st.client_pids.insert(inf.index, pid);
                        }
                    }
                }
            }
        }

        unsafe extern "C" fn on_sink_input_info(
            _ctx: *mut c_void,
            info: *const PaSinkInputInfo,
            eol: c_int,
            userdata: *mut c_void,
        ) {
            // SAFETY: `userdata` points to `PulseState`.
            unsafe {
                let st = &mut *(userdata as *mut PulseState);
                if eol != 0 {
                    st.pending_ops
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    (st.signal_fn)(st.mainloop, 0);
                    return;
                }
                if !info.is_null() {
                    let inf = &*info;
                    let mut pid = None;
                    if !inf.proplist.is_null() {
                        let pid_ptr =
                            (st.proplist_gets)(inf.proplist, c"application.process.id".as_ptr());
                        if !pid_ptr.is_null()
                            && let Ok(pid_txt) = CStr::from_ptr(pid_ptr).to_str()
                        {
                            pid = pid_txt.trim().parse::<u32>().ok();
                        }
                    }
                    st.sink_inputs.push(PulseSinkInputRecord {
                        index: inf.index,
                        sink_index: inf.sink,
                        client_index: inf.client,
                        pid,
                        rate_hz: inf.sample_spec.rate,
                        channels: inf.sample_spec.channels,
                        corked: inf.corked != 0,
                    });
                }
            }
        }

        // SAFETY: `pa_threaded_mainloop` and `pa_context` follow standard locking and unref rules.
        unsafe {
            let mainloop = (lib.pa_threaded_mainloop_new)();
            if mainloop.is_null() {
                return Err(CapabilityError::ServerUnavailable);
            }
            let api = (lib.pa_threaded_mainloop_get_api)(mainloop);
            let ctx = (lib.pa_context_new)(api, c"cspa-discovery".as_ptr());
            if ctx.is_null() {
                (lib.pa_threaded_mainloop_free)(mainloop);
                return Err(CapabilityError::ServerUnavailable);
            }
            let mut st = Box::new(PulseState {
                mainloop,
                signal_fn: lib.pa_threaded_mainloop_signal,
                proplist_gets: lib.pa_proplist_gets,
                default_sink: None,
                default_source: None,
                sinks: Vec::new(),
                sources: Vec::new(),
                client_pids: BTreeMap::new(),
                sink_inputs: Vec::new(),
                pending_ops: std::sync::atomic::AtomicUsize::new(5),
            });
            let st_ptr: *mut c_void = (&mut *st as *mut PulseState).cast();
            (lib.pa_context_set_state_callback)(ctx, Some(on_ctx_state), st_ptr);
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
            let op1 = (lib.pa_context_get_server_info)(ctx, Some(on_server_info), st_ptr);
            let op2 = (lib.pa_context_get_sink_info_list)(ctx, Some(on_sink_info), st_ptr);
            let op3 = (lib.pa_context_get_source_info_list)(ctx, Some(on_source_info), st_ptr);
            let op4 = (lib.pa_context_get_client_info_list)(ctx, Some(on_client_info), st_ptr);
            let op5 =
                (lib.pa_context_get_sink_input_info_list)(ctx, Some(on_sink_input_info), st_ptr);
            while st.pending_ops.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                (lib.pa_threaded_mainloop_wait)(mainloop);
            }
            for op in [op1, op2, op3, op4, op5] {
                if !op.is_null() {
                    (lib.pa_operation_unref)(op);
                }
            }
            (lib.pa_context_set_state_callback)(ctx, None, std::ptr::null_mut());
            (lib.pa_context_disconnect)(ctx);
            (lib.pa_threaded_mainloop_unlock)(mainloop);
            (lib.pa_threaded_mainloop_stop)(mainloop);
            (lib.pa_context_unref)(ctx);
            (lib.pa_threaded_mainloop_free)(mainloop);

            let mut devices = Vec::new();
            for (idx, name, desc) in &st.sinks {
                let is_default = st.default_sink.as_deref() == Some(name.as_str());
                devices.push(PulseSinkRecord {
                    index: *idx,
                    name: name.clone(),
                    description: desc.clone(),
                    is_default,
                });
            }

            let mut any_default_src = false;
            for src in &mut st.sources {
                if !src.is_monitor && st.default_source.as_deref() == Some(src.name.as_str()) {
                    src.is_default = true;
                    any_default_src = true;
                }
            }
            if !any_default_src
                && let Some(first_non_mon) = st.sources.iter_mut().find(|s| !s.is_monitor)
            {
                first_non_mon.is_default = true;
            }

            let mut pid_audio: BTreeMap<u32, bool> = BTreeMap::new();
            for input in &mut st.sink_inputs {
                if input.pid.is_none()
                    && let Some(&client_pid) = st.client_pids.get(&input.client_index)
                {
                    input.pid = Some(client_pid);
                }
                if let Some(pid) = input.pid.filter(|&p| p > 0) {
                    let entry = pid_audio.entry(pid).or_insert(false);
                    if !input.corked {
                        *entry = true;
                    }
                }
            }

            Ok((devices, st.sources, pid_audio, st.sink_inputs))
        }
    }

    pub fn spawn_pulse_server_event_listener(
        paths: &LinuxClientPaths,
        latest_sink: Arc<Mutex<Option<String>>>,
        dirty: Arc<AtomicBool>,
        wake: thread::Thread,
        stop: Arc<AtomicBool>,
        include_stream_events: bool,
    ) -> Option<JoinHandle<()>> {
        spawn_pulse_server_event_listener_ext(
            paths,
            Some(latest_sink),
            None,
            dirty,
            wake,
            stop,
            include_stream_events,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_pulse_server_event_listener_ext(
        paths: &LinuxClientPaths,
        latest_sink: Option<Arc<Mutex<Option<String>>>>,
        latest_source: Option<Arc<Mutex<Option<String>>>>,
        dirty: Arc<AtomicBool>,
        wake: thread::Thread,
        stop: Arc<AtomicBool>,
        include_stream_events: bool,
        watch_sources: bool,
    ) -> Option<JoinHandle<()>> {
        let lib = PulseLibrary::load(paths).ok()?;
        Some(thread::spawn(move || {
            const PA_SUB_MASK_SINK_AND_SERVER: c_int = 0x0081;
            const PA_SUB_MASK_SOURCE_AND_SERVER: c_int = 0x0082;
            const PA_SUB_MASK_DISCOVERY: c_int = 0x00A7;

            struct SubState {
                mainloop: *mut c_void,
                signal_fn: unsafe extern "C" fn(*mut c_void, c_int),
                get_server_info: unsafe extern "C" fn(
                    *mut c_void,
                    Option<unsafe extern "C" fn(*mut c_void, *const PaServerInfo, *mut c_void)>,
                    *mut c_void,
                ) -> *mut c_void,
                op_unref: unsafe extern "C" fn(*mut c_void),
                latest_sink: Option<Arc<Mutex<Option<String>>>>,
                latest_source: Option<Arc<Mutex<Option<String>>>>,
                dirty: Arc<AtomicBool>,
                wake: thread::Thread,
            }

            unsafe extern "C" fn on_state(_ctx: *mut c_void, userdata: *mut c_void) {
                if !userdata.is_null() {
                    // SAFETY: `userdata` points to `SubState`.
                    unsafe {
                        let st = &*(userdata as *const SubState);
                        (st.signal_fn)(st.mainloop, 0);
                    }
                }
            }

            unsafe extern "C" fn on_server_info(
                _ctx: *mut c_void,
                info: *const PaServerInfo,
                userdata: *mut c_void,
            ) {
                if userdata.is_null() {
                    return;
                }
                // SAFETY: `userdata` points to `SubState` and `info` is valid during callback.
                unsafe {
                    let st = &*(userdata as *const SubState);
                    if !info.is_null() {
                        let inf = &*info;
                        if let Some(ref sink_cell) = st.latest_sink
                            && !inf.default_sink_name.is_null()
                        {
                            let sink = CStr::from_ptr(inf.default_sink_name)
                                .to_string_lossy()
                                .into_owned();
                            if !sink.is_empty() {
                                *sink_cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
                            }
                        }
                        if let Some(ref src_cell) = st.latest_source
                            && !inf.default_source_name.is_null()
                        {
                            let src = CStr::from_ptr(inf.default_source_name)
                                .to_string_lossy()
                                .into_owned();
                            if !src.is_empty() && !src.ends_with(".monitor") {
                                *src_cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(src);
                            }
                        }
                    }
                    st.dirty.store(true, SeqCst);
                    st.wake.unpark();
                    (st.signal_fn)(st.mainloop, 0);
                }
            }

            unsafe extern "C" fn on_subscribe(
                ctx: *mut c_void,
                event_type: c_int,
                _idx: u32,
                userdata: *mut c_void,
            ) {
                if userdata.is_null() || ctx.is_null() {
                    return;
                }
                // SAFETY: `userdata` points to `SubState`; queries updated server info on sink/source/server event.
                unsafe {
                    let st = &*(userdata as *const SubState);
                    st.dirty.store(true, SeqCst);
                    let facility = event_type & 0x000F;
                    if facility == 0x0000 || facility == 0x0001 || facility == 0x0007 {
                        let op = (st.get_server_info)(ctx, Some(on_server_info), userdata);
                        if !op.is_null() {
                            (st.op_unref)(op);
                        }
                    }
                    st.wake.unpark();
                }
            }

