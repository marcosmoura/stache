//! Process-id fallback for running applications that report none.
//!
//! `NSRunningApplication.processIdentifier` answers `-1` for some live,
//! finished-launching applications (observed for Xcode's sandboxed Device Hub).
//! Their windows can only be read through `AXUIElementCreateApplication(pid)`,
//! so the pid is resolved from the application's executable path, as legacy
//! did. The resolved pid is a native handle for Accessibility reads only: it is
//! never identity or authority (exact `NSRunningApplication` equality stays the
//! only identity). It is accepted only when **exactly one** live process runs
//! that executable; none or several resolve to nothing.

use std::ffi::{CStr, c_int, c_void};

/// `PROC_PIDPATHINFO_MAXSIZE` from `<sys/proc_info.h>` (4 * `MAXPATHLEN`).
const PATH_CAPACITY: usize = 4096;
/// Upper bound on the process table read once per resolution.
const PID_CAPACITY: usize = 8192;

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    /// `<libproc.h>`: writes live pids, returns how many were written.
    fn proc_listallpids(buffer: *mut c_void, buffersize: c_int) -> c_int;
    /// `<libproc.h>`: writes the NUL-terminated executable path of `pid`.
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
}

/// The pid to use for `application`: `reported` when positive, otherwise the
/// single live process running `executable`, or `None`.
#[must_use]
pub fn resolve(reported: i32, executable: Option<&str>) -> Option<i32> {
    if reported > 0 {
        return Some(reported);
    }
    let executable = executable?;
    unique_match(executable, &live_pids(), executable_path)
}

/// The one pid among `pids` whose executable path equals `executable`.
fn unique_match(
    executable: &str,
    pids: &[i32],
    path_of: impl Fn(i32) -> Option<Vec<u8>>,
) -> Option<i32> {
    let mut found = None;
    for &pid in pids {
        if pid <= 0 || path_of(pid).as_deref() != Some(executable.as_bytes()) {
            continue;
        }
        if found.is_some() {
            // Two processes run the same executable: ambiguous, resolve nothing.
            return None;
        }
        found = Some(pid);
    }
    found
}

fn live_pids() -> Vec<i32> {
    let mut pids = vec![0_i32; PID_CAPACITY];
    let Ok(bytes) = c_int::try_from(PID_CAPACITY * size_of::<i32>()) else {
        return Vec::new();
    };
    // SAFETY: `pids` is valid for `bytes` bytes of `pid_t` (i32) storage.
    let written = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    // The return value counts pids, not bytes.
    let count = usize::try_from(written).unwrap_or(0).min(PID_CAPACITY);
    pids.truncate(count);
    pids
}

fn executable_path(pid: i32) -> Option<Vec<u8>> {
    let mut buffer = vec![0_u8; PATH_CAPACITY];
    let capacity = u32::try_from(PATH_CAPACITY).ok()?;
    // SAFETY: `buffer` is valid for `capacity` bytes; the callee writes a
    // NUL-terminated path on success.
    let length = unsafe { proc_pidpath(pid, buffer.as_mut_ptr().cast(), capacity) };
    if length <= 0 {
        return None;
    }
    path_in(&buffer)
}

/// The NUL-terminated path at the start of `buffer`. Bounded by the buffer: no
/// NUL inside it is a failure, never an overread.
fn path_in(buffer: &[u8]) -> Option<Vec<u8>> {
    let path = CStr::from_bytes_until_nul(buffer).ok()?;
    Some(path.to_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(pid: i32) -> Option<Vec<u8>> {
        match pid {
            10 => Some(b"/Apps/Hub.app/Contents/MacOS/Hub".to_vec()),
            11 | 12 => Some(b"/Apps/Twin.app/Contents/MacOS/Twin".to_vec()),
            13 => Some(b"/Apps/Other".to_vec()),
            _ => None,
        }
    }

    #[test]
    fn a_path_is_read_up_to_its_nul_and_never_past_the_buffer() {
        assert_eq!(path_in(b"/Apps/Hub\0garbage\0"), Some(b"/Apps/Hub".to_vec()));
        assert_eq!(path_in(b"\0"), Some(Vec::new()));
        assert_eq!(path_in(b"/Apps/Hub"), None, "no terminator inside the buffer");
    }

    #[test]
    fn a_positive_reported_pid_is_used_as_is() {
        assert_eq!(resolve(677, None), Some(677));
    }

    #[test]
    fn a_missing_pid_without_an_executable_resolves_nothing() {
        assert_eq!(resolve(-1, None), None);
    }

    #[test]
    fn exactly_one_matching_process_resolves() {
        let pids = [0, -1, 10, 13, 99];
        assert_eq!(
            unique_match("/Apps/Hub.app/Contents/MacOS/Hub", &pids, paths),
            Some(10)
        );
    }

    #[test]
    fn no_or_several_matching_processes_resolve_nothing() {
        let pids = [10, 11, 12, 13];
        assert_eq!(unique_match("/Apps/None", &pids, paths), None);
        assert_eq!(
            unique_match("/Apps/Twin.app/Contents/MacOS/Twin", &pids, paths),
            None
        );
    }

    #[test]
    fn the_real_process_table_lists_this_process_and_its_path() {
        // Read-only libproc queries of this test process; parallel test
        // processes may share the executable, so uniqueness is not asserted.
        let me = i32::try_from(std::process::id()).expect("pid fits");
        assert!(live_pids().contains(&me));
        let path = executable_path(me).expect("own path");
        let exe = std::env::current_exe().expect("current exe");
        let exe = std::fs::canonicalize(exe).expect("canonical exe");
        assert_eq!(path, exe.as_os_str().as_encoded_bytes());
    }
}
