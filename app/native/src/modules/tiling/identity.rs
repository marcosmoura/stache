use objc::{msg_send, sel, sel_impl};
use serde::{Deserialize, Serialize};

/// Stable application identity combining PID and launch date.
///
/// Prevents PID-reuse races: after an app terminates the kernel may reuse
/// its PID for a different process. Binding ownership to `(pid, launch_date)`
/// ensures we never restore a wrong process that inherited the same PID.
///
/// `unsafe_derive_deserialize` is intentional: deserialization of the plain
/// `(pid, launch_date_bits)` pair is safe — the unsafe capture path is never
/// invoked by `serde`.
#[allow(clippy::unsafe_derive_deserialize)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AppIdentity {
    pub pid: i32,
    pub launch_date: LaunchDateBits,
}

/// Exact target for every delayed, cached, or external window operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WindowTarget {
    pub identity: AppIdentity,
    pub window_id: u32,
}

/// High-precision launch-date bits from
/// `NSRunningApplication.launchDate.timeIntervalSinceReferenceDate`, falling
/// back to the kernel process start time when `AppKit` has no launch date.
///
/// Stored as `f64::to_bits` for `Send + Sync + Copy`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LaunchDateBits(u64);

impl LaunchDateBits {
    /// Creates bits from a `timeIntervalSinceReferenceDate` value.
    /// Returns `None` if `t` is not finite and >0 (fail-closed on
    /// missing/null launch date).
    #[must_use]
    pub const fn from_time_interval_since_reference_date(t: f64) -> Option<Self> {
        if t.is_finite() && t > 0.0 {
            Some(Self(t.to_bits()))
        } else {
            None
        }
    }

    /// Returns the stored launch-date bit pattern for structured diagnostics.
    #[must_use]
    pub const fn bits(self) -> u64 { self.0 }
}

impl AppIdentity {
    /// Captures identity from an `NSRunningApplication` `ObjC` object.
    ///
    /// Uses the kernel process start time when `launchDate` is null (as it can
    /// be for Finder). Returns `None` if neither source provides a valid start
    /// time. Callers must skip the app rather than use a PID-only identity.
    ///
    /// # Safety
    ///
    /// `app` must be a valid non-null `*mut Object` pointing to an
    /// `NSRunningApplication` instance for the duration of this call.
    #[must_use]
    pub unsafe fn from_ns_running_app(app: *mut objc::runtime::Object) -> Option<Self> {
        unsafe {
            if app.is_null() {
                return None;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if pid <= 0 {
                return None;
            }
            Self::from_ns_running_app_with_pid(app, pid)
        }
    }

    /// Captures identity for a PID the caller has already resolved.
    ///
    /// Some Xcode applications (e.g. Device Hub, `com.apple.dt.Devices`)
    /// report `NSNotFound` (-1) from `processIdentifier` even though their
    /// windows belong to a real PID. Such an app cannot supply its own PID, so
    /// the caller's resolved PID is authoritative. A positive but conflicting
    /// `processIdentifier` still fails closed.
    ///
    /// # Safety
    ///
    /// `app` must be null or a valid `NSRunningApplication` `ObjC` object for
    /// the duration of this call.
    #[must_use]
    pub unsafe fn from_ns_running_app_with_pid(
        app: *mut objc::runtime::Object,
        pid: i32,
    ) -> Option<Self> {
        unsafe {
            if app.is_null() {
                return None;
            }
            let reported: i32 = msg_send![app, processIdentifier];
            let pid = resolve_identity_pid(reported, pid)?;
            let launch_date: *mut objc::runtime::Object = msg_send![app, launchDate];
            if launch_date.is_null() {
                let launch_date = kernel_launch_date(pid)?;
                return Some(Self { pid, launch_date });
            }
            let interval: f64 = msg_send![launch_date, timeIntervalSinceReferenceDate];
            let bits = LaunchDateBits::from_time_interval_since_reference_date(interval)?;
            Some(Self { pid, launch_date: bits })
        }
    }
}

/// Chooses the PID for an identity from the app's reported PID and a
/// caller-resolved PID.
///
/// Returns `None` when the caller's PID is not positive, or when the app
/// reports a different, positive PID (fail closed).
const fn resolve_identity_pid(reported_pid: i32, authoritative_pid: i32) -> Option<i32> {
    if authoritative_pid <= 0 {
        return None;
    }
    if reported_pid > 0 && reported_pid != authoritative_pid {
        return None;
    }
    Some(authoritative_pid)
}

/// Resolves the live process ID for an `NSRunningApplication`.
///
/// `NSRunningApplication.processIdentifier` reports `NSNotFound` (-1) for some
/// applications (e.g. Device Hub, `com.apple.dt.Devices`) even though the
/// process is running. When that happens, resolve the real PID by matching the
/// app's executable path against the kernel's process list.
///
/// # Safety
///
/// `app` must be a valid non-null `NSRunningApplication` `ObjC` object for the
/// duration of this call.
#[must_use]
pub unsafe fn process_id_for_app(app: *mut objc::runtime::Object) -> Option<i32> {
    unsafe {
        let reported: i32 = msg_send![app, processIdentifier];
        if reported > 0 {
            return Some(reported);
        }
        let path = executable_path_of(app)?;
        pid_for_executable_path(&path)
    }
}

/// Reads an application's executable path from its `executableURL`.
///
/// # Safety
///
/// `app` must be a valid non-null `NSRunningApplication` `ObjC` object.
unsafe fn executable_path_of(app: *mut objc::runtime::Object) -> Option<String> {
    unsafe {
        let url: *mut objc::runtime::Object = msg_send![app, executableURL];
        if url.is_null() {
            return None;
        }
        let path: *mut objc::runtime::Object = msg_send![url, path];
        if path.is_null() {
            return None;
        }
        Some(crate::platform::objc::nsstring_to_string(path))
    }
}

/// Finds the live PID whose executable path exactly matches `executable_path`.
fn pid_for_executable_path(executable_path: &str) -> Option<i32> {
    const PATH_BUFFER_SIZE: usize = 4096;

    let capacity = 4096usize;
    let mut pids = vec![0 as libc::pid_t; capacity];
    let buffer_size = i32::try_from(capacity * std::mem::size_of::<libc::pid_t>()).ok()?;
    // SAFETY: `pids` is valid for `buffer_size` bytes.
    let bytes = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), buffer_size) };
    if bytes <= 0 {
        return None;
    }
    let entry_size = std::mem::size_of::<libc::pid_t>();
    let count = usize::try_from(bytes).ok()? / entry_size;
    let buffer_len = u32::try_from(PATH_BUFFER_SIZE).ok()?;
    let mut path_buffer = vec![0 as libc::c_char; PATH_BUFFER_SIZE];

    for &pid in pids.iter().take(count) {
        if pid <= 0 {
            continue;
        }
        // SAFETY: `path_buffer` is valid for `buffer_len` bytes.
        let length =
            unsafe { libc::proc_pidpath(pid, path_buffer.as_mut_ptr().cast(), buffer_len) };
        if length <= 0 {
            continue;
        }
        // SAFETY: `proc_pidpath` wrote a NUL-terminated path into `path_buffer`.
        let path = unsafe { std::ffi::CStr::from_ptr(path_buffer.as_ptr()) };
        if path.to_bytes() == executable_path.as_bytes() {
            return Some(pid);
        }
    }
    None
}

/// Returns whether a process with `pid` is currently alive.
///
/// A PID we are not permitted to signal is still alive (`EPERM`).
#[must_use]
pub fn is_process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 only performs error checking.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0
}

/// Reads a process incarnation from the kernel without relying on `AppKit`'s
/// optional launch date. The timestamp uses the same reference epoch as `NSDate`.
fn kernel_launch_date(pid: i32) -> Option<LaunchDateBits> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: info points to writable storage of exactly the supplied size.
    let bytes = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size)
    };
    if bytes != size {
        return None;
    }
    // SAFETY: proc_pidinfo successfully initialized the complete structure.
    let info = unsafe { info.assume_init() };
    #[allow(clippy::cast_precision_loss)]
    let interval =
        info.pbi_start_tvsec as f64 - 978_307_200.0 + info.pbi_start_tvusec as f64 / 1_000_000.0;
    LaunchDateBits::from_time_interval_since_reference_date(interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_finder_identity_when_running() {
        use core_foundation::base::TCFType;

        objc::rc::autoreleasepool(|| unsafe {
            let class = objc::runtime::Class::get("NSRunningApplication").unwrap();
            let bundle_id = core_foundation::string::CFString::new("com.apple.finder");
            let apps: *mut objc::runtime::Object = msg_send![class,
                runningApplicationsWithBundleIdentifier: bundle_id.as_concrete_TypeRef()
            ];
            let count: usize = msg_send![apps, count];
            for index in 0..count {
                let app: *mut objc::runtime::Object = msg_send![apps, objectAtIndex: index];
                let first = AppIdentity::from_ns_running_app(app).expect(
                    "Finder must have a process identity even without an AppKit launch date",
                );
                assert_eq!(Some(first), AppIdentity::from_ns_running_app(app));
            }
        });
    }

    #[test]
    fn kernel_launch_date_is_stable_for_current_process() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let first = kernel_launch_date(pid).expect("current process has a kernel start time");
        assert_eq!(Some(first), kernel_launch_date(pid));
    }

    #[test]
    fn kernel_launch_date_rejects_invalid_processes() {
        assert!(kernel_launch_date(0).is_none());
        assert!(kernel_launch_date(-1).is_none());
        assert!(kernel_launch_date(i32::MAX).is_none());
    }

    #[test]
    fn capture_from_null_fails_closed() {
        let identity = unsafe { AppIdentity::from_ns_running_app(std::ptr::null_mut()) };
        assert!(identity.is_none());
    }

    /// Reads the executable path the kernel reports for `pid`.
    fn kernel_path_of(pid: i32) -> Option<String> {
        let mut buffer = vec![0 as libc::c_char; 4096];
        // SAFETY: `buffer` is valid for its full length.
        let length = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), 4096) };
        if length <= 0 {
            return None;
        }
        // SAFETY: `proc_pidpath` wrote a NUL-terminated path.
        let path = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
        Some(path.to_string_lossy().into_owned())
    }

    #[test]
    fn kernel_scan_resolves_a_live_process_for_a_known_path() {
        // Other test binaries may share this path (nextest runs one process per
        // test), so assert the scan returns *a* live process for the path.
        let path = kernel_path_of(i32::try_from(std::process::id()).unwrap()).expect("own path");

        let found = pid_for_executable_path(&path).expect("kernel scan resolves the path");

        assert!(is_process_alive(found));
        assert_eq!(kernel_path_of(found).as_deref(), Some(path.as_str()));
    }

    #[test]
    fn kernel_scan_rejects_unknown_paths() {
        assert!(pid_for_executable_path("/nonexistent/binary").is_none());
    }

    #[test]
    fn is_process_alive_detects_current_process_and_rejects_invalid_pids() {
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(is_process_alive(pid));
        assert!(!is_process_alive(0));
        assert!(!is_process_alive(-1));
    }

    #[test]
    fn resolve_identity_pid_prefers_caller_pid_when_app_reports_not_found() {
        // Device Hub reports NSNotFound (-1) from `processIdentifier` while its
        // windows belong to a real, caller-resolved PID.
        assert_eq!(resolve_identity_pid(-1, 62086), Some(62086));
    }

    #[test]
    fn resolve_identity_pid_accepts_matching_reported_pid() {
        assert_eq!(resolve_identity_pid(679, 679), Some(679));
    }

    #[test]
    fn resolve_identity_pid_rejects_conflicting_reported_pid() {
        assert_eq!(resolve_identity_pid(679, 1), None);
    }

    #[test]
    fn resolve_identity_pid_rejects_non_positive_authoritative_pid() {
        assert_eq!(resolve_identity_pid(-1, -1), None);
        assert_eq!(resolve_identity_pid(-1, 0), None);
    }

    #[test]
    fn identity_ord_deterministic() {
        let a = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(1000.0).unwrap(),
        };
        let b = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(2000.0).unwrap(),
        };
        let c = AppIdentity {
            pid: 20,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(1000.0).unwrap(),
        };
        assert!(a < b);
        assert!(a < c);
        assert!(b < c);
    }

    #[test]
    fn identity_equality_pid_and_launch_date() {
        let a = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(42.0).unwrap(),
        };
        let b = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(42.0).unwrap(),
        };
        let c = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(43.0).unwrap(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn launch_date_bits_rejects_non_positive_and_non_finite() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(LaunchDateBits::from_time_interval_since_reference_date(bad).is_none());
        }
    }

    #[test]
    fn window_target_distinguishes_same_window_id() {
        let a = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(42.0).unwrap(),
        };
        let b = AppIdentity {
            pid: 10,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(43.0).unwrap(),
        };
        assert_ne!(WindowTarget { identity: a, window_id: 99 }, WindowTarget {
            identity: b,
            window_id: 99
        },);
    }
}
