//! Process discovery and termination for workspace cleanup.
//!
//! Finds processes whose working directory is within a workspace path,
//! and provides graceful termination (SIGTERM + timeout + SIGKILL).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Information about a process running in a workspace.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: i32,
    pub name: String,
}

impl std::fmt::Display for ProcessInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (pid {})", self.name, self.pid)
    }
}

/// Error returned when processes are still running and `--kill` was not set.
#[derive(Debug, thiserror::Error)]
#[error("Processes still running in workspace:\n{}\nRerun with --kill to terminate these processes", processes.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"))]
pub struct WorkspaceProcessesRunning {
    pub processes: Vec<ProcessInfo>,
}

/// Find processes whose working directory is within the given workspace path.
///
/// Skips the current process. Returns an empty vec if enumeration fails.
pub fn find_workspace_processes(workspace_path: &Path) -> Vec<ProcessInfo> {
    let workspace_canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());

    let pids = list_all_pids();
    let current_pid = std::process::id() as i32;
    let mut processes = Vec::new();

    for pid in pids {
        if pid <= 0 || pid == current_pid {
            continue;
        }

        let cwd = match pidcwd(pid) {
            Some(cwd) => cwd,
            None => continue,
        };

        let cwd_canonical = cwd.canonicalize().unwrap_or(cwd);
        if cwd_canonical.starts_with(&workspace_canonical) {
            let name = process_name(pid).unwrap_or_else(|| "<unknown>".to_string());
            debug!("Found workspace process: {} (pid {})", name, pid);
            processes.push(ProcessInfo { pid, name });
        }
    }

    processes
}

/// Terminate processes: SIGTERM first, wait up to `timeout`, then SIGKILL survivors.
pub fn terminate_processes(processes: &[ProcessInfo], timeout: Duration) -> anyhow::Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    if processes.is_empty() {
        return Ok(());
    }

    // Send SIGTERM to all
    for proc in processes {
        info!("Sending SIGTERM to {}", proc);
        if let Err(e) = kill(Pid::from_raw(proc.pid), Signal::SIGTERM) {
            debug!(
                "SIGTERM failed for {}: {} (may have already exited)",
                proc, e
            );
        }
    }

    // Poll until all exit or timeout
    let start = Instant::now();
    loop {
        let alive: Vec<_> = processes
            .iter()
            .filter(|p| kill(Pid::from_raw(p.pid), None).is_ok())
            .collect();

        if alive.is_empty() {
            info!("All workspace processes terminated gracefully");
            return Ok(());
        }

        if start.elapsed() >= timeout {
            // SIGKILL survivors
            for proc in &alive {
                warn!("Force killing {}", proc);
                let _ = kill(Pid::from_raw(proc.pid), Signal::SIGKILL);
            }
            info!("Force-killed {} remaining process(es)", alive.len());
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------
// Platform-specific process enumeration
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn list_all_pids() -> Vec<i32> {
    use std::os::raw::c_int;

    extern "C" {
        fn proc_listallpids(buffer: *mut c_int, buffersize: c_int) -> c_int;
    }

    // First call with null to get count
    let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }

    let capacity = count as usize + 64;
    let mut pids = vec![0i32; capacity];
    let ret = unsafe {
        proc_listallpids(
            pids.as_mut_ptr(),
            (capacity * std::mem::size_of::<i32>()) as c_int,
        )
    };

    if ret <= 0 {
        return Vec::new();
    }

    pids.truncate(ret as usize);
    pids.retain(|&p| p > 0);
    pids
}

#[cfg(target_os = "macos")]
fn pidcwd(pid: i32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_void};

    const PROC_PIDVNODEPATHINFO: c_int = 9;
    const MAXPATHLEN: usize = 1024;

    // Struct layout from <sys/proc_info.h>:
    //   vnode_info       = 152 bytes (vinfo_stat + vi_type + vi_pad + vi_fsid)
    //   vnode_info_path  = vnode_info(152) + path[1024] = 1176 bytes
    //   proc_vnodepathinfo = cdir(1176) + rdir(1176) = 2352 bytes
    const VNODE_INFO_SIZE: usize = 152;
    const PROC_VNODEPATHINFO_SIZE: usize = (VNODE_INFO_SIZE + MAXPATHLEN) * 2;

    extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    let mut buf = [0u8; PROC_VNODEPATHINFO_SIZE];
    let ret = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDVNODEPATHINFO,
            0,
            buf.as_mut_ptr() as *mut c_void,
            PROC_VNODEPATHINFO_SIZE as c_int,
        )
    };

    if ret <= 0 {
        return None;
    }

    // CWD path is at offset VNODE_INFO_SIZE within pvi_cdir (the first vnode_info_path)
    let path_bytes = &buf[VNODE_INFO_SIZE..VNODE_INFO_SIZE + MAXPATHLEN];
    let cstr = unsafe { CStr::from_ptr(path_bytes.as_ptr() as *const c_char) };
    let path_str = cstr.to_str().ok()?;
    if path_str.is_empty() {
        None
    } else {
        Some(PathBuf::from(path_str))
    }
}

#[cfg(target_os = "macos")]
fn process_name(pid: i32) -> Option<String> {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_void};

    extern "C" {
        fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    }

    let mut buf = [0u8; 256];
    let ret = unsafe { proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };

    if ret <= 0 {
        return None;
    }

    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    cstr.to_str().ok().map(|s| s.to_string())
}

// Linux: read from /proc filesystem
#[cfg(target_os = "linux")]
fn list_all_pids() -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .collect()
}

#[cfg(target_os = "linux")]
fn pidcwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{}/cwd", pid)).ok()
}

#[cfg(target_os = "linux")]
fn process_name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_string())
}

// Fallback for unsupported platforms: process discovery is a no-op
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn list_all_pids() -> Vec<i32> {
    Vec::new()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pidcwd(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_name(_pid: i32) -> Option<String> {
    None
}
