use crate::{
    CapacityError,
    manager::Visibility,
    schema::{
        BackendId, Backends, CaptureTarget, OnRootExit, PlanError, PlatformOs, ProcessIdentity,
        ProcessScope, TargetKind, extract_backend_id_prefix, strip_backend_id_prefix,
    },
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering::SeqCst},
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryFilter {
    pub active_only: bool,
    pub include_non_audio: bool,
    pub include_processes: bool,
    pub include_applications: bool,
    pub include_devices: bool,
    pub include_input_devices: bool,
}

impl DiscoveryFilter {
    pub fn applications(active_only: bool, include_non_audio: bool) -> Self {
        Self {
            active_only,
            include_non_audio,
            include_processes: false,
            include_applications: true,
            include_devices: false,
            include_input_devices: false,
        }
    }

    pub fn processes(active_only: bool, include_non_audio: bool) -> Self {
        Self {
            active_only,
            include_non_audio,
            include_processes: true,
            include_applications: false,
            include_devices: false,
            include_input_devices: false,
        }
    }

    pub fn output_devices() -> Self {
        Self {
            active_only: false,
            include_non_audio: false,
            include_processes: false,
            include_applications: false,
            include_devices: true,
            include_input_devices: false,
        }
    }

    pub fn input_devices() -> Self {
        Self {
            active_only: false,
            include_non_audio: false,
            include_processes: false,
            include_applications: false,
            include_devices: false,
            include_input_devices: true,
        }
    }

    pub fn all(active_only: bool, include_non_audio: bool) -> Self {
        Self {
            active_only,
            include_non_audio,
            include_processes: true,
            include_applications: true,
            include_devices: true,
            include_input_devices: true,
        }
    }

    pub fn matches(&self, target: &CaptureTarget) -> bool {
        match target.kind {
            TargetKind::Application => {
                if !self.include_applications {
                    return false;
                }
                self.matches_audio_state(target.active_audio)
            }
            TargetKind::Process => {
                if !self.include_processes {
                    return false;
                }
                self.matches_audio_state(target.active_audio)
            }
            TargetKind::Device | TargetKind::DefaultOutput => self.include_devices,
            TargetKind::InputDevice | TargetKind::DefaultInput => self.include_input_devices,
        }
    }

    fn matches_audio_state(&self, active_audio: Option<bool>) -> bool {
        if self.active_only {
            active_audio == Some(true)
        } else if self.include_non_audio {
            true
        } else {
            active_audio.is_some()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeviceDirection {
    #[default]
    Output,
    Input,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiscoveryDedupe {
    #[default]
    ByLogicalTarget,
    PerBackend,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiscoveryOrder {
    #[default]
    KindThenBackend,
    BackendThenKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryQuery {
    pub os: PlatformOs,
    pub backends: Backends,
    pub dedupe: DiscoveryDedupe,
    pub order: DiscoveryOrder,
    pub active_only: bool,
    pub include_non_audio: bool,
    pub include_applications: bool,
    pub include_processes: bool,
    pub include_output_devices: bool,
    pub include_input_devices: bool,
}

impl Default for DiscoveryQuery {
    fn default() -> Self {
        Self {
            os: PlatformOs::current(),
            backends: Backends::default(),
            dedupe: DiscoveryDedupe::ByLogicalTarget,
            order: DiscoveryOrder::KindThenBackend,
            active_only: false,
            include_non_audio: false,
            include_applications: true,
            include_processes: true,
            include_output_devices: true,
            include_input_devices: true,
        }
    }
}

impl DiscoveryQuery {
    pub fn all_for_os(os: PlatformOs) -> Self {
        Self {
            os,
            ..Self::default()
        }
    }

    pub fn from_filter_for_os(os: PlatformOs, filter: &DiscoveryFilter) -> Self {
        Self {
            os,
            backends: Backends::default(),
            dedupe: DiscoveryDedupe::ByLogicalTarget,
            order: DiscoveryOrder::KindThenBackend,
            active_only: filter.active_only,
            include_non_audio: filter.include_non_audio,
            include_applications: filter.include_applications,
            include_processes: filter.include_processes,
            include_output_devices: filter.include_devices,
            include_input_devices: filter.include_input_devices,
        }
    }

    pub fn as_filter(&self) -> DiscoveryFilter {
        DiscoveryFilter {
            active_only: self.active_only,
            include_non_audio: self.include_non_audio,
            include_processes: self.include_processes,
            include_applications: self.include_applications,
            include_devices: self.include_output_devices,
            include_input_devices: self.include_input_devices,
        }
    }

    pub fn matches_target(&self, target: &CaptureTarget) -> bool {
        let filter = self.as_filter();
        if !filter.matches(target) {
            return false;
        }
        let active_backends = self.backends.for_os(self.os);
        match self.dedupe {
            DiscoveryDedupe::ByLogicalTarget => {
                if target.backend.is_some() {
                    return false;
                }
                target.available_backends.is_empty()
                    || target
                        .available_backends
                        .iter()
                        .any(|b| active_backends.contains(b))
            }
            DiscoveryDedupe::PerBackend => {
                target.backend.is_some_and(|b| active_backends.contains(&b))
            }
        }
    }

    pub fn with_backends(mut self, backends: Backends) -> Self {
        self.backends = backends;
        self
    }

    pub fn with_dedupe(mut self, dedupe: DiscoveryDedupe) -> Self {
        self.dedupe = dedupe;
        self
    }

    pub fn with_order(mut self, order: DiscoveryOrder) -> Self {
        self.order = order;
        self
    }

    pub fn input_devices_for_os(os: PlatformOs) -> Self {
        Self {
            os,
            include_applications: false,
            include_processes: false,
            include_output_devices: false,
            include_input_devices: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawProcessRecord {
    pub identity: ProcessIdentity,
    pub parent_pid: Option<u32>,
    pub app_id: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
    pub executable_path: Option<Arc<str>>,
    pub active_audio: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawBackendProcessRecord {
    pub backend: BackendId,
    pub process: RawProcessRecord,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawBackendDeviceRecord {
    pub backend: BackendId,
    pub direction: DeviceDirection,
    pub device_id: Arc<str>,
    pub name: Option<Arc<str>>,
    pub is_default: bool,
    pub is_monitor_sink: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackendRawInventory {
    pub processes: Vec<RawBackendProcessRecord>,
    pub devices: Vec<RawBackendDeviceRecord>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryCursor {
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetSnapshot {
    pub cursor: DiscoveryCursor,
    pub targets: Vec<CaptureTarget>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetCatalog {
    pub cursor: DiscoveryCursor,
    pub targets: Vec<CaptureTarget>,
    pub applications: Vec<CaptureTarget>,
    pub processes: Vec<CaptureTarget>,
    pub output_devices: Vec<CaptureTarget>,
    pub input_devices: Vec<CaptureTarget>,
    pub default_output_device_id: Option<Arc<str>>,
    pub default_input_device_id: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoveryEventKind {
    Added(CaptureTarget),
    Removed(Arc<str>),
    StateChanged(CaptureTarget),
    DefaultDeviceChanged { device_id: Arc<str> },
    DefaultInputChanged { device_id: Arc<str> },
    WatcherFailed { reason: Arc<str> },
    VisibilityChanged(Visibility),
    ResnapshotRequired { minimum_revision: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryEvent {
    pub revision: u64,
    pub kind: DiscoveryEventKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindAppError {
    EmptyQuery,
    NotFound,
    AmbiguousMatch(usize),
}

impl std::fmt::Display for FindAppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyQuery => formatter.write_str("application query must not be empty"),
            Self::NotFound => formatter.write_str("no matching application or process found"),
            Self::AmbiguousMatch(count) => {
                write!(
                    formatter,
                    "ambiguous application query matched {count} targets"
                )
            }
        }
    }
}

impl std::error::Error for FindAppError {}

fn normalize_path_lower(path: &str) -> String {
    let mut s = path.trim().replace('\\', "/").to_ascii_lowercase();
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

fn extract_stem_from_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let lower = base.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    (!stem.is_empty()).then(|| stem.to_owned())
}

fn extract_exe_stem_lower(record: &RawProcessRecord) -> Option<String> {
    record
        .executable_path
        .as_deref()
        .and_then(extract_stem_from_str)
        .or_else(|| record.name.as_deref().and_then(extract_stem_from_str))
        .or_else(|| record.app_id.as_deref().and_then(extract_stem_from_str))
}

/// Returns whether a lowercase executable stem identifies a shell or session host on `os`.
#[inline]
pub fn is_shell_or_session_host_stem_for_os(os: PlatformOs, stem: &str) -> bool {
    // Literal matches avoid hashing and were faster in the Windows lookup benchmark.
    match os {
        PlatformOs::Windows => matches!(
            stem,
            "explorer"
                | "sihost"
                | "svchost"
                | "services"
                | "wininit"
                | "winlogon"
                | "csrss"
                | "smss"
                | "lsass"
                | "taskhostw"
                | "dllhost"
                | "conhost"
                | "openconsole"
                | "windowsterminal"
                | "wt"
                | "cmd"
                | "powershell"
                | "pwsh"
                | "wsl"
                | "runtimebroker"
                | "applicationframehost"
                | "searchhost"
                | "startmenuexperiencehost"
                | "shellexperiencehost"
                | "backgroundtaskhost"
                | "userinit"
                | "fontdrvhost"
                | "dwm"
                | "alacritty"
                | "wezterm"
                | "wezterm-gui"
        ),
        PlatformOs::Linux => matches!(
            stem,
            "systemd"
                | "init"
                | "gnome-shell"
                | "plasmashell"
                | "kwin_wayland"
                | "kwin_x11"
                | "sway"
                | "hyprland"
                | "wayfire"
                | "mutter"
                | "weston"
                | "xdg-desktop-portal"
                | "dbus-daemon"
                | "dbus-broker"
                | "pipewire"
                | "pipewire-pulse"
                | "wireplumber"
                | "pulseaudio"
                | "bash"
                | "zsh"
                | "fish"
                | "sh"
                | "dash"
                | "tmux"
                | "screen"
                | "alacritty"
                | "kitty"
                | "wezterm"
                | "wezterm-gui"
                | "gnome-terminal-server"
                | "konsole"
                | "xterm"
                | "foot"
                | "tilix"
                | "terminator"
                | "sshd"
                | "pwsh"
                | "powershell"
        ),
        PlatformOs::MacOs => matches!(
            stem,
            "launchd"
                | "windowserver"
                | "dock"
                | "finder"
                | "systemuiserver"
                | "loginwindow"
                | "terminal"
                | "iterm2"
                | "sshd"
                | "sshd-keygen-wrapper"
                | "xpcproxy"
                | "bash"
                | "zsh"
                | "fish"
                | "sh"
                | "tmux"
                | "screen"
                | "alacritty"
                | "kitty"
                | "wezterm"
                | "wezterm-gui"
                | "pwsh"
        ),
    }
}

/// Returns whether a lowercase executable stem identifies a shell or session host on the current OS.
#[inline]
pub fn is_shell_or_session_host_stem(stem: &str) -> bool {
    is_shell_or_session_host_stem_for_os(PlatformOs::current(), stem)
}

/// Returns `true` if `record` represents an OS shell, session/service host, or terminal launcher on `os`.
pub fn is_shell_or_session_host_for_os(os: PlatformOs, record: &RawProcessRecord) -> bool {
    if let Some(stem) = extract_exe_stem_lower(record)
        && is_shell_or_session_host_stem_for_os(os, &stem)
    {
        return true;
    }
    if let Some(name_stem) = record.name.as_deref().and_then(extract_stem_from_str)
        && is_shell_or_session_host_stem_for_os(os, &name_stem)
    {
        return true;
    }
    false
}

/// Returns `true` if `record` represents an OS shell, session/service host, or terminal launcher.
pub fn is_shell_or_session_host(record: &RawProcessRecord) -> bool {
    is_shell_or_session_host_for_os(PlatformOs::current(), record)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacAppBundleInfo<'a> {
    pub bundle_root: &'a str,
    pub bundle_name: &'a str,
    pub is_embedded_helper: bool,
}

pub fn extract_macos_app_bundle_info(norm_path: &str) -> Option<MacAppBundleInfo<'_>> {
    let mut search_from = 0usize;
    while let Some(rel_idx) = norm_path[search_from..].find(".app") {
        let idx = search_from + rel_idx;
        let rest = &norm_path[idx + 4..];
        let (bundle_root, after_bundle, bundle_name) = if rest.is_empty() || rest.starts_with('/') {
            let root = &norm_path[..idx + 4];
            let after = rest.strip_prefix('/').unwrap_or("");
            let name = root.rsplit('/').next().unwrap_or(root);
            (root, after, name)
        } else if rest == ".bundle" || rest.starts_with(".bundle/") {
            let root = &norm_path[..idx + 11];
            let after = rest.strip_prefix(".bundle/").unwrap_or("");
            let raw_name = root.rsplit('/').next().unwrap_or(root);
            let name = raw_name.strip_suffix(".bundle").unwrap_or(raw_name);
            (root, after, name)
        } else {
            search_from = idx + 4;
            continue;
        };

        let is_embedded_helper = after_bundle.starts_with("contents/frameworks/")
            || after_bundle.starts_with("contents/helpers/")
            || after_bundle.starts_with("contents/xpcservices/")
            || after_bundle.starts_with("contents/plugins/")
            || after_bundle.starts_with("contents/library/")
            || after_bundle.contains(".app/")
            || after_bundle.contains(".xpc/");

        return Some(MacAppBundleInfo {
            bundle_root,
            bundle_name,
            is_embedded_helper,
        });
    }
    None
}

/// Extract the outer lowercase macOS `.app` / `.app.bundle` bundle name from an arbitrary path.
pub fn extract_outer_macos_bundle_name(path: &str) -> Option<String> {
    let norm = normalize_path_lower(path);
    extract_macos_app_bundle_info(&norm).map(|info| info.bundle_name.to_owned())
}

fn same_macos_app_bundle(
    c_path: &str,
    c_bundle: MacAppBundleInfo<'_>,
    p_path: &str,
    p_bundle: MacAppBundleInfo<'_>,
) -> bool {
    c_bundle.bundle_root == p_bundle.bundle_root
        || (c_bundle.bundle_name == p_bundle.bundle_name
            && (c_path.contains(".code_sign_clone/") || p_path.contains(".code_sign_clone/")))
}

#[inline]
pub fn is_generic_system_bin_dir_for_os(os: PlatformOs, norm_dir: &str) -> bool {
    // Small fixed lists need no hash table; Windows lookup benchmarks favored matches.
    match os {
        PlatformOs::Windows => matches!(
            norm_dir,
            "c:/windows" | "c:/windows/system32" | "c:/windows/syswow64"
        ),
        PlatformOs::Linux => matches!(
            norm_dir,
            "/bin"
                | "/sbin"
                | "/usr/bin"
                | "/usr/sbin"
                | "/usr/local/bin"
                | "/usr/local/sbin"
                | "/usr/libexec"
        ),
        PlatformOs::MacOs => matches!(
            norm_dir,
            "/bin"
                | "/sbin"
                | "/usr/bin"
                | "/usr/sbin"
                | "/usr/local/bin"
                | "/usr/local/sbin"
                | "/usr/libexec"
                | "/system/library/coreservices"
                | "/system/library/frameworks"
        ),
    }
}

#[inline]
fn is_generic_system_bin_dir(norm_dir: &str) -> bool {
    is_generic_system_bin_dir_for_os(PlatformOs::current(), norm_dir)
}

fn crosses_multi_app_container(rel: &str) -> bool {
    // Matches keeps this small fixed set simple and avoids hashing.
    rel.split('/').any(|seg| {
        matches!(
            seg,
            "steamapps"
                | "common"
                | "applications"
                | "program files"
                | "program files (x86)"
                | "programs"
        )
    })
}

/// Returns whether `stem` is a shared runtime or script launcher that can host unrelated applications.
#[inline]
pub fn is_runtime_interpreter_stem(stem: &str) -> bool {
    matches!(
        stem,
        "gjs"
            | "gjs-console"
            | "python"
            | "python3"
            | "node"
            | "nodejs"
            | "deno"
            | "bun"
            | "java"
            | "javaw"
            | "ruby"
            | "perl"
            | "php"
            | "lua"
            | "dotnet"
            | "flatpak-spawn"
    ) || stem
        .strip_prefix("python3.")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn check_app_id_compatibility(child: &RawProcessRecord, parent: &RawProcessRecord) -> Option<bool> {
    let (Some(c_app_raw), Some(p_app_raw)) = (child.app_id.as_deref(), parent.app_id.as_deref())
    else {
        return None;
    };
    if c_app_raw.contains(['/', '\\']) || p_app_raw.contains(['/', '\\']) {
        return None;
    }
    let c_app = c_app_raw.trim().to_ascii_lowercase();
    let p_app = p_app_raw.trim().to_ascii_lowercase();
    if c_app.is_empty() || p_app.is_empty() {
        return None;
    }
    if c_app == p_app
        || c_app
            .strip_prefix(&p_app)
            .is_some_and(|rest| rest.starts_with('.'))
        || p_app
            .strip_prefix(&c_app)
            .is_some_and(|rest| rest.starts_with('.'))
    {
        return Some(true);
    }
    if let Some(helper_pos) = c_app.find(".helper").or_else(|| c_app.find(".xpc")) {
        let c_base = &c_app[..helper_pos];
        if !c_base.is_empty()
            && (p_app == c_base
                || p_app
                    .strip_prefix(c_base)
                    .is_some_and(|rest| rest.starts_with('.')))
        {
            return Some(true);
        }
    }
    if c_app.contains('.') && p_app.contains('.') {
        return Some(false);
    }
    None
}

/// Determines whether `child` belongs to the same logical application package as `parent` on `os`
/// and should roll up into `parent` for `applications()` and `find_app()`.
pub fn can_roll_up_to_parent_for_os(
    os: PlatformOs,
    child: &RawProcessRecord,
    parent: &RawProcessRecord,
) -> bool {
    if is_shell_or_session_host_for_os(os, parent) {
        return false;
    }

    let c_stem_opt = extract_exe_stem_lower(child);
    let p_stem_opt = extract_exe_stem_lower(parent);
    let shared_interpreter = c_stem_opt
        .as_deref()
        .is_some_and(is_runtime_interpreter_stem)
        || p_stem_opt
            .as_deref()
            .is_some_and(is_runtime_interpreter_stem);

    if shared_interpreter {
        if let Some(compatible) = check_app_id_compatibility(child, parent) {
            return compatible;
        }
        if let (Some(c_name), Some(p_name)) = (child.name.as_deref(), parent.name.as_deref()) {
            let cn = c_name.trim().to_ascii_lowercase();
            let pn = p_name.trim().to_ascii_lowercase();
            if !cn.is_empty() && !pn.is_empty() && cn != pn {
                return false;
            }
        }
    }

    if let (Some(c_raw), Some(p_raw)) = (
        child.executable_path.as_deref(),
        parent.executable_path.as_deref(),
    ) {
        let c_path = normalize_path_lower(c_raw);
        let p_path = normalize_path_lower(p_raw);
        if c_path == p_path && !c_path.is_empty() {
            return true;
        }
        match (
            extract_macos_app_bundle_info(&c_path),
            extract_macos_app_bundle_info(&p_path),
        ) {
            (Some(c_bundle), Some(p_bundle)) => {
                return same_macos_app_bundle(&c_path, c_bundle, &p_path, p_bundle);
            }
            (Some(_), None) | (None, Some(_)) => {
                return false;
            }
            (None, None) => {}
        }
        if let (Some((c_dir, _)), Some((p_dir, _))) =
            (c_path.rsplit_once('/'), p_path.rsplit_once('/'))
            && !c_dir.is_empty()
            && !p_dir.is_empty()
        {
            if is_generic_system_bin_dir_for_os(os, p_dir) {
                return c_dir == p_dir
                    && c_stem_opt
                        .as_ref()
                        .zip(p_stem_opt.as_ref())
                        .is_some_and(|(cs, ps)| cs == ps && !is_runtime_interpreter_stem(cs));
            }
            if c_dir == p_dir {
                return true;
            }
            if let Some(rel) = c_dir.strip_prefix(p_dir).and_then(|r| r.strip_prefix('/')) {
                return !crosses_multi_app_container(rel);
            }
            if let Some(rel) = p_dir.strip_prefix(c_dir).and_then(|r| r.strip_prefix('/')) {
                return !crosses_multi_app_container(rel);
            }
            return false;
        }
    }

    if let (Some(c_stem), Some(p_stem)) = (&c_stem_opt, &p_stem_opt)
        && c_stem == p_stem
        && !is_runtime_interpreter_stem(c_stem)
    {
        return true;
    }

    if let Some(compatible) = check_app_id_compatibility(child, parent) {
        return compatible;
    }

    true
}

pub fn can_roll_up_to_parent(child: &RawProcessRecord, parent: &RawProcessRecord) -> bool {
    let _ = is_generic_system_bin_dir;
    can_roll_up_to_parent_for_os(PlatformOs::current(), child, parent)
}

/// Roll up raw OS processes and per-PID audio states into `processes()` and `applications()`
/// `CaptureTarget` lists for `os`.
fn build_process_and_app_targets_for_os(
    os: PlatformOs,
    processes: &[RawProcessRecord],
    filter: &DiscoveryFilter,
) -> Result<Vec<CaptureTarget>, PlanError> {
    let mut results = Vec::new();
    if !filter.include_processes && !filter.include_applications {
        return Ok(results);
    }

    let by_pid: BTreeMap<u32, usize> = processes
        .iter()
        .enumerate()
        .map(|(idx, p)| (p.identity.pid, idx))
        .collect();

    if filter.include_processes {
        for proc_rec in processes {
            if !filter.matches_audio_state(proc_rec.active_audio) {
                continue;
            }
            let target =
                CaptureTarget::process(proc_rec.identity, ProcessScope::Tree, OnRootExit::Stop)?
                    .with_metadata(
                        proc_rec.app_id.clone(),
                        proc_rec.name.clone(),
                        proc_rec.executable_path.clone(),
                        proc_rec.active_audio,
                    );
            results.push(target);
        }
    }

    if filter.include_applications {
        let norm_paths: Vec<Option<String>> = processes
            .iter()
            .map(|p| p.executable_path.as_deref().map(normalize_path_lower))
            .collect();
        let bundle_infos: Vec<Option<MacAppBundleInfo<'_>>> = norm_paths
            .iter()
            .map(|np| np.as_deref().and_then(extract_macos_app_bundle_info))
            .collect();

        let find_root_idx = |mut idx: usize| -> usize {
            let mut visited = BTreeSet::new();
            visited.insert(processes[idx].identity.pid);
            while let Some(parent_pid) = processes[idx].parent_pid {
                if !visited.insert(parent_pid) {
                    break;
                }
                if let Some(&parent_idx) = by_pid.get(&parent_pid)
                    && processes[parent_idx].identity.generation
                        <= processes[idx].identity.generation
                    && can_roll_up_to_parent_for_os(os, &processes[idx], &processes[parent_idx])
                {
                    idx = parent_idx;
                } else {
                    break;
                }
            }

            if let (Some(c_path), Some(c_bundle)) = (norm_paths[idx].as_deref(), bundle_infos[idx])
                && c_bundle.is_embedded_helper
            {
                let mut best_main: Option<usize> = None;
                let mut best_any: Option<usize> = None;
                for (other_idx, other_rec) in processes.iter().enumerate() {
                    if is_shell_or_session_host_for_os(os, other_rec) {
                        continue;
                    }
                    let (Some(o_path), Some(o_bundle)) =
                        (norm_paths[other_idx].as_deref(), bundle_infos[other_idx])
                    else {
                        continue;
                    };
                    if !same_macos_app_bundle(c_path, c_bundle, o_path, o_bundle) {
                        continue;
                    }
                    let o_key = (other_rec.identity.generation, other_rec.identity.pid);
                    if !o_bundle.is_embedded_helper {
                        if best_main.is_none_or(|prev| {
                            o_key
                                < (
                                    processes[prev].identity.generation,
                                    processes[prev].identity.pid,
                                )
                        }) {
                            best_main = Some(other_idx);
                        }
                    } else if best_any.is_none_or(|prev| {
                        o_key
                            < (
                                processes[prev].identity.generation,
                                processes[prev].identity.pid,
                            )
                    }) {
                        best_any = Some(other_idx);
                    }
                }
                if let Some(target_idx) = best_main.or(best_any) {
                    idx = target_idx;
                }
            }
            idx
        };

        let mut root_audio: BTreeMap<usize, Option<bool>> = BTreeMap::new();
        for (idx, proc_rec) in processes.iter().enumerate() {
            let root_idx = find_root_idx(idx);
            let entry = root_audio.entry(root_idx).or_insert(None);
            match (*entry, proc_rec.active_audio) {
                (_, Some(true)) => *entry = Some(true),
                (None, Some(false)) => *entry = Some(false),
                _ => {}
            }
        }

        for (root_idx, aggregated_audio) in root_audio {
            if !filter.matches_audio_state(aggregated_audio) {
                continue;
            }
            let root_rec = &processes[root_idx];
            let mut target = CaptureTarget::application(
                root_rec.identity,
                root_rec.app_id.clone(),
                root_rec.name.clone(),
                root_rec.executable_path.clone(),
                ProcessScope::Tree,
                OnRootExit::Stop,
            )?;
            target.active_audio = aggregated_audio;
            results.push(target);
        }
    }

    Ok(results)
}

/// Resolve a pre-audio or active application target by `app_id`, `name`, or executable basename.
/// Rejects `0` matches with `NotFound` and `> 1` matches with `AmbiguousMatch`.
pub fn find_app_in_raw_for_os(
    os: PlatformOs,
    processes: &[RawProcessRecord],
    query: &str,
    scope: ProcessScope,
    on_root_exit: OnRootExit,
) -> Result<CaptureTarget, FindAppError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(FindAppError::EmptyQuery);
    }
    let needle = trimmed.to_ascii_lowercase();
    let all_apps = build_process_and_app_targets_for_os(
        os,
        processes,
        &DiscoveryFilter::applications(false, true),
    )
    .map_err(|_| FindAppError::NotFound)?;

    let matches: Vec<CaptureTarget> = all_apps
        .into_iter()
        .filter(|target| {
            let app_match = target
                .app_id
                .as_deref()
                .is_some_and(|id| id.to_ascii_lowercase() == needle);
            let name_match = target
                .name
                .as_deref()
                .is_some_and(|name| name.to_ascii_lowercase() == needle);
            let exe_match = target.executable_path.as_deref().is_some_and(|path| {
                let lower = normalize_path_lower(path);
                if lower == needle {
                    return true;
                }
                let base = lower.rsplit('/').next().unwrap_or(&lower);
                if base == needle || base.strip_suffix(".exe") == Some(needle.as_str()) {
                    return true;
                }
                if let Some(bundle) = extract_macos_app_bundle_info(&lower) {
                    return bundle.bundle_name == needle
                        || bundle.bundle_name.strip_suffix(".app") == Some(needle.as_str());
                }
                false
            });
            app_match || name_match || exe_match
        })
        .collect();

    match matches.len() {
        0 => Err(FindAppError::NotFound),
        1 => {
            let mut found = matches.into_iter().next().unwrap();
            found.scope = scope;
            found.on_root_exit = on_root_exit;
            Ok(found)
        }
        count => Err(FindAppError::AmbiguousMatch(count)),
    }
}

pub fn find_app_in_raw(
    processes: &[RawProcessRecord],
    query: &str,
    scope: ProcessScope,
    on_root_exit: OnRootExit,
) -> Result<CaptureTarget, FindAppError> {
    find_app_in_raw_for_os(PlatformOs::current(), processes, query, scope, on_root_exit)
}

struct SubscriberState {
    filter: DiscoveryFilter,
    query: Option<DiscoveryQuery>,
    baseline_targets: BTreeMap<Arc<str>, CaptureTarget>,
    baseline_initialized: bool,
    max_queue: usize,
    queue: VecDeque<DiscoveryEvent>,
    overflowed: bool,
    minimum_resnapshot_revision: Option<u64>,
    last_polled_revision: u64,
    closed: bool,
}

#[derive(Clone)]
pub struct DiscoverySubscription {
    id: u64,
    service: DiscoveryService,
    state: Arc<Mutex<SubscriberState>>,
}

impl DiscoverySubscription {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn poll_event(&self) -> Option<DiscoveryEvent> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let event = guard.queue.pop_front()?;
        guard.last_polled_revision = guard.last_polled_revision.max(event.revision);
        Some(event)
    }

    pub fn pending_len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queue
            .len()
    }

    pub fn is_overflowed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .overflowed
    }

    /// Reconcile an overflowed or stale subscriber cursor against a fresh snapshot.
    pub fn resnapshot(&self) -> Result<TargetSnapshot, PlanError> {
        let (filter, query_opt) = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.filter.clone(), guard.query.clone())
        };
        let snap = if let Some(ref q) = query_opt {
            let cat = self.service.discover_query(q)?;
            TargetSnapshot {
                cursor: cat.cursor,
                targets: cat.targets,
            }
        } else {
            self.service.discover(filter)?
        };
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.query.is_some() {
            guard.baseline_targets = snap
                .targets
                .iter()
                .cloned()
                .map(|t| (t.id.clone(), t))
                .collect();
            guard.baseline_initialized = true;
        }
        guard
            .queue
            .retain(|event| event.revision > snap.cursor.revision);
        guard.overflowed = false;
        guard.minimum_resnapshot_revision = None;
        guard.last_polled_revision = snap.cursor.revision;
        Ok(snap)
    }

    /// Reconcile an overflowed or stale subscriber cursor against a full `TargetCatalog`.
    pub fn resnapshot_catalog(&self) -> Result<TargetCatalog, PlanError> {
        let query = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.query.clone().unwrap_or_else(|| {
                DiscoveryQuery::from_filter_for_os(self.service.os(), &guard.filter)
            })
        };
        let cat = self.service.discover_query(&query)?;
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.query.is_some() {
            guard.baseline_targets = cat
                .targets
                .iter()
                .cloned()
                .map(|t| (t.id.clone(), t))
                .collect();
            guard.baseline_initialized = true;
        }
        guard
            .queue
            .retain(|event| event.revision > cat.cursor.revision);
        guard.overflowed = false;
        guard.minimum_resnapshot_revision = None;
        guard.last_polled_revision = cat.cursor.revision;
        Ok(cat)
    }
}

impl Drop for DiscoverySubscription {
    fn drop(&mut self) {
        if Arc::strong_count(&self.state) <= 2 {
            self.service.unsubscribe(self.id);
        }
    }
}

struct DiffBaseline {
    initialized: bool,
    default_device_id: Option<Arc<str>>,
    default_input_id: Option<Arc<str>>,
    targets: BTreeMap<Arc<str>, CaptureTarget>,
}

struct ServiceInner {
    os: Mutex<PlatformOs>,
    revision: AtomicU64,
    next_sub_id: AtomicU64,
    backend_inventory: Mutex<BackendRawInventory>,
    diff_baseline: Mutex<DiffBaseline>,
    subscribers: Mutex<BTreeMap<u64, Arc<Mutex<SubscriberState>>>>,
}

fn push_event_to_subscriber_guard(
    guard: &mut SubscriberState,
    revision: u64,
    kind: DiscoveryEventKind,
) {
    if guard.overflowed {
        if let Some(DiscoveryEvent {
            kind: DiscoveryEventKind::ResnapshotRequired { minimum_revision },
            ..
        }) = guard.queue.back_mut()
        {
            *minimum_revision = revision;
        }
        guard.minimum_resnapshot_revision = Some(revision);
        return;
    }
    if guard.queue.len() >= guard.max_queue {
        guard.overflowed = true;
        guard.minimum_resnapshot_revision = Some(revision);
        guard.queue.pop_back();
        guard.queue.push_back(DiscoveryEvent {
            revision,
            kind: DiscoveryEventKind::ResnapshotRequired {
                minimum_revision: revision,
            },
        });
    } else {
        guard.queue.push_back(DiscoveryEvent { revision, kind });
    }
}

/// Evaluate multi-backend `BackendRawInventory` against a `DiscoveryQuery` (`backends` filter & rank,
/// `.monitor` sink exclusion from `input_devices`, ScreenCaptureKit `Application(Exact)` / no-`Process`
/// rules, `ByLogicalTarget` vs `PerBackend` deduplication, and `KindThenBackend` vs `BackendThenKind` ordering).
pub fn build_catalog_from_backend_inventory(
    inventory: &BackendRawInventory,
    query: &DiscoveryQuery,
    revision: u64,
) -> Result<TargetCatalog, PlanError> {
    query.backends.validate()?;
    let active_backends = query.backends.for_os(query.os);
    let backend_rank =
        |b: BackendId| -> Option<usize> { active_backends.iter().position(|&cand| cand == b) };

    let per_backend_filter = DiscoveryFilter {
        active_only: query.active_only,
        include_non_audio: query.include_non_audio,
        include_processes: query.include_processes,
        include_applications: query.include_applications,
        include_devices: false,
        include_input_devices: false,
    };

    let mut raw_candidates: Vec<(BackendId, CaptureTarget)> = Vec::new();
    let mut default_output_id: Option<Arc<str>> = None;
    let mut default_input_id: Option<Arc<str>> = None;

    for &backend in active_backends {
        let b_procs: Vec<RawProcessRecord> = inventory
            .processes
            .iter()
            .filter(|r| r.backend == backend)
            .map(|r| r.process.clone())
            .collect();
        if !b_procs.is_empty() && (query.include_applications || query.include_processes) {
            let mut b_filter = per_backend_filter.clone();
            if backend == BackendId::ScreenCaptureKit {
                // ScreenCaptureKit captures shareable applications (`scope = Exact`), never raw BSD processes.
                b_filter.include_processes = false;
            }
            let built = build_process_and_app_targets_for_os(query.os, &b_procs, &b_filter)?;
            for mut t in built {
                if backend == BackendId::ScreenCaptureKit && t.kind == TargetKind::Application {
                    t.scope = ProcessScope::Exact;
                }
                raw_candidates.push((backend, t));
            }
        }

        for dev in inventory.devices.iter().filter(|d| d.backend == backend) {
            match dev.direction {
                DeviceDirection::Output => {
                    if dev.is_default && default_output_id.is_none() {
                        default_output_id = Some(dev.device_id.clone());
                    }
                    if query.include_output_devices {
                        let mut target = CaptureTarget::device(dev.device_id.clone())?;
                        target.name = dev.name.clone();
                        target.active_audio = None;
                        raw_candidates.push((backend, target));
                    }
                }
                DeviceDirection::Input => {
                    // Strictly exclude `.monitor` sink monitors from input/microphone targets
                    if dev.is_monitor_sink || dev.device_id.ends_with(".monitor") {
                        continue;
                    }
                    if dev.is_default && default_input_id.is_none() {
                        default_input_id = Some(dev.device_id.clone());
                    }
                    if query.include_input_devices {
                        let mut target = CaptureTarget::input_device(dev.device_id.clone())?;
                        target.name = dev.name.clone();
                        target.active_audio = None;
                        raw_candidates.push((backend, target));
                    }
                }
            }
        }
    }

    let mut targets: Vec<(BackendId, CaptureTarget)> = Vec::new();
    match query.dedupe {
        DiscoveryDedupe::ByLogicalTarget => {
            let mut by_key: BTreeMap<String, usize> = BTreeMap::new();
            for (backend, mut cand) in raw_candidates {
                let logical_key = strip_backend_id_prefix(&cand.id).to_owned();
                if let Some(&idx) = by_key.get(&logical_key) {
                    let (_, existing) = &mut targets[idx];
                    if !existing.available_backends.contains(&backend) {
                        let mut avail: Vec<BackendId> = existing.available_backends.to_vec();
                        avail.push(backend);
                        existing.available_backends = Arc::from(avail);
                    }
                    if cand.active_audio == Some(true) {
                        existing.active_audio = Some(true);
                    } else if existing.active_audio.is_none() {
                        existing.active_audio = cand.active_audio;
                    }
                    if existing.name.is_none() {
                        existing.name = cand.name.take();
                    }
                } else {
                    cand.backend = None;
                    cand.available_backends = Arc::from([backend]);
                    let idx = targets.len();
                    by_key.insert(logical_key, idx);
                    targets.push((backend, cand));
                }
            }
        }
        DiscoveryDedupe::PerBackend => {
            let mut seen = BTreeSet::new();
            for (backend, cand) in raw_candidates {
                let pinned = cand
                    .with_backend(backend)
                    .with_available_backends(vec![backend]);
                if seen.insert(pinned.id.clone()) {
                    targets.push((backend, pinned));
                }
            }
        }
    }

    let audio_rank = |a: Option<bool>| match a {
        Some(true) => 0u8,
        Some(false) => 1u8,
        None => 2u8,
    };

    targets.sort_by(|(ba, a), (bb, b)| match query.order {
        DiscoveryOrder::KindThenBackend => a
            .kind
            .cmp(&b.kind)
            .then_with(|| audio_rank(a.active_audio).cmp(&audio_rank(b.active_audio)))
            .then_with(|| {
                backend_rank(*ba)
                    .unwrap_or(usize::MAX)
                    .cmp(&backend_rank(*bb).unwrap_or(usize::MAX))
            })
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
            .then_with(|| a.id.cmp(&b.id)),
        DiscoveryOrder::BackendThenKind => backend_rank(*ba)
            .unwrap_or(usize::MAX)
            .cmp(&backend_rank(*bb).unwrap_or(usize::MAX))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| audio_rank(a.active_audio).cmp(&audio_rank(b.active_audio)))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
            .then_with(|| a.id.cmp(&b.id)),
    });

    let all_targets: Vec<CaptureTarget> = targets.into_iter().map(|(_, t)| t).collect();
    let applications = all_targets
        .iter()
        .filter(|t| t.kind == TargetKind::Application)
        .cloned()
        .collect();
    let processes = all_targets
        .iter()
        .filter(|t| t.kind == TargetKind::Process)
        .cloned()
        .collect();
    let output_devices = all_targets
        .iter()
        .filter(|t| t.kind == TargetKind::Device)
        .cloned()
        .collect();
    let input_devices = all_targets
        .iter()
        .filter(|t| t.kind == TargetKind::InputDevice)
        .cloned()
        .collect();

    Ok(TargetCatalog {
        cursor: DiscoveryCursor { revision },
        targets: all_targets,
        applications,
        processes,
        output_devices,
        input_devices,
        default_output_device_id: default_output_id,
        default_input_device_id: default_input_id,
    })
}

/// Shareable lazy discovery service backed by a single unified `BackendRawInventory`.
/// One-shot snapshots never spawn background threads.
#[derive(Clone)]
pub struct DiscoveryService {
    inner: Arc<ServiceInner>,
}

impl DiscoveryService {
    pub fn new_for_os(os: PlatformOs) -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                os: Mutex::new(os),
                revision: AtomicU64::new(1),
                next_sub_id: AtomicU64::new(1),
                backend_inventory: Mutex::new(BackendRawInventory::default()),
                diff_baseline: Mutex::new(DiffBaseline {
                    initialized: false,
                    default_device_id: None,
                    default_input_id: None,
                    targets: BTreeMap::new(),
                }),
                subscribers: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    pub fn os(&self) -> PlatformOs {
        *self.inner.os.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn active_subscriber_count(&self) -> usize {
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn current_revision(&self) -> u64 {
        self.inner.revision.load(SeqCst)
    }

    pub fn default_output_device_id(&self) -> Option<Arc<str>> {
        self.inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .devices
            .iter()
            .find(|d| d.direction == DeviceDirection::Output && d.is_default)
            .map(|d| d.device_id.clone())
    }

    pub fn default_input_device_id(&self) -> Option<Arc<str>> {
        self.inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .devices
            .iter()
            .find(|d| {
                d.direction == DeviceDirection::Input
                    && !d.is_monitor_sink
                    && !d.device_id.ends_with(".monitor")
                    && d.is_default
            })
            .map(|d| d.device_id.clone())
    }

    pub fn update_backend_inventory(
        &self,
        inventory: BackendRawInventory,
        query: &DiscoveryQuery,
    ) -> Result<u64, PlanError> {
        let rev_preview = self.inner.revision.load(SeqCst) + 1;
        let catalog = build_catalog_from_backend_inventory(&inventory, query, rev_preview)?;
        *self.inner.os.lock().unwrap_or_else(|e| e.into_inner()) = query.os;
        *self
            .inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = inventory.clone();

        {
            let mut base = self
                .inner
                .diff_baseline
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !base.initialized {
                base.initialized = true;
                base.default_device_id = catalog.default_output_device_id;
                base.default_input_id = catalog.default_input_device_id;
                base.targets = catalog
                    .targets
                    .into_iter()
                    .map(|t| (t.id.clone(), t))
                    .collect();
            }
        }
        self.init_query_subscriber_baselines(&inventory, rev_preview);
        Ok(self.inner.revision.fetch_add(1, SeqCst) + 1)
    }

    pub fn applications(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> Result<Vec<CaptureTarget>, PlanError> {
        Ok(self
            .discover(DiscoveryFilter::applications(
                active_only,
                include_non_audio,
            ))?
            .targets)
    }

    pub fn processes(
        &self,
        active_only: bool,
        include_non_audio: bool,
    ) -> Result<Vec<CaptureTarget>, PlanError> {
        Ok(self
            .discover(DiscoveryFilter::processes(active_only, include_non_audio))?
            .targets)
    }

    pub fn output_devices(&self) -> Result<Vec<CaptureTarget>, PlanError> {
        Ok(self.discover(DiscoveryFilter::output_devices())?.targets)
    }

    pub fn input_devices(&self) -> Result<Vec<CaptureTarget>, PlanError> {
        Ok(self
            .discover_query(&DiscoveryQuery::input_devices_for_os(self.os()))?
            .input_devices)
    }

    fn init_query_subscriber_baselines(&self, inventory: &BackendRawInventory, rev: u64) {
        let subs: Vec<Arc<Mutex<SubscriberState>>> = self
            .inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for sub in subs {
            let mut guard = sub.lock().unwrap_or_else(|e| e.into_inner());
            if guard.closed || guard.baseline_initialized {
                continue;
            }
            if let Some(ref q) = guard.query
                && let Ok(cat) = build_catalog_from_backend_inventory(inventory, q, rev)
            {
                guard.baseline_targets =
                    cat.targets.into_iter().map(|t| (t.id.clone(), t)).collect();
                guard.baseline_initialized = true;
            }
        }
    }

    pub fn ingest_backend_inventory(
        &self,
        inventory: BackendRawInventory,
        query: &DiscoveryQuery,
    ) -> Result<u64, PlanError> {
        let rev_preview = self.inner.revision.load(SeqCst) + 1;
        let catalog = build_catalog_from_backend_inventory(&inventory, query, rev_preview)?;
        *self.inner.os.lock().unwrap_or_else(|e| e.into_inner()) = query.os;
        *self
            .inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = inventory.clone();

        let mut last_rev = self.inner.revision.fetch_add(1, SeqCst) + 1;
        let mut default_events = Vec::new();
        let mut filter_events = Vec::new();
        {
            let mut base = self
                .inner
                .diff_baseline
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut next_map = BTreeMap::new();
            if !base.initialized {
                for target in catalog.targets {
                    filter_events.push(DiscoveryEventKind::Added(target.clone()));
                    next_map.insert(target.id.clone(), target);
                }
                base.initialized = true;
                base.default_device_id = catalog.default_output_device_id;
                base.default_input_id = catalog.default_input_device_id;
                base.targets = next_map;
            } else {
                if let (Some(prev_def), Some(curr_def)) =
                    (&base.default_device_id, &catalog.default_output_device_id)
                    && prev_def != curr_def
                {
                    default_events.push(DiscoveryEventKind::DefaultDeviceChanged {
                        device_id: curr_def.clone(),
                    });
                }
                base.default_device_id = catalog.default_output_device_id;

                if let (Some(prev_in), Some(curr_in)) =
                    (&base.default_input_id, &catalog.default_input_device_id)
                    && prev_in != curr_in
                {
                    default_events.push(DiscoveryEventKind::DefaultInputChanged {
                        device_id: curr_in.clone(),
                    });
                }
                base.default_input_id = catalog.default_input_device_id;

                for target in catalog.targets {
                    match base.targets.get(&target.id) {
                        None => {
                            filter_events.push(DiscoveryEventKind::Added(target.clone()));
                        }
                        Some(old)
                            if old.active_audio != target.active_audio
                                || old.name != target.name =>
                        {
                            filter_events.push(DiscoveryEventKind::StateChanged(target.clone()));
                        }
                        _ => {}
                    }
                    next_map.insert(target.id.clone(), target);
                }
                for old_id in base.targets.keys() {
                    if !next_map.contains_key(old_id) {
                        filter_events.push(DiscoveryEventKind::Removed(old_id.clone()));
                    }
                }
                base.targets = next_map;
            }
        }

        for ev in default_events {
            last_rev = self.publish_event(ev);
        }
        for ev in filter_events {
            last_rev = self.publish_filter_only_event(ev);
        }

        let subs: Vec<Arc<Mutex<SubscriberState>>> = self
            .inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for sub in subs {
            let sub_query = {
                let guard = sub.lock().unwrap_or_else(|e| e.into_inner());
                if guard.closed {
                    None
                } else {
                    guard.query.clone()
                }
            };
            let Some(sub_q) = sub_query else {
                continue;
            };
            let Ok(sub_cat) = build_catalog_from_backend_inventory(&inventory, &sub_q, last_rev)
            else {
                continue;
            };
            let mut guard = sub.lock().unwrap_or_else(|e| e.into_inner());
            if guard.closed {
                continue;
            }
            let mut next_sub_map = BTreeMap::new();
            let mut sub_events = Vec::new();
            if !guard.baseline_initialized {
                for target in sub_cat.targets {
                    sub_events.push(DiscoveryEventKind::Added(target.clone()));
                    next_sub_map.insert(target.id.clone(), target);
                }
                guard.baseline_initialized = true;
            } else {
                for target in sub_cat.targets {
                    match guard.baseline_targets.get(&target.id) {
                        None => sub_events.push(DiscoveryEventKind::Added(target.clone())),
                        Some(old)
                            if old.active_audio != target.active_audio
                                || old.name != target.name =>
                        {
                            sub_events.push(DiscoveryEventKind::StateChanged(target.clone()));
                        }
                        _ => {}
                    }
                    next_sub_map.insert(target.id.clone(), target);
                }
                for old_id in guard.baseline_targets.keys() {
                    if !next_sub_map.contains_key(old_id) {
                        sub_events.push(DiscoveryEventKind::Removed(old_id.clone()));
                    }
                }
            }
            guard.baseline_targets = next_sub_map;
            for kind in sub_events {
                last_rev = self.inner.revision.fetch_add(1, SeqCst) + 1;
                push_event_to_subscriber_guard(&mut guard, last_rev, kind);
            }
        }

        Ok(last_rev)
    }

    pub fn discover_query(&self, query: &DiscoveryQuery) -> Result<TargetCatalog, PlanError> {
        let revision = self.inner.revision.load(SeqCst);
        let inv = self
            .inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        build_catalog_from_backend_inventory(&inv, query, revision)
    }

    pub fn discover(&self, filter: DiscoveryFilter) -> Result<TargetSnapshot, PlanError> {
        let revision = self.inner.revision.load(SeqCst);
        let os = self.os();
        let inv = self
            .inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let query = DiscoveryQuery::from_filter_for_os(os, &filter);
        let catalog = build_catalog_from_backend_inventory(&inv, &query, revision)?;
        Ok(TargetSnapshot {
            cursor: catalog.cursor,
            targets: catalog.targets,
        })
    }

    pub fn find_app(
        &self,
        query: &str,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, FindAppError> {
        self.find_app_with_backends(query, &Backends::default(), scope, on_root_exit)
    }

    pub fn find_app_with_backends(
        &self,
        query: &str,
        backends: &Backends,
        scope: ProcessScope,
        on_root_exit: OnRootExit,
    ) -> Result<CaptureTarget, FindAppError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(FindAppError::EmptyQuery);
        }
        let needle = trimmed.to_ascii_lowercase();
        let os = self.os();
        let mut all_apps_query = DiscoveryQuery::all_for_os(os).with_backends(backends.clone());
        all_apps_query.active_only = false;
        all_apps_query.include_non_audio = true;
        all_apps_query.include_applications = true;
        all_apps_query.include_processes = false;
        all_apps_query.include_output_devices = false;
        all_apps_query.include_input_devices = false;

        let catalog = self
            .discover_query(&all_apps_query)
            .map_err(|_| FindAppError::NotFound)?;

        let matches: Vec<CaptureTarget> = catalog
            .applications
            .into_iter()
            .filter(|target| {
                let app_match = target
                    .app_id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase() == needle);
                let name_match = target
                    .name
                    .as_deref()
                    .is_some_and(|name| name.to_ascii_lowercase() == needle);
                let exe_match = target.executable_path.as_deref().is_some_and(|path| {
                    let lower = normalize_path_lower(path);
                    if lower == needle {
                        return true;
                    }
                    let base = lower.rsplit('/').next().unwrap_or(&lower);
                    if base == needle || base.strip_suffix(".exe") == Some(needle.as_str()) {
                        return true;
                    }
                    if let Some(bundle) = extract_macos_app_bundle_info(&lower) {
                        return bundle.bundle_name == needle
                            || bundle.bundle_name.strip_suffix(".app") == Some(needle.as_str());
                    }
                    false
                });
                app_match || name_match || exe_match
            })
            .collect();

        match matches.len() {
            0 => Err(FindAppError::NotFound),
            1 => {
                let mut found = matches.into_iter().next().unwrap();
                found.scope = scope;
                found.on_root_exit = on_root_exit;
                Ok(found)
            }
            count => Err(FindAppError::AmbiguousMatch(count)),
        }
    }

    pub fn subscribe(
        &self,
        filter: DiscoveryFilter,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, PlanError> {
        if !(32..=4096).contains(&max_discovery_queue) {
            return Err(PlanError::Capacity(CapacityError::ExceedsByteCeiling));
        }
        let id = self.inner.next_sub_id.fetch_add(1, SeqCst);
        let rev = self.inner.revision.load(SeqCst);
        let state = Arc::new(Mutex::new(SubscriberState {
            filter,
            query: None,
            baseline_targets: BTreeMap::new(),
            baseline_initialized: false,
            max_queue: max_discovery_queue,
            queue: VecDeque::with_capacity(max_discovery_queue.min(256)),
            overflowed: false,
            minimum_resnapshot_revision: None,
            last_polled_revision: rev,
            closed: false,
        }));
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, state.clone());
        Ok(DiscoverySubscription {
            id,
            service: self.clone(),
            state,
        })
    }

    /// Subscribe to incremental discovery events governed by a `DiscoveryQuery`
    /// (supporting `DiscoveryDedupe::PerBackend`, custom `Backends` filtering, and `resnapshot_catalog`).
    pub fn subscribe_query(
        &self,
        query: DiscoveryQuery,
        max_discovery_queue: usize,
    ) -> Result<DiscoverySubscription, PlanError> {
        query.backends.validate()?;
        if !(32..=4096).contains(&max_discovery_queue) {
            return Err(PlanError::Capacity(CapacityError::ExceedsByteCeiling));
        }
        let id = self.inner.next_sub_id.fetch_add(1, SeqCst);
        let rev = self.inner.revision.load(SeqCst);
        let inv = self
            .inner
            .backend_inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let has_initial_inv = !inv.processes.is_empty() || !inv.devices.is_empty();
        let initial_targets = if has_initial_inv {
            build_catalog_from_backend_inventory(&inv, &query, rev)?
                .targets
                .into_iter()
                .map(|t| (t.id.clone(), t))
                .collect()
        } else {
            BTreeMap::new()
        };
        let filter = query.as_filter();
        let state = Arc::new(Mutex::new(SubscriberState {
            filter,
            query: Some(query),
            baseline_targets: initial_targets,
            baseline_initialized: has_initial_inv,
            max_queue: max_discovery_queue,
            queue: VecDeque::with_capacity(max_discovery_queue.min(256)),
            overflowed: false,
            minimum_resnapshot_revision: None,
            last_polled_revision: rev,
            closed: false,
        }));
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, state.clone());
        Ok(DiscoverySubscription {
            id,
            service: self.clone(),
            state,
        })
    }

    fn unsubscribe(&self, id: u64) {
        if let Some(sub) = self
            .inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        {
            sub.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        }
    }

    fn publish_filter_only_event(&self, kind: DiscoveryEventKind) -> u64 {
        self.publish_event_internal(kind, true)
    }

    pub fn publish_event(&self, kind: DiscoveryEventKind) -> u64 {
        self.publish_event_internal(kind, false)
    }

    fn publish_event_internal(&self, kind: DiscoveryEventKind, skip_query_subs: bool) -> u64 {
        let revision = self.inner.revision.fetch_add(1, SeqCst) + 1;
        let subs: Vec<Arc<Mutex<SubscriberState>>> = self
            .inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();

        for sub in subs {
            let mut guard = sub.lock().unwrap_or_else(|e| e.into_inner());
            if guard.closed || (skip_query_subs && guard.query.is_some()) {
                continue;
            }
            let matches = match &kind {
                DiscoveryEventKind::Added(target) | DiscoveryEventKind::StateChanged(target) => {
                    match &guard.query {
                        Some(q) => q.matches_target(target),
                        None => guard.filter.matches(target),
                    }
                }
                DiscoveryEventKind::Removed(id) => {
                    let (id_backend, bare) = extract_backend_id_prefix(id);
                    let backend_ok = match &guard.query {
                        Some(q) => match q.dedupe {
                            DiscoveryDedupe::ByLogicalTarget => id_backend.is_none(),
                            DiscoveryDedupe::PerBackend => {
                                id_backend.is_some_and(|b| q.backends.for_os(q.os).contains(&b))
                            }
                        },
                        None => true,
                    };
                    let kind_ok =
                        if bare.starts_with("device:") || bare.starts_with("default_output:") {
                            guard.filter.include_devices
                        } else if bare.starts_with("input_device:")
                            || bare.starts_with("default_input:")
                        {
                            guard.filter.include_input_devices
                        } else if bare.starts_with("application:") {
                            guard.filter.include_applications
                        } else if bare.starts_with("process:") {
                            guard.filter.include_processes
                        } else {
                            guard.filter.include_applications
                                || guard.filter.include_processes
                                || guard.filter.include_devices
                                || guard.filter.include_input_devices
                        };
                    backend_ok && kind_ok
                }
                DiscoveryEventKind::DefaultDeviceChanged { .. } => guard.filter.include_devices,
                DiscoveryEventKind::DefaultInputChanged { .. } => {
                    guard.filter.include_input_devices
                }
                DiscoveryEventKind::WatcherFailed { .. }
                | DiscoveryEventKind::VisibilityChanged(_)
                | DiscoveryEventKind::ResnapshotRequired { .. } => true,
            };
            if !matches {
                continue;
            }
            push_event_to_subscriber_guard(&mut guard, revision, kind.clone());
        }
        revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{CaptureRequest, TargetPlan};

    fn win_backend_inventory(
        processes: Vec<RawProcessRecord>,
        devices: Vec<(&str, Option<&str>, bool)>,
    ) -> BackendRawInventory {
        BackendRawInventory {
            processes: processes
                .into_iter()
                .map(|process| RawBackendProcessRecord {
                    backend: BackendId::Wasapi,
                    process,
                })
                .collect(),
            devices: devices
                .into_iter()
                .map(|(id, name, is_def)| RawBackendDeviceRecord {
                    backend: BackendId::Wasapi,
                    direction: DeviceDirection::Output,
                    device_id: id.into(),
                    name: name.map(Into::into),
                    is_default: is_def,
                    is_monitor_sink: false,
                })
                .collect(),
        }
    }

    fn sample_processes() -> Vec<RawProcessRecord> {
        vec![
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 100,
                    generation: 1000,
                },
                parent_pid: None,
                app_id: Some("org.mozilla.firefox".into()),
                name: Some("Firefox".into()),
                executable_path: Some("C:\\Program Files\\Mozilla Firefox\\firefox.exe".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 200,
                    generation: 2000,
                },
                parent_pid: Some(100),
                app_id: None,
                name: Some("firefox-audio-helper".into()),
                executable_path: Some("C:\\Program Files\\Mozilla Firefox\\firefox.exe".into()),
                active_audio: Some(true),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 300,
                    generation: 3000,
                },
                parent_pid: None,
                app_id: Some("com.spotify.client".into()),
                name: Some("Spotify".into()),
                executable_path: Some("C:\\Users\\user\\AppData\\Spotify\\Spotify.exe".into()),
                active_audio: Some(false),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 400,
                    generation: 4000,
                },
                parent_pid: None,
                app_id: None,
                name: Some("Notepad".into()),
                executable_path: Some("C:\\Windows\\System32\\notepad.exe".into()),
                active_audio: None,
            },
        ]
    }

    #[test]
    fn discovery_rolls_up_child_helpers_and_respects_audio_filters() {
        let service = DiscoveryService::new_for_os(PlatformOs::Windows);
        let win_q = DiscoveryQuery::all_for_os(PlatformOs::Windows);
        service
            .update_backend_inventory(
                win_backend_inventory(
                    sample_processes(),
                    vec![("endpoint-speakers-0", Some("Speakers"), true)],
                ),
                &win_q,
            )
            .unwrap();

        // Default applications(): audio-bearing only (active or idle), child 200 rolled up to root 100
        let apps = service.applications(false, false).unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].pid, Some(100));
        assert_eq!(apps[0].active_audio, Some(true));
        assert_eq!(apps[0].scope, ProcessScope::Tree);
        assert_eq!(apps[1].pid, Some(300));
        assert_eq!(apps[1].active_audio, Some(false));

        let active_apps = service.applications(true, false).unwrap();
        assert_eq!(active_apps.len(), 1);
        assert_eq!(active_apps[0].pid, Some(100));

        let all_apps = service.applications(false, true).unwrap();
        assert_eq!(all_apps.len(), 3);
        assert_eq!(all_apps[2].pid, Some(400));
        assert_eq!(all_apps[2].active_audio, None);

        let audio_procs = service.processes(false, false).unwrap();
        assert_eq!(audio_procs.len(), 2);
        assert_eq!(audio_procs[0].pid, Some(200));
        assert_eq!(audio_procs[1].pid, Some(300));

        let out_devs = service.output_devices().unwrap();
        assert_eq!(out_devs.len(), 1);
        assert_eq!(
            out_devs[0].device_id.as_deref(),
            Some("endpoint-speakers-0")
        );
        assert_eq!(out_devs[0].name.as_deref(), Some("Speakers"));
        assert_eq!(out_devs[0].active_audio, None);

        assert_eq!(service.active_subscriber_count(), 0);

        application_rollup_stops_at_shells_launchers_and_disjoint_apps();
    }

    #[test]
    fn find_app_supports_pre_audio_and_rejects_ambiguity() {
        let service = DiscoveryService::new_for_os(PlatformOs::Windows);
        let win_q = DiscoveryQuery::all_for_os(PlatformOs::Windows);
        let mut procs = sample_processes();
        service
            .update_backend_inventory(
                win_backend_inventory(
                    procs.clone(),
                    vec![("endpoint-speakers-0", Some("Speakers"), true)],
                ),
                &win_q,
            )
            .unwrap();

        let notepad = service
            .find_app("notepad", ProcessScope::Tree, OnRootExit::Stop)
            .unwrap();
        assert_eq!(notepad.pid, Some(400));
        assert_eq!(notepad.kind, TargetKind::Application);

        let lowered = notepad.lower_to_process().unwrap();
        assert_eq!(lowered.kind, TargetKind::Process);
        assert_eq!(lowered.pid, Some(400));
        assert_eq!(lowered.process_start_time, Some(4000));

        assert_eq!(
            service.find_app("nonexistent", ProcessScope::Tree, OnRootExit::Stop),
            Err(FindAppError::NotFound)
        );

        procs.push(RawProcessRecord {
            identity: ProcessIdentity {
                pid: 401,
                generation: 4010,
            },
            parent_pid: None,
            app_id: None,
            name: Some("Notepad".into()),
            executable_path: Some("C:\\Windows\\System32\\notepad.exe".into()),
            active_audio: None,
        });
        service
            .update_backend_inventory(
                win_backend_inventory(procs, vec![("endpoint-speakers-0", Some("Speakers"), true)]),
                &win_q,
            )
            .unwrap();
        assert_eq!(
            service.find_app("Notepad", ProcessScope::Tree, OnRootExit::Stop),
            Err(FindAppError::AmbiguousMatch(2))
        );
    }

    #[test]
    fn bounded_256_event_subscription_overflow_and_cursor_resnapshot() {
        let service = DiscoveryService::new_for_os(PlatformOs::Windows);
        let win_q = DiscoveryQuery::all_for_os(PlatformOs::Windows);
        service
            .update_backend_inventory(
                win_backend_inventory(
                    sample_processes(),
                    vec![("endpoint-speakers-0", Some("Speakers"), true)],
                ),
                &win_q,
            )
            .unwrap();

        let sub1 = service
            .subscribe(DiscoveryFilter::applications(false, false), 256)
            .unwrap();
        let sub2 = service
            .subscribe(DiscoveryFilter::applications(false, false), 256)
            .unwrap();
        assert_eq!(service.active_subscriber_count(), 2);

        let sample_app = CaptureTarget::application(
            ProcessIdentity {
                pid: 500,
                generation: 5000,
            },
            Some("app.test".into()),
            Some("TestApp".into()),
            None,
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_metadata(
            Some("app.test".into()),
            Some("TestApp".into()),
            None,
            Some(true),
        );

        for _ in 0..256 {
            service.publish_event(DiscoveryEventKind::StateChanged(sample_app.clone()));
        }
        assert!(!sub1.is_overflowed());
        assert_eq!(sub1.pending_len(), 256);

        for _ in 0..10 {
            assert!(sub2.poll_event().is_some());
        }

        let overflow_rev =
            service.publish_event(DiscoveryEventKind::StateChanged(sample_app.clone()));
        assert!(sub1.is_overflowed());
        assert!(!sub2.is_overflowed());
        assert_eq!(sub2.pending_len(), 247);

        let mut saw_resnapshot = false;
        while let Some(ev) = sub1.poll_event() {
            if ev.kind
                == (DiscoveryEventKind::ResnapshotRequired {
                    minimum_revision: overflow_rev,
                })
            {
                saw_resnapshot = true;
            }
        }
        assert!(saw_resnapshot);

        let snap = sub1.resnapshot().unwrap();
        assert!(!sub1.is_overflowed());
        assert_eq!(snap.cursor.revision, overflow_rev);

        drop(sub1);
        assert_eq!(service.active_subscriber_count(), 1);
        drop(sub2);
        assert_eq!(service.active_subscriber_count(), 0);

        device_subscription_emits_added_removed_rename_and_respects_filter_and_resnapshot();
    }

    fn device_subscription_emits_added_removed_rename_and_respects_filter_and_resnapshot() {
        let service = DiscoveryService::new_for_os(PlatformOs::Windows);
        let win_q = DiscoveryQuery::all_for_os(PlatformOs::Windows);
        let procs = sample_processes();
        service
            .update_backend_inventory(
                win_backend_inventory(
                    procs.clone(),
                    vec![("endpoint-speakers-0", Some("Speakers"), true)],
                ),
                &win_q,
            )
            .unwrap();

        let dev_sub = service
            .subscribe(DiscoveryFilter::output_devices(), 32)
            .unwrap();
        let app_sub = service
            .subscribe(DiscoveryFilter::applications(false, false), 32)
            .unwrap();

        service
            .ingest_backend_inventory(
                win_backend_inventory(
                    procs.clone(),
                    vec![("endpoint-speakers-0", Some("Studio Monitors"), true)],
                ),
                &win_q,
            )
            .unwrap();
        let ev1 = dev_sub.poll_event().expect("expected rename StateChanged");
        assert!(matches!(
            &ev1.kind,
            DiscoveryEventKind::StateChanged(t)
                if t.kind == TargetKind::Device
                    && t.device_id.as_deref() == Some("endpoint-speakers-0")
                    && t.name.as_deref() == Some("Studio Monitors")
                    && t.active_audio.is_none()
        ));
        assert!(app_sub.poll_event().is_none());

        service
            .ingest_backend_inventory(
                win_backend_inventory(
                    procs.clone(),
                    vec![
                        ("endpoint-speakers-0", Some("Studio Monitors"), false),
                        ("endpoint-usb-dac-1", Some("USB DAC Headphones"), true),
                    ],
                ),
                &win_q,
            )
            .unwrap();
        let ev2 = dev_sub.poll_event().expect("expected DefaultDeviceChanged");
        assert_eq!(
            ev2.kind,
            DiscoveryEventKind::DefaultDeviceChanged {
                device_id: "endpoint-usb-dac-1".into()
            }
        );
        let ev3 = dev_sub.poll_event().expect("expected device Added");
        assert!(matches!(
            &ev3.kind,
            DiscoveryEventKind::Added(t)
                if t.kind == TargetKind::Device
                    && t.device_id.as_deref() == Some("endpoint-usb-dac-1")
                    && t.name.as_deref() == Some("USB DAC Headphones")
        ));
        assert!(ev3.revision > ev2.revision);
        assert!(app_sub.poll_event().is_none());

        let procs_without_spotify: Vec<RawProcessRecord> = procs
            .iter()
            .filter(|p| p.identity.pid != 300)
            .cloned()
            .collect();
        service
            .ingest_backend_inventory(
                win_backend_inventory(
                    procs_without_spotify.clone(),
                    vec![("endpoint-usb-dac-1", Some("USB DAC Headphones"), true)],
                ),
                &win_q,
            )
            .unwrap();
        let dev_removed = dev_sub.poll_event().expect("expected device Removed");
        assert_eq!(
            dev_removed.kind,
            DiscoveryEventKind::Removed("device:endpoint-speakers-0".into())
        );
        assert!(dev_sub.poll_event().is_none());

        let app_removed = app_sub.poll_event().expect("expected app Removed");
        assert_eq!(
            app_removed.kind,
            DiscoveryEventKind::Removed("application:300:3000".into())
        );
        assert!(app_sub.poll_event().is_none());

        service
            .ingest_backend_inventory(
                win_backend_inventory(
                    procs_without_spotify.clone(),
                    vec![
                        ("endpoint-speakers-0", Some("Speakers Replugged"), false),
                        ("endpoint-usb-dac-1", Some("USB DAC Headphones"), true),
                    ],
                ),
                &win_q,
            )
            .unwrap();
        for idx in 0..40 {
            let renamed = format!("Speakers Rename #{idx}");
            service
                .ingest_backend_inventory(
                    win_backend_inventory(
                        procs_without_spotify.clone(),
                        vec![
                            ("endpoint-speakers-0", Some(renamed.as_str()), false),
                            ("endpoint-usb-dac-1", Some("USB DAC Headphones"), true),
                        ],
                    ),
                    &win_q,
                )
                .unwrap();
        }
        assert!(dev_sub.is_overflowed());
        let snap = dev_sub.resnapshot().unwrap();
        assert!(!dev_sub.is_overflowed());
        assert_eq!(snap.targets.len(), 2);
        assert_eq!(snap.targets[0].name.as_deref(), Some("Speakers Rename #39"));
    }

    fn application_rollup_stops_at_shells_launchers_and_disjoint_apps() {
        let win_service = DiscoveryService::new_for_os(PlatformOs::Windows);
        let win_q = DiscoveryQuery::all_for_os(PlatformOs::Windows);

        // Real Windows desktop hierarchy:
        // explorer.exe (9880) -> Code.exe (3584) -> chrome.exe (20300, root) -> chrome.exe (2832, audio utility)
        let mut windows_procs = vec![
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 9880,
                    generation: 100,
                },
                parent_pid: None,
                app_id: Some("C:\\Windows\\explorer.exe".into()),
                name: Some("explorer.exe".into()),
                executable_path: Some("C:\\Windows\\explorer.exe".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 3584,
                    generation: 200,
                },
                parent_pid: Some(9880),
                app_id: Some("D:\\Programs\\Microsoft VS Code\\Code.exe".into()),
                name: Some("Code.exe".into()),
                executable_path: Some("D:\\Programs\\Microsoft VS Code\\Code.exe".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 20300,
                    generation: 300,
                },
                parent_pid: Some(3584),
                app_id: Some("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into()),
                name: Some("chrome.exe".into()),
                executable_path: Some(
                    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into(),
                ),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 2832,
                    generation: 310,
                },
                parent_pid: Some(20300),
                app_id: Some("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into()),
                name: Some("chrome.exe".into()),
                executable_path: Some(
                    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into(),
                ),
                active_audio: Some(true),
            },
        ];

        win_service
            .update_backend_inventory(
                win_backend_inventory(windows_procs.clone(), Vec::new()),
                &win_q,
            )
            .unwrap();

        let apps_playing = win_service.applications(false, false).unwrap();
        assert_eq!(apps_playing.len(), 1);
        assert_eq!(apps_playing[0].pid, Some(20300));
        assert_eq!(apps_playing[0].name.as_deref(), Some("chrome.exe"));
        assert_eq!(apps_playing[0].active_audio, Some(true));
        assert_eq!(apps_playing[0].scope, ProcessScope::Tree);

        let found_chrome = win_service
            .find_app("chrome", ProcessScope::Tree, OnRootExit::Stop)
            .unwrap();
        assert_eq!(found_chrome.pid, Some(20300));

        windows_procs[3].active_audio = Some(false);
        win_service
            .update_backend_inventory(win_backend_inventory(windows_procs, Vec::new()), &win_q)
            .unwrap();
        let apps_idle = win_service.applications(false, false).unwrap();
        assert_eq!(apps_idle.len(), 1);
        assert_eq!(apps_idle[0].pid, Some(20300));
        assert_eq!(apps_idle[0].active_audio, Some(false));
        assert!(win_service.applications(true, false).unwrap().is_empty());

        // A macOS bundle helper under launchd and Dock must roll up to its application.
        let mac_service = DiscoveryService::new_for_os(PlatformOs::MacOs);
        let mac_q = DiscoveryQuery::all_for_os(PlatformOs::MacOs);
        let mac_procs = vec![
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 1,
                    generation: 1,
                },
                parent_pid: None,
                app_id: Some("/sbin/launchd".into()),
                name: Some("launchd".into()),
                executable_path: Some("/sbin/launchd".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 50,
                    generation: 5,
                },
                parent_pid: Some(1),
                app_id: Some("com.apple.dock".into()),
                name: Some("Dock".into()),
                executable_path: Some(
                    "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock".into(),
                ),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 600,
                    generation: 60,
                },
                parent_pid: Some(50),
                app_id: Some("com.google.Chrome".into()),
                name: Some("Google Chrome".into()),
                executable_path: Some(
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
                ),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 601,
                    generation: 61,
                },
                parent_pid: Some(600),
                app_id: Some("com.google.Chrome.helper".into()),
                name: Some("Google Chrome Helper (Audio)".into()),
                executable_path: Some(
                    "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper".into(),
                ),
                active_audio: Some(true),
            },
        ];
        mac_service
            .update_backend_inventory(
                BackendRawInventory {
                    processes: mac_procs
                        .into_iter()
                        .map(|process| RawBackendProcessRecord {
                            backend: BackendId::CoreAudio,
                            process,
                        })
                        .collect(),
                    devices: Vec::new(),
                },
                &mac_q,
            )
            .unwrap();
        let mac_apps = mac_service.applications(true, false).unwrap();
        assert_eq!(mac_apps.len(), 1);
        assert_eq!(mac_apps[0].pid, Some(600));
        assert_eq!(mac_apps[0].name.as_deref(), Some("Google Chrome"));

        // Simultaneous Chrome + Chrome Canary on macOS (including code_sign_clone .app.bundle,
        // shared com.google.Chrome.helper bundle ID, multiple helper PIDs 73414/73415, and
        // ppid=None daemonized crashpad helper) plus disjoint Windows Chrome vs Chrome SxS.
        let chrome_and_canary = vec![
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 30208,
                    generation: 1_792_653_285_327_000,
                },
                parent_pid: None,
                app_id: Some("com.google.Chrome".into()),
                name: Some("Google Chrome".into()),
                executable_path: Some(
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
                ),
                active_audio: Some(false),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 30270,
                    generation: 1_792_653_293_838_207,
                },
                parent_pid: Some(30208),
                app_id: Some("com.google.Chrome.helper".into()),
                name: Some("Google Chrome Helper".into()),
                executable_path: Some(
                    "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/137.0.7151.69/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper".into(),
                ),
                active_audio: Some(true),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 73325,
                    generation: 1_793_092_920_556_483,
                },
                parent_pid: Some(30208),
                app_id: Some("com.google.Chrome.canary".into()),
                name: Some("Google Chrome Canary".into()),
                executable_path: Some(
                    "/private/var/folders/0l/8jqz15r56gx0jtj9wcf5kbgm0000gn/X/com.google.Chrome.canary.code_sign_clone/code_sign_clone.IR8PfD/Google Chrome Canary.app.bundle/Contents/MacOS/Google Chrome Canary".into(),
                ),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 73414,
                    generation: 1_793_092_954_861_719,
                },
                parent_pid: Some(73325),
                app_id: Some("com.google.Chrome.helper".into()),
                name: Some("Google Chrome Helper (Aperitif)".into()),
                executable_path: Some(
                    "/Applications/Google Chrome Canary.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/157.0.8100.0/Helpers/Google Chrome Helper (Aperitif).app/Contents/MacOS/Google Chrome Helper (Aperitif)".into(),
                ),
                active_audio: Some(true),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 73415,
                    generation: 1_793_092_954_862_068,
                },
                parent_pid: Some(73325),
                app_id: Some("com.google.Chrome.helper".into()),
                name: Some("Google Chrome Helper (Aperitif)".into()),
                executable_path: Some(
                    "/Applications/Google Chrome Canary.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/157.0.8100.0/Helpers/Google Chrome Helper (Aperitif).app/Contents/MacOS/Google Chrome Helper (Aperitif)".into(),
                ),
                active_audio: Some(false),
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 73332,
                    generation: 1_793_092_921_000_000,
                },
                parent_pid: None,
                app_id: Some(
                    "/Applications/Google Chrome Canary.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/157.0.8100.0/Helpers/chrome_crashpad_handler".into(),
                ),
                name: Some("chrome_crashpad_handler".into()),
                executable_path: Some(
                    "/Applications/Google Chrome Canary.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/157.0.8100.0/Helpers/chrome_crashpad_handler".into(),
                ),
                active_audio: None,
            },
        ];
        mac_service
            .update_backend_inventory(
                BackendRawInventory {
                    processes: chrome_and_canary
                        .into_iter()
                        .map(|process| RawBackendProcessRecord {
                            backend: BackendId::CoreAudio,
                            process,
                        })
                        .collect(),
                    devices: Vec::new(),
                },
                &mac_q,
            )
            .unwrap();

        let audio_apps = mac_service.applications(false, false).unwrap();
        assert_eq!(audio_apps.len(), 2);
        assert_eq!(audio_apps[0].pid, Some(30208));
        assert_eq!(audio_apps[0].name.as_deref(), Some("Google Chrome"));
        assert_eq!(audio_apps[0].active_audio, Some(true));
        assert_eq!(audio_apps[1].pid, Some(73325));
        assert_eq!(audio_apps[1].name.as_deref(), Some("Google Chrome Canary"));
        assert_eq!(audio_apps[1].active_audio, Some(true));

        let all_apps = mac_service.applications(false, true).unwrap();
        assert_eq!(all_apps.len(), 2);
        assert_eq!(
            mac_service
                .find_app("Google Chrome", ProcessScope::Tree, OnRootExit::Stop)
                .unwrap()
                .pid,
            Some(30208)
        );
        assert_eq!(
            mac_service
                .find_app("Google Chrome Canary", ProcessScope::Tree, OnRootExit::Stop)
                .unwrap()
                .pid,
            Some(73325)
        );

        // Disjoint Windows Chrome vs Chrome SxS (both named chrome.exe) must not merge
        let win_chrome = RawProcessRecord {
            identity: ProcessIdentity {
                pid: 1000,
                generation: 100,
            },
            parent_pid: None,
            app_id: Some("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into()),
            name: Some("chrome.exe".into()),
            executable_path: Some(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into(),
            ),
            active_audio: None,
        };
        let win_canary = RawProcessRecord {
            identity: ProcessIdentity {
                pid: 2000,
                generation: 200,
            },
            parent_pid: Some(1000),
            app_id: Some(
                "C:\\Users\\user\\AppData\\Local\\Google\\Chrome SxS\\Application\\chrome.exe"
                    .into(),
            ),
            name: Some("chrome.exe".into()),
            executable_path: Some(
                "C:\\Users\\user\\AppData\\Local\\Google\\Chrome SxS\\Application\\chrome.exe"
                    .into(),
            ),
            active_audio: Some(true),
        };
        assert!(!can_roll_up_to_parent(&win_canary, &win_chrome));

        // Runtime interpreter processes (gjs-console, python3.14) hosting distinct apps
        // must not merge across different app_ids, while same-app_id helpers do roll up.
        assert!(is_runtime_interpreter_stem("gjs-console"));
        assert!(is_runtime_interpreter_stem("python3.14"));
        assert!(!is_runtime_interpreter_stem("firefox"));

        let gjs_notifications = RawProcessRecord {
            identity: ProcessIdentity {
                pid: 1797,
                generation: 2170,
            },
            parent_pid: None,
            app_id: Some("org.gnome.Shell.Notifications".into()),
            name: Some("org.gnome.Shell.Notifications".into()),
            executable_path: Some("/usr/bin/gjs-console".into()),
            active_audio: None,
        };
        let gjs_decibels = RawProcessRecord {
            identity: ProcessIdentity {
                pid: 12918,
                generation: 1724290,
            },
            parent_pid: Some(1797),
            app_id: Some("org.gnome.Decibels".into()),
            name: Some("org.gnome.Decibels".into()),
            executable_path: Some("/usr/bin/gjs-console".into()),
            active_audio: Some(true),
        };
        let gjs_decibels_worker = RawProcessRecord {
            identity: ProcessIdentity {
                pid: 12950,
                generation: 1724300,
            },
            parent_pid: Some(12918),
            app_id: Some("org.gnome.Decibels".into()),
            name: Some("org.gnome.Decibels".into()),
            executable_path: Some("/usr/bin/gjs-console".into()),
            active_audio: Some(true),
        };
        assert!(!can_roll_up_to_parent(&gjs_decibels, &gjs_notifications));
        assert!(can_roll_up_to_parent(&gjs_decibels_worker, &gjs_decibels));

        assert!(is_shell_or_session_host_stem_for_os(
            PlatformOs::Windows,
            "explorer"
        ));
        assert!(!is_shell_or_session_host_stem_for_os(
            PlatformOs::Windows,
            "systemd"
        ));
        assert!(is_shell_or_session_host_stem_for_os(
            PlatformOs::Linux,
            "systemd"
        ));
        assert!(!is_shell_or_session_host_stem_for_os(
            PlatformOs::Linux,
            "explorer"
        ));
        assert!(is_shell_or_session_host_stem_for_os(
            PlatformOs::MacOs,
            "launchd"
        ));
        assert!(!is_shell_or_session_host_stem_for_os(
            PlatformOs::MacOs,
            "systemd"
        ));
        assert!(is_generic_system_bin_dir_for_os(
            PlatformOs::Windows,
            "c:/windows/system32"
        ));
        assert!(!is_generic_system_bin_dir_for_os(
            PlatformOs::Windows,
            "/usr/bin"
        ));
        assert!(is_generic_system_bin_dir_for_os(
            PlatformOs::Linux,
            "/usr/bin"
        ));
        assert!(is_generic_system_bin_dir_for_os(
            PlatformOs::MacOs,
            "/system/library/coreservices"
        ));

        let linux_tree = vec![
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 1,
                    generation: 1,
                },
                parent_pid: None,
                app_id: Some("/sbin/init".into()),
                name: Some("systemd".into()),
                executable_path: Some("/sbin/init".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 700,
                    generation: 70,
                },
                parent_pid: Some(1),
                app_id: Some("/usr/bin/bash".into()),
                name: Some("bash".into()),
                executable_path: Some("/usr/bin/bash".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 710,
                    generation: 71,
                },
                parent_pid: Some(700),
                app_id: Some("/opt/firefox/firefox".into()),
                name: Some("firefox".into()),
                executable_path: Some("/opt/firefox/firefox".into()),
                active_audio: None,
            },
            RawProcessRecord {
                identity: ProcessIdentity {
                    pid: 711,
                    generation: 72,
                },
                parent_pid: Some(710),
                app_id: Some("/opt/firefox/plugin-container".into()),
                name: Some("plugin-container".into()),
                executable_path: Some("/opt/firefox/plugin-container".into()),
                active_audio: Some(true),
            },
        ];
        let linux_apps = build_process_and_app_targets_for_os(
            PlatformOs::Linux,
            &linux_tree,
            &DiscoveryFilter::applications(true, false),
        )
        .unwrap();
        assert_eq!(linux_apps.len(), 1);
        assert_eq!(linux_apps[0].pid(), Some(710));
        assert_eq!(
            extract_outer_macos_bundle_name(
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"
            )
            .as_deref(),
            Some("google chrome canary.app")
        );
    }

    #[test]
    fn per_target_backends_and_discovery_query_dedupe_and_order() {
        use crate::schema::{
            CombineMode, CombineResolution, DefaultDeviceMode, DiagnosticOptions, ProcessTreeRange,
            ProcessTreeStrategy, TargetAcquisition,
        };

        let mic = CaptureTarget::microphone(DefaultDeviceMode::Follow);
        assert_eq!(mic.kind(), TargetKind::DefaultInput);
        assert_eq!(mic.id().as_ref(), "default_input:follow");
        mic.validate().unwrap();
        assert!(
            mic.diagnostic_summary(DiagnosticOptions::default())
                .contains("id=<redacted>")
        );

        let pinned_twice = mic
            .clone()
            .with_backend(BackendId::PipeWire)
            .with_backend(BackendId::PulseAudio);
        assert_eq!(pinned_twice.backend(), Some(BackendId::PulseAudio));
        assert_eq!(
            pinned_twice.id().as_ref(),
            "pulseaudio:default_input:follow"
        );
        let unpinned = pinned_twice.with_optional_backend(None);
        assert_eq!(unpinned.id().as_ref(), "default_input:follow");
        assert!(CaptureTarget::input_device("").is_err());

        let sck_app = CaptureTarget::application(
            ProcessIdentity {
                pid: 550,
                generation: 99_000,
            },
            Some("us.zoom.xos".into()),
            Some("Zoom".into()),
            None,
            ProcessScope::Exact,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_backend(BackendId::ScreenCaptureKit)
        .with_gain(0.8)
        .unwrap();
        let ca_mic = CaptureTarget::microphone(DefaultDeviceMode::Follow);

        let cross_req = CaptureRequest::new(vec![sck_app.clone(), ca_mic.clone()]);
        let mac_plan = cross_req.inspect_for_os(PlatformOs::MacOs).unwrap();
        assert_eq!(mac_plan.combine_resolution, CombineResolution::CspaMixer);
        assert_eq!(mac_plan.selected_backend, None); // cross-backend!
        assert_eq!(
            mac_plan.target_plans[0].resolved_backend,
            BackendId::ScreenCaptureKit
        );
        assert_eq!(
            mac_plan.target_plans[1],
            TargetPlan {
                target_index: 1,
                target_id: ca_mic.id().clone(),
                resolved_backend: BackendId::CoreAudio,
                acquisition: TargetAcquisition::DirectEndpoint,
                attempts: mac_plan.target_plans[1].attempts.clone(),
            }
        );

        let native_only_req =
            CaptureRequest::new(vec![sck_app, ca_mic]).with_combine_mode(CombineMode::NativeOnly);
        assert_eq!(
            native_only_req.inspect_for_os(PlatformOs::MacOs),
            Err(PlanError::UnsupportedCapturePlan)
        );

        let mic_only_req = CaptureRequest::single(
            CaptureTarget::microphone(DefaultDeviceMode::Pin).with_backend(BackendId::PipeWire),
        )
        .with_tree_strategy(ProcessTreeRange {
            start_level: ProcessTreeStrategy::NativeKernelCapture,
            fallback_limit: None,
        });
        let mic_linux_plan = mic_only_req.inspect_for_os(PlatformOs::Linux).unwrap();
        assert_eq!(
            mic_linux_plan.combine_resolution,
            CombineResolution::PassThrough(BackendId::PipeWire)
        );
        assert_eq!(
            mic_linux_plan.target_plans[0].acquisition,
            TargetAcquisition::DirectEndpoint
        );

        let bad_pin = CaptureRequest::single(
            CaptureTarget::microphone(DefaultDeviceMode::Follow)
                .with_backend(BackendId::PulseAudio),
        );
        assert_eq!(
            bad_pin.inspect_for_os(PlatformOs::Windows),
            Err(PlanError::InvalidBackend)
        );

        let service = DiscoveryService::new_for_os(PlatformOs::Linux);
        let input_sub = service
            .subscribe(DiscoveryFilter::input_devices(), 32)
            .unwrap();
        let query_pa_per_backend = DiscoveryQuery::all_for_os(PlatformOs::Linux)
            .with_backends(
                Backends::new(
                    None,
                    Some(vec![BackendId::PulseAudio, BackendId::PipeWire]),
                    None,
                )
                .unwrap(),
            )
            .with_dedupe(DiscoveryDedupe::PerBackend)
            .with_order(DiscoveryOrder::BackendThenKind);
        let pb_sub = service
            .subscribe_query(query_pa_per_backend.clone(), 32)
            .unwrap();

        let ff_proc = RawProcessRecord {
            identity: ProcessIdentity {
                pid: 4200,
                generation: 88_000,
            },
            parent_pid: None,
            app_id: Some("org.mozilla.firefox".into()),
            name: Some("Firefox".into()),
            executable_path: Some("/usr/lib64/firefox/firefox".into()),
            active_audio: Some(false),
        };
        let mut ff_proc_pa = ff_proc.clone();
        ff_proc_pa.active_audio = Some(true);

        let inv = BackendRawInventory {
            processes: vec![
                RawBackendProcessRecord {
                    backend: BackendId::PipeWire,
                    process: ff_proc,
                },
                RawBackendProcessRecord {
                    backend: BackendId::PulseAudio,
                    process: ff_proc_pa,
                },
            ],
            devices: vec![
                RawBackendDeviceRecord {
                    backend: BackendId::PipeWire,
                    direction: DeviceDirection::Input,
                    device_id: "alsa_input.usb-mic".into(),
                    name: Some("USB Studio Mic".into()),
                    is_default: true,
                    is_monitor_sink: false,
                },
                RawBackendDeviceRecord {
                    backend: BackendId::PulseAudio,
                    direction: DeviceDirection::Input,
                    device_id: "alsa_output.pci.analog-stereo.monitor".into(),
                    name: Some("Monitor of Speakers".into()),
                    is_default: false,
                    is_monitor_sink: true,
                },
                RawBackendDeviceRecord {
                    backend: BackendId::PulseAudio,
                    direction: DeviceDirection::Output,
                    device_id: "alsa_output.pci.analog-stereo".into(),
                    name: Some("Speakers".into()),
                    is_default: true,
                    is_monitor_sink: false,
                },
            ],
        };

        let query_pw_first = DiscoveryQuery::all_for_os(PlatformOs::Linux).with_backends(
            Backends::new(
                None,
                Some(vec![BackendId::PipeWire, BackendId::PulseAudio]),
                None,
            )
            .unwrap(),
        );
        service
            .ingest_backend_inventory(inv.clone(), &query_pw_first)
            .unwrap();

        let found_ff = service
            .find_app("Firefox", ProcessScope::Tree, OnRootExit::Stop)
            .unwrap();
        assert_eq!(found_ff.pid(), Some(4200));

        let cat_logical = service.discover_query(&query_pw_first).unwrap();
        assert_eq!(cat_logical.input_devices.len(), 1);
        assert_eq!(
            cat_logical.input_devices[0].device_id().map(|s| s.as_ref()),
            Some("alsa_input.usb-mic")
        );
        assert_eq!(cat_logical.applications.len(), 1);
        assert_eq!(
            cat_logical.applications[0].available_backends().as_ref(),
            &[BackendId::PipeWire, BackendId::PulseAudio]
        );
        assert_eq!(cat_logical.applications[0].active_audio(), Some(true));

        let cat_pb = service.discover_query(&query_pa_per_backend).unwrap();
        assert_eq!(cat_pb.applications.len(), 2);
        assert_ne!(cat_pb.applications[0].id(), cat_pb.applications[1].id());
        assert_eq!(cat_pb.targets[0].backend(), Some(BackendId::PulseAudio));

        let mut pb_added_ids = BTreeSet::new();
        while let Some(ev) = pb_sub.poll_event() {
            if let DiscoveryEventKind::Added(t) = ev.kind {
                assert!(t.backend().is_some());
                assert!(pb_added_ids.insert(t.id().to_string()));
            }
        }
        assert!(pb_added_ids.contains("pulseaudio:application:4200:88000"));
        assert!(pb_added_ids.contains("pipewire:application:4200:88000"));

        let mut inv2 = inv;
        inv2.devices[0].is_default = false;
        inv2.devices.push(RawBackendDeviceRecord {
            backend: BackendId::PipeWire,
            direction: DeviceDirection::Input,
            device_id: "alsa_input.headset-mic".into(),
            name: Some("Headset Mic".into()),
            is_default: true,
            is_monitor_sink: false,
        });
        service
            .ingest_backend_inventory(inv2, &query_pw_first)
            .unwrap();
        let mut saw_def_input_change = false;
        while let Some(ev) = input_sub.poll_event() {
            if ev.kind
                == (DiscoveryEventKind::DefaultInputChanged {
                    device_id: "alsa_input.headset-mic".into(),
                })
            {
                saw_def_input_change = true;
            }
        }
        assert!(saw_def_input_change);

        let mac_inv = BackendRawInventory {
            processes: vec![RawBackendProcessRecord {
                backend: BackendId::ScreenCaptureKit,
                process: RawProcessRecord {
                    identity: ProcessIdentity {
                        pid: 900,
                        generation: 9000,
                    },
                    parent_pid: None,
                    app_id: Some("com.apple.Music".into()),
                    name: Some("Music".into()),
                    executable_path: Some(
                        "/System/Applications/Music.app/Contents/MacOS/Music".into(),
                    ),
                    active_audio: Some(true),
                },
            }],
            devices: Vec::new(),
        };
        let mac_pb_query =
            DiscoveryQuery::all_for_os(PlatformOs::MacOs).with_dedupe(DiscoveryDedupe::PerBackend);
        let mac_cat = build_catalog_from_backend_inventory(&mac_inv, &mac_pb_query, 1).unwrap();
        assert_eq!(mac_cat.processes.len(), 0);
        assert_eq!(mac_cat.applications.len(), 1);
        assert_eq!(mac_cat.applications[0].scope(), ProcessScope::Exact);
        assert_eq!(
            mac_cat.applications[0].backend(),
            Some(BackendId::ScreenCaptureKit)
        );
    }
}
