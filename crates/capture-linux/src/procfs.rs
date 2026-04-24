#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::core::{schema::ProcessIdentity, traits::CapabilityError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcStatRecord {
    pub pid: u32,
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub starttime_ticks: u64,
}

#[cfg(any(test, feature = "native-tests"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThreadSwitchCounts {
    pub voluntary_ctxt_switches: u64,
    pub nonvoluntary_ctxt_switches: u64,
}

/// Parse a `/proc/<pid>/stat` line safely even when `comm` contains spaces or parentheses.
/// Field 1 is `pid`, field 2 is `(comm)`, field 3 is `state`, field 4 is `ppid`, and
/// field 22 (index 19 after `')'`) is `starttime` in clock ticks since boot.
pub fn parse_proc_stat_line(line: &str) -> Result<ProcStatRecord, CapabilityError> {
    let trimmed = line.trim();
    let open = trimmed.find('(').ok_or(CapabilityError::InvalidTarget)?;
    let close = trimmed.rfind(')').ok_or(CapabilityError::InvalidTarget)?;
    if close <= open {
        return Err(CapabilityError::InvalidTarget);
    }
    let pid: u32 = trimmed[..open]
        .trim()
        .parse()
        .map_err(|_| CapabilityError::InvalidTarget)?;
    if pid == 0 {
        return Err(CapabilityError::InvalidTarget);
    }
    let comm = trimmed[open + 1..close].to_owned();
    let rest: Vec<&str> = trimmed[close + 1..].split_whitespace().collect();
    if rest.len() < 20 {
        return Err(CapabilityError::InvalidTarget);
    }
    let state = rest[0]
        .chars()
        .next()
        .ok_or(CapabilityError::InvalidTarget)?;
    if matches!(state, 'Z' | 'X' | 'x') {
        return Err(CapabilityError::NoSuchProcess);
    }
    let ppid: u32 = rest[1]
        .parse()
        .map_err(|_| CapabilityError::InvalidTarget)?;
    let starttime_ticks: u64 = rest[19]
        .parse()
        .map_err(|_| CapabilityError::InvalidTarget)?;
    if starttime_ticks == 0 {
        return Err(CapabilityError::InvalidTarget);
    }
    Ok(ProcStatRecord {
        pid,
        comm,
        state,
        ppid,
        starttime_ticks,
    })
}

/// Parse `voluntary_ctxt_switches` and `nonvoluntary_ctxt_switches` from `/proc/self/task/<tid>/status`.
#[cfg(any(test, feature = "native-tests"))]
pub fn parse_proc_status_switches(text: &str) -> ThreadSwitchCounts {
    let mut counts = ThreadSwitchCounts::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("voluntary_ctxt_switches:") {
            if let Ok(val) = rest.trim().parse::<u64>() {
                counts.voluntary_ctxt_switches = val;
            }
        } else if let Some(rest) = line.strip_prefix("nonvoluntary_ctxt_switches:")
            && let Ok(val) = rest.trim().parse::<u64>()
        {
            counts.nonvoluntary_ctxt_switches = val;
        }
    }
    counts
}

/// Read current process context switches from `/proc/self/status` on Linux.
#[cfg(any(test, feature = "native-tests"))]
pub fn current_process_switch_counts() -> Option<ThreadSwitchCounts> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    Some(parse_proc_status_switches(&text))
}

fn stem_lower_from_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

/// Resolve the effective display name and entrypoint path for a Linux process:
/// - When `raw_exe` (or `argv[0]`) is a shared language runtime interpreter (`gjs`, `gjs-console`,
///   `python3`, `node`, `java`, etc.), scans `cmdline` past interpreter flags (`-m`, `-u`, `-O`, etc.,
///   stopping on inline-code flags `-c`/`-e`) to resolve the executed script/module path and full name.
/// - Repairs 15-character Linux `TASK_COMM_LEN` truncation on `stat_comm` when `raw_exe` or `argv[0]`
///   provides the full untruncated basename.
pub fn resolve_linux_cmdline_target(
    stat_comm: &str,
    raw_exe: Option<&str>,
    cmdline_bytes: Option<&[u8]>,
) -> (String, Option<String>) {
    let args: Vec<&str> = cmdline_bytes
        .unwrap_or_default()
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();

    let exe_stem = raw_exe
        .map(stem_lower_from_path)
        .or_else(|| args.first().map(|a| stem_lower_from_path(a)));
    let is_interpreter = exe_stem
        .as_deref()
        .is_some_and(crate::core::discovery::is_runtime_interpreter_stem);

    if is_interpreter && args.len() >= 2 {
        let mut idx = 1usize;
        let mut resolved_target: Option<&str> = None;
        while idx < args.len() {
            let arg = args[idx].trim();
            if arg.is_empty() {
                idx += 1;
                continue;
            }
            if matches!(arg, "-c" | "-e" | "--eval" | "--print" | "-p") {
                break;
            }
            if matches!(
                arg,
                "-X" | "-W" | "--Warning" | "-cp" | "-classpath" | "--class-path"
            ) {
                idx += 2;
                continue;
            }
            if arg.starts_with('-') {
                idx += 1;
                continue;
            }
            if !arg.contains(['\n', '\r', ' ', '\t']) && arg.len() <= 512 {
                resolved_target = Some(arg);
            }
            break;
        }

        if let Some(target_arg) = resolved_target {
            let base = target_arg.rsplit('/').next().unwrap_or(target_arg);
            if !base.is_empty() {
                let effective_exe = if target_arg.starts_with('/') {
                    Some(target_arg.to_owned())
                } else {
                    raw_exe.map(str::to_owned)
                };
                return (base.to_owned(), effective_exe);
            }
        }
    }

    if stat_comm.len() >= 15 {
        if let Some(exe) = raw_exe {
            let exe_base = exe.rsplit('/').next().unwrap_or(exe);
            if exe_base.len() > stat_comm.len()
                && exe_base.starts_with(stat_comm)
                && !crate::core::discovery::is_runtime_interpreter_stem(
                    &exe_base.to_ascii_lowercase(),
                )
            {
                return (exe_base.to_owned(), Some(exe.to_owned()));
            }
        }
        if let Some(arg0) = args.first() {
            let arg0_token = arg0.split_whitespace().next().unwrap_or(arg0);
            let arg0_base = arg0_token.rsplit('/').next().unwrap_or(arg0_token);
            if arg0_base.len() > stat_comm.len() && arg0_base.starts_with(stat_comm) {
                let effective_exe = if arg0_token.starts_with('/') && raw_exe.is_none() {
                    Some(arg0_token.to_owned())
                } else {
                    raw_exe.map(str::to_owned)
                };
                return (arg0_base.to_owned(), effective_exe);
            }
        }
    }

    (stat_comm.to_owned(), raw_exe.map(str::to_owned))
}

/// Extract a reverse-DNS freedesktop application ID from `/proc/<pid>/cgroup` when the process
/// runs inside a systemd desktop scope or D-Bus activated service, such as:
/// - `dbus-:1.2-org.gnome.Decibels@0.service` -> `Some("org.gnome.Decibels")`
/// - `app-flatpak-com.spotify.Client-1234.scope` -> `Some("com.spotify.Client")`
/// - `app-gnome-org.gnome.Rhythmbox3-5678.scope` -> `Some("org.gnome.Rhythmbox3")`
pub fn extract_cgroup_app_id(cgroup_text: &str) -> Option<String> {
    for line in cgroup_text.lines() {
        let unit_raw = line.trim().rsplit('/').next().unwrap_or("");
        if unit_raw.is_empty() {
            continue;
        }
        let unit = unit_raw
            .strip_suffix(".service")
            .or_else(|| unit_raw.strip_suffix(".scope"))
            .unwrap_or(unit_raw);
        let unit_no_instance = unit.split('@').next().unwrap_or(unit);

        let candidate = if let Some(rest) = unit_no_instance.strip_prefix("dbus-:") {
            rest.split_once('-').map(|(_, app)| app)
        } else if let Some(rest) = unit_no_instance
            .strip_prefix("app-flatpak-")
            .or_else(|| unit_no_instance.strip_prefix("app-gnome-"))
            .or_else(|| unit_no_instance.strip_prefix("app-kde-"))
            .or_else(|| unit_no_instance.strip_prefix("app-"))
        {
            let trimmed = if let Some((before_dash, tail)) = rest.rsplit_once('-')
                && !tail.is_empty()
                && tail.chars().all(|c| c.is_ascii_digit())
            {
                before_dash
            } else {
                rest
            };
            Some(trimmed)
        } else {
            None
        };

        if let Some(raw_id) = candidate {
            let unescaped = raw_id.replace("\\x2d", "-");
            let clean = unescaped.trim();
            if clean.contains('.')
                && !clean.starts_with('.')
                && !clean.ends_with('.')
                && !clean.contains(['/', '\\', ' ', '\t'])
            {
                return Some(clean.to_owned());
            }
        }
    }
    None
}

pub fn inspect_process_cgroup_app_id(proc_root: &Path, pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let text = std::fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup")).ok()?;
    extract_cgroup_app_id(&text)
}

/// Inspect `(pid, starttime_ticks)` from `/proc/<pid>/stat` and resolve effective executable/script path.
pub fn inspect_process_in_proc_root(
    proc_root: &Path,
    pid: u32,
) -> Result<(ProcStatRecord, Option<String>), CapabilityError> {
    if pid == 0 {
        return Err(CapabilityError::InvalidTarget);
    }
    let pid_dir = proc_root.join(pid.to_string());
    let stat_text = std::fs::read_to_string(pid_dir.join("stat"))
        .map_err(|_| CapabilityError::NoSuchProcess)?;
    let mut record = parse_proc_stat_line(&stat_text)?;
    if record.pid != pid {
        return Err(CapabilityError::InvalidTarget);
    }
    let raw_exe = std::fs::read_link(pid_dir.join("exe"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .map(|s| s.strip_suffix(" (deleted)").unwrap_or(&s).to_owned());
    let cmdline_bytes = std::fs::read(pid_dir.join("cmdline")).ok();
    let (effective_name, effective_exe) =
        resolve_linux_cmdline_target(&record.comm, raw_exe.as_deref(), cmdline_bytes.as_deref());
    record.comm = effective_name;
    let exe_path = effective_exe.or_else(|| {
        std::fs::read_to_string(pid_dir.join("comm"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    });
    Ok((record, exe_path))
}

/// Inspect `(ProcessIdentity, executable_path)` for `pid` on Linux (or synthetic fallback on non-Linux hosts).
pub fn inspect_process_executable(pid: u32) -> Result<(ProcessIdentity, String), CapabilityError> {
    if pid == 0 {
        return Err(CapabilityError::InvalidTarget);
    }
    #[cfg(target_os = "linux")]
    {
        let (rec, exe) = inspect_process_in_proc_root(Path::new("/proc"), pid)?;
        let exe_str = exe.unwrap_or_else(|| rec.comm.clone());
        Ok((
            ProcessIdentity {
                pid: rec.pid,
                generation: rec.starttime_ticks,
            },
            exe_str,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok((
            ProcessIdentity {
                pid,
                generation: u64::from(pid) * 1000,
            },
            format!("/usr/bin/proc-{pid}"),
        ))
    }
}

#[cfg(any(test, feature = "native-tests"))]
pub fn inspect_process(pid: u32) -> Result<ProcessIdentity, CapabilityError> {
    inspect_process_executable(pid).map(|(id, _)| id)
}

/// Check whether `candidate_pid` belongs to `root` (`scope="exact"` requires `candidate_pid == root.pid`
/// and matching `starttime`; `scope="tree"` walks `ppid` upward in `proc_map`, requiring every ancestor's
/// `starttime_ticks <= child.starttime_ticks` to prevent PID-reuse cycle/adoption).
pub fn pid_belongs_to_target(
    proc_map: &BTreeMap<u32, ProcStatRecord>,
    root: ProcessIdentity,
    exact_only: bool,
    candidate_pid: u32,
) -> Result<bool, CapabilityError> {
    let Some(root_rec) = proc_map.get(&root.pid) else {
        return Err(CapabilityError::NoSuchProcess);
    };
    if root.generation != 0 && root_rec.starttime_ticks != root.generation {
        return Err(CapabilityError::PidReused);
    }
    let Some(mut current) = proc_map.get(&candidate_pid) else {
        return Ok(false);
    };
    if exact_only {
        return Ok(current.pid == root.pid && current.starttime_ticks == root_rec.starttime_ticks);
    }
    let mut visited = BTreeSet::new();
    loop {
        if current.pid == root.pid {
            return Ok(current.starttime_ticks >= root_rec.starttime_ticks);
        }
        if !visited.insert(current.pid) || current.ppid <= 1 || current.ppid == current.pid {
            return Ok(false);
        }
        let Some(parent) = proc_map.get(&current.ppid) else {
            return Ok(false);
        };
        if parent.starttime_ticks > current.starttime_ticks {
            return Ok(false);
        }
        current = parent;
    }
}

/// Enumerate `/proc/<pid>/stat` records only (without resolving `exe`/`cmdline` symlinks),
/// optimized for fast `pid_belongs_to_target` ancestry checks.
pub fn collect_proc_stat_table(
    proc_root: &Path,
) -> Result<BTreeMap<u32, ProcStatRecord>, CapabilityError> {
    let mut table = BTreeMap::new();
    let entries = std::fs::read_dir(proc_root).map_err(|_| CapabilityError::ServerUnavailable)?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            continue;
        };
        let Ok(pid) = text.parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        let stat_path = proc_root.join(text).join("stat");
        if let Ok(stat_text) = std::fs::read_to_string(stat_path)
            && let Ok(record) = parse_proc_stat_line(&stat_text)
            && record.pid == pid
        {
            table.insert(pid, record);
        }
    }
    Ok(table)
}

/// Enumerate all live processes under `proc_root` (typically `/proc`).
pub fn collect_proc_table(
    proc_root: &Path,
) -> Result<BTreeMap<u32, (ProcStatRecord, Option<String>)>, CapabilityError> {
    let mut table = BTreeMap::new();
    let entries = std::fs::read_dir(proc_root).map_err(|_| CapabilityError::ServerUnavailable)?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            continue;
        };
        let Ok(pid) = text.parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        if let Ok(pair) = inspect_process_in_proc_root(proc_root, pid) {
            table.insert(pid, pair);
        }
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_stat_handles_spaces_and_parens_in_comm_and_rejects_zombies() {
        let line = "4210 (Web Content (1)) S 4100 4100 4100 0 -1 4194304 120 0 0 0 15 4 0 0 20 0 8 0 987654 12345678 1024";
        let rec = parse_proc_stat_line(line).unwrap();
        assert_eq!(rec.pid, 4210);
        assert_eq!(rec.comm, "Web Content (1)");
        assert_eq!(rec.state, 'S');
        assert_eq!(rec.ppid, 4100);
        assert_eq!(rec.starttime_ticks, 987654);

        let zombie =
            "4210 (defunct) Z 4100 4100 4100 0 -1 4194304 120 0 0 0 15 4 0 0 20 0 8 0 987654 0 0";
        assert_eq!(
            parse_proc_stat_line(zombie),
            Err(CapabilityError::NoSuchProcess)
        );
    }

    #[test]
    fn ancestry_enforces_starttime_and_detects_pid_reuse() {
        let mut map = BTreeMap::new();
        map.insert(
            100,
            ProcStatRecord {
                pid: 100,
                comm: "browser".into(),
                state: 'S',
                ppid: 1,
                starttime_ticks: 1000,
            },
        );
        map.insert(
            200,
            ProcStatRecord {
                pid: 200,
                comm: "audio-helper".into(),
                state: 'S',
                ppid: 100,
                starttime_ticks: 1200,
            },
        );
        map.insert(
            300,
            ProcStatRecord {
                pid: 300,
                comm: "stale-parent-link".into(),
                state: 'S',
                ppid: 100,
                starttime_ticks: 900,
            },
        );

        let root = ProcessIdentity {
            pid: 100,
            generation: 1000,
        };
        assert_eq!(pid_belongs_to_target(&map, root, false, 100), Ok(true));
        assert_eq!(pid_belongs_to_target(&map, root, false, 200), Ok(true));
        assert_eq!(pid_belongs_to_target(&map, root, true, 200), Ok(false));
        assert_eq!(pid_belongs_to_target(&map, root, false, 300), Ok(false));

        let reused_root = ProcessIdentity {
            pid: 100,
            generation: 500,
        };
        assert_eq!(
            pid_belongs_to_target(&map, reused_root, false, 200),
            Err(CapabilityError::PidReused)
        );
    }

    #[test]
    fn resolves_interpreter_cmdline_entrypoints_and_extracts_cgroup_app_ids() {
        let decibels_cmdline =
            b"/usr/bin/gjs-console\0-m\0/usr/bin/org.gnome.Decibels\0--gapplication-service\0";
        let (name, exe) = resolve_linux_cmdline_target(
            "org.gnome.Decib",
            Some("/usr/bin/gjs-console"),
            Some(decibels_cmdline),
        );
        assert_eq!(name, "org.gnome.Decibels");
        assert_eq!(exe.as_deref(), Some("/usr/bin/org.gnome.Decibels"));

        let notif_cmdline =
            b"/usr/bin/gjs\0-m\0/usr/share/gnome-shell/org.gnome.Shell.Notifications\0";
        let (notif_name, notif_exe) =
            resolve_linux_cmdline_target("gjs", Some("/usr/bin/gjs-console"), Some(notif_cmdline));
        assert_eq!(notif_name, "org.gnome.Shell.Notifications");
        assert_eq!(
            notif_exe.as_deref(),
            Some("/usr/share/gnome-shell/org.gnome.Shell.Notifications")
        );

        let py_inline = b"/usr/bin/python3\0-c\0import time; time.sleep(1)\0";
        let (py_name, py_exe) =
            resolve_linux_cmdline_target("python3", Some("/usr/bin/python3.14"), Some(py_inline));
        assert_eq!(py_name, "python3");
        assert_eq!(py_exe.as_deref(), Some("/usr/bin/python3.14"));

        let (long_name, long_exe) = resolve_linux_cmdline_target(
            "ContinuityCaptu",
            Some("/usr/libexec/ContinuityCaptureAgent"),
            None,
        );
        assert_eq!(long_name, "ContinuityCaptureAgent");
        assert_eq!(
            long_exe.as_deref(),
            Some("/usr/libexec/ContinuityCaptureAgent")
        );

        let cgroup_dbus = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/dbus-:1.2-org.gnome.Decibels@0.service\n";
        assert_eq!(
            extract_cgroup_app_id(cgroup_dbus).as_deref(),
            Some("org.gnome.Decibels")
        );
        let cgroup_flatpak = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-com.spotify.Client-4821.scope\n";
        assert_eq!(
            extract_cgroup_app_id(cgroup_flatpak).as_deref(),
            Some("com.spotify.Client")
        );
        let cgroup_plain = "0::/user.slice/user-1000.slice/session-2.scope\n";
        assert_eq!(extract_cgroup_app_id(cgroup_plain), None);
    }
}
