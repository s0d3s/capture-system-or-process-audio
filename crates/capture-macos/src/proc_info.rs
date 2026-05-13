#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::core::{
    discovery::extract_outer_macos_bundle_name, schema::ProcessIdentity, traits::CapabilityError,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BsdProcRecord {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    pub exe_path: Option<String>,
    pub start_time_us: u64,
}

#[cfg(any(test, feature = "native-tests"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MachUsageCounts {
    pub voluntary_ctxt_switches: u64,
    pub nonvoluntary_ctxt_switches: u64,
    pub user_time_ns: u64,
    pub system_time_ns: u64,
}

/// Convert a macOS dynamic `code_sign_clone` vnode path
/// (`/private/var/folders/.../X/<id>.code_sign_clone/code_sign_clone.<tag>/<App>.app.bundle/Contents/...`)
/// into its canonical `/Applications/<App>.app/Contents/...` path.
pub fn canonicalize_macos_code_sign_clone_path(raw_path: &str) -> Option<String> {
    if !raw_path.contains(".code_sign_clone/") {
        return None;
    }
    let idx = raw_path.find(".app.bundle/")?;
    let before = &raw_path[..idx];
    let app_name = before.rsplit('/').next()?;
    if app_name.is_empty() {
        return None;
    }
    let after = &raw_path[idx + ".app.bundle".len()..];
    Some(format!("/Applications/{app_name}.app{after}"))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use super::*;
    #[cfg(any(test, feature = "native-tests"))]
    use std::ffi::c_long;
    use std::ffi::{c_char, c_int, c_void};

    const PROC_ALL_PIDS: u32 = 1;
    const PROC_PIDTBSDINFO: c_int = 3;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;
    const SZOMB: u32 = 5;
    #[cfg(any(test, feature = "native-tests"))]
    const RUSAGE_SELF: c_int = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [c_char; 16],
        pbi_name: [c_char; 32],
        pbi_nfiles: u32,
        pbi_pgjobc: u32,
        pbi_e_tdev: u32,
        pbi_e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    #[cfg(any(test, feature = "native-tests"))]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TimeVal {
        tv_sec: c_long,
        tv_usec: i32,
    }

    #[cfg(any(test, feature = "native-tests"))]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RUsage {
        ru_utime: TimeVal,
        ru_stime: TimeVal,
        ru_maxrss: c_long,
        ru_ixrss: c_long,
        ru_idrss: c_long,
        ru_isrss: c_long,
        ru_minflt: c_long,
        ru_majflt: c_long,
        ru_nswap: c_long,
        ru_inblock: c_long,
        ru_oublock: c_long,
        ru_msgsnd: c_long,
        ru_msgrcv: c_long,
        ru_nsignals: c_long,
        ru_nvcsw: c_long,
        ru_nivcsw: c_long,
    }

    unsafe extern "C" {
        fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: c_int)
        -> c_int;
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
        #[cfg(any(test, feature = "native-tests"))]
        fn getrusage(who: c_int, usage: *mut RUsage) -> c_int;
    }

    pub fn inspect_bsd_process(pid: u32) -> Result<BsdProcRecord, CapabilityError> {
        if pid == 0 {
            return Err(CapabilityError::InvalidTarget);
        }
        let mut info: ProcBsdInfo = unsafe { std::mem::zeroed() };
        let expected = std::mem::size_of::<ProcBsdInfo>() as c_int;
        // SAFETY: `info` is a valid writable `ProcBsdInfo` struct of exact size `expected`.
        let ret = unsafe {
            proc_pidinfo(
                pid as c_int,
                PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut ProcBsdInfo).cast::<c_void>(),
                expected,
            )
        };
        if ret != expected || info.pbi_status == SZOMB || info.pbi_pid != pid {
            return Err(CapabilityError::NoSuchProcess);
        }
        let start_time_us = info
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec);
        if start_time_us == 0 {
            return Err(CapabilityError::InvalidTarget);
        }
        let c_chars_to_string = |buf: &[c_char]| -> String {
            let bytes: &[u8] =
                // SAFETY: `c_char` and `u8` have identical size and alignment.
                unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len()) };
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        };
        let name_str = c_chars_to_string(&info.pbi_name);
        let comm = if !name_str.trim().is_empty() {
            name_str
        } else {
            c_chars_to_string(&info.pbi_comm)
        };
        let mut path_buf = [0u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: `path_buf` has `PROC_PIDPATHINFO_MAXSIZE` writable bytes.
        let path_len = unsafe {
            proc_pidpath(
                pid as c_int,
                path_buf.as_mut_ptr().cast::<c_void>(),
                PROC_PIDPATHINFO_MAXSIZE as u32,
            )
        };
        let exe_path = if path_len > 0 {
            std::str::from_utf8(&path_buf[..(path_len as usize)])
                .ok()
                .map(|s| s.trim_end_matches('\0').to_owned())
                .filter(|s| !s.is_empty())
                .map(|raw| {
                    if let Some(canon) = canonicalize_macos_code_sign_clone_path(&raw)
                        && std::path::Path::new(&canon).exists()
                    {
                        canon
                    } else {
                        raw
                    }
                })
        } else {
            None
        };
        Ok(BsdProcRecord {
            pid,
            ppid: info.pbi_ppid,
            comm,
            exe_path,
            start_time_us,
        })
    }

    pub fn collect_bsd_proc_table() -> Result<BTreeMap<u32, BsdProcRecord>, CapabilityError> {
        // SAFETY: passing null buffer queries an upper bound on required byte capacity.
        let needed = unsafe { proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let cap = (needed as usize / std::mem::size_of::<i32>()) + 64;
        let mut buf = vec![0i32; cap];
        // SAFETY: `buf` has `cap * sizeof(i32)` valid writable bytes.
        let bytes = unsafe {
            proc_listpids(
                PROC_ALL_PIDS,
                0,
                buf.as_mut_ptr().cast::<c_void>(),
                (cap * std::mem::size_of::<i32>()) as c_int,
            )
        };
        if bytes <= 0 {
            return Err(CapabilityError::ServerUnavailable);
        }
        let count = bytes as usize / std::mem::size_of::<i32>();
        let mut map = BTreeMap::new();
        for pid in buf.into_iter().take(count).filter(|&p| p > 0) {
            let pid = pid as u32;
            if let Ok(rec) = inspect_bsd_process(pid) {
                map.insert(pid, rec);
            }
        }
        Ok(map)
    }

    #[cfg(any(test, feature = "native-tests"))]
    pub fn current_mach_usage_counts() -> Option<MachUsageCounts> {
        let mut usage: RUsage = unsafe { std::mem::zeroed() };
        // SAFETY: `usage` is a valid writable `RUsage` struct.
        let rc = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
        if rc != 0 {
            return None;
        }
        let user_ns = (usage.ru_utime.tv_sec.max(0) as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add((usage.ru_utime.tv_usec.max(0) as u64).saturating_mul(1_000));
        let sys_ns = (usage.ru_stime.tv_sec.max(0) as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add((usage.ru_stime.tv_usec.max(0) as u64).saturating_mul(1_000));
        Some(MachUsageCounts {
            voluntary_ctxt_switches: usage.ru_nvcsw.max(0) as u64,
            nonvoluntary_ctxt_switches: usage.ru_nivcsw.max(0) as u64,
            user_time_ns: user_ns,
            system_time_ns: sys_ns,
        })
    }
}

/// Inspect `(BsdProcRecord)` for `pid` on macOS (or deterministic fallback on non-macOS hosts).
pub fn inspect_bsd_process(pid: u32) -> Result<BsdProcRecord, CapabilityError> {
    if pid == 0 {
        return Err(CapabilityError::InvalidTarget);
    }
    #[cfg(target_os = "macos")]
    {
        imp::inspect_bsd_process(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(BsdProcRecord {
            pid,
            ppid: 1,
            comm: format!("proc-{pid}"),
            exe_path: Some(format!(
                "/Applications/Proc{pid}.app/Contents/MacOS/proc-{pid}"
            )),
            start_time_us: u64::from(pid) * 1_000,
        })
    }
}

pub fn inspect_process_executable(pid: u32) -> Result<(ProcessIdentity, String), CapabilityError> {
    let rec = inspect_bsd_process(pid)?;
    let exe = rec.exe_path.unwrap_or(rec.comm);
    Ok((
        ProcessIdentity {
            pid: rec.pid,
            generation: rec.start_time_us,
        },
        exe,
    ))
}

#[cfg(any(test, feature = "native-tests"))]
pub fn inspect_process(pid: u32) -> Result<ProcessIdentity, CapabilityError> {
    inspect_process_executable(pid).map(|(id, _)| id)
}

pub fn collect_bsd_proc_table() -> Result<BTreeMap<u32, BsdProcRecord>, CapabilityError> {
    #[cfg(target_os = "macos")]
    {
        imp::collect_bsd_proc_table()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut map = BTreeMap::new();
        let pid = std::process::id();
        if let Ok(rec) = inspect_bsd_process(pid) {
            map.insert(pid, rec);
        }
        Ok(map)
    }
}

#[cfg(any(test, feature = "native-tests"))]
pub fn current_mach_usage_counts() -> Option<MachUsageCounts> {
    #[cfg(target_os = "macos")]
    {
        imp::current_mach_usage_counts()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(MachUsageCounts::default())
    }
}

/// Discard stale Core Audio process objects whose PIDs are absent from the live process table.
pub fn filter_live_audio_process_objects(
    proc_map: &BTreeMap<u32, BsdProcRecord>,
    raw_objects: &[(u32, u32, bool)],
) -> Vec<(u32, u32, bool, u64)> {
    let mut live = Vec::with_capacity(raw_objects.len());
    for &(obj_id, pid, running) in raw_objects {
        if pid == 0 {
            continue;
        }
        if let Some(proc_rec) = proc_map.get(&pid) {
            live.push((obj_id, pid, running, proc_rec.start_time_us));
        }
    }
    live
}

/// Reject ancestors newer than their children to prevent PID-reuse adoption.
pub fn pid_belongs_to_target(
    proc_map: &BTreeMap<u32, BsdProcRecord>,
    root: ProcessIdentity,
    exact_only: bool,
    candidate_pid: u32,
) -> Result<bool, CapabilityError> {
    let Some(root_rec) = proc_map.get(&root.pid) else {
        return Err(CapabilityError::NoSuchProcess);
    };
    if root.generation != 0 && root_rec.start_time_us != root.generation {
        return Err(CapabilityError::PidReused);
    }
    let Some(mut current) = proc_map.get(&candidate_pid) else {
        return Ok(false);
    };
    if exact_only {
        return Ok(current.pid == root.pid && current.start_time_us == root_rec.start_time_us);
    }
    let root_bundle = root_rec
        .exe_path
        .as_deref()
        .and_then(extract_outer_macos_bundle_name);
    let cand_bundle = current
        .exe_path
        .as_deref()
        .and_then(extract_outer_macos_bundle_name);
    if let (Some(rb), Some(cb)) = (&root_bundle, &cand_bundle)
        && rb != cb
    {
        return Ok(false);
    }

    let mut visited = BTreeSet::new();
    loop {
        if current.pid == root.pid {
            return Ok(current.start_time_us >= root_rec.start_time_us);
        }
        if !visited.insert(current.pid) || current.ppid <= 1 || current.ppid == current.pid {
            break;
        }
        let Some(parent) = proc_map.get(&current.ppid) else {
            break;
        };
        if parent.start_time_us > current.start_time_us {
            return Ok(false);
        }
        current = parent;
    }

    if let (Some(rb), Some(cb)) = (&root_bundle, &cand_bundle)
        && rb == cb
        && let Some(cand_rec) = proc_map.get(&candidate_pid)
        && cand_rec.ppid == 1
        && cand_rec.exe_path != root_rec.exe_path
        && cand_rec.start_time_us >= root_rec.start_time_us
    {
        return Ok(true);
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_stale_post_exit_audio_objects_and_enforces_start_time_ancestry() {
        let mut map = BTreeMap::new();
        map.insert(
            100,
            BsdProcRecord {
                pid: 100,
                ppid: 1,
                comm: "RootApp".into(),
                exe_path: Some("/Applications/Root.app/Contents/MacOS/RootApp".into()),
                start_time_us: 1_000_000,
            },
        );
        map.insert(
            200,
            BsdProcRecord {
                pid: 200,
                ppid: 100,
                comm: "AudioHelper".into(),
                exe_path: Some("/Applications/Root.app/Contents/MacOS/AudioHelper".into()),
                start_time_us: 1_050_000,
            },
        );

        let raw_objs = vec![(105, 100, false), (106, 999, true), (107, 200, true)];
        let live_objs = filter_live_audio_process_objects(&map, &raw_objs);
        assert_eq!(
            live_objs,
            vec![(105, 100, false, 1_000_000), (107, 200, true, 1_050_000)]
        );

        let root_id = ProcessIdentity {
            pid: 100,
            generation: 1_000_000,
        };
        assert!(pid_belongs_to_target(&map, root_id, false, 100).unwrap());
        assert!(pid_belongs_to_target(&map, root_id, false, 200).unwrap());
        assert!(!pid_belongs_to_target(&map, root_id, true, 200).unwrap());

        let reused_root = ProcessIdentity {
            pid: 100,
            generation: 999_999,
        };
        assert_eq!(
            pid_belongs_to_target(&map, reused_root, false, 100),
            Err(CapabilityError::PidReused)
        );

        let clone_path = "/private/var/folders/0l/8jqz15r56gx0jtj9wcf5kbgm0000gn/X/com.google.Chrome.canary.code_sign_clone/code_sign_clone.IR8PfD/Google Chrome Canary.app.bundle/Contents/MacOS/Google Chrome Canary";
        assert_eq!(
            canonicalize_macos_code_sign_clone_path(clone_path).as_deref(),
            Some("/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary")
        );

        // Disjoint .app bundle child (e.g. Chrome Canary spawned by Chrome) is excluded from Chrome's tree
        map.insert(
            300,
            BsdProcRecord {
                pid: 300,
                ppid: 100,
                comm: "OtherApp".into(),
                exe_path: Some("/Applications/Other.app/Contents/MacOS/OtherApp".into()),
                start_time_us: 1_100_000,
            },
        );
        assert!(!pid_belongs_to_target(&map, root_id, false, 300).unwrap());

        // Same .app bundle daemonized helper (ppid = 1) is included in Root.app's tree
        map.insert(
            250,
            BsdProcRecord {
                pid: 250,
                ppid: 1,
                comm: "XpcHelper".into(),
                exe_path: Some(
                    "/Applications/Root.app/Contents/Frameworks/Helper.app/Contents/MacOS/XpcHelper"
                        .into(),
                ),
                start_time_us: 1_060_000,
            },
        );
        assert!(pid_belongs_to_target(&map, root_id, false, 250).unwrap());

        // Separate independent launch of the same main executable is NOT adopted into Root.app's tree
        map.insert(
            350,
            BsdProcRecord {
                pid: 350,
                ppid: 1,
                comm: "RootApp".into(),
                exe_path: Some("/Applications/Root.app/Contents/MacOS/RootApp".into()),
                start_time_us: 1_070_000,
            },
        );
        assert!(!pid_belongs_to_target(&map, root_id, false, 350).unwrap());
    }
}
