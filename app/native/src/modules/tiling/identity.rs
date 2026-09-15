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
