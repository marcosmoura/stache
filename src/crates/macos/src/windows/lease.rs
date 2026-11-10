//! The `AXEnhancedUserInterface` suppression lease.
//!
//! Applications with the attribute on animate every geometry write themselves
//! and lag by hundreds of milliseconds. Before a burst of frame writes a shard
//! reads the attribute on the application element once; when it is on, the
//! shard turns it off and holds a [`Lease`] that owes it back. The lease lives
//! in the owner's per-application state: no lock, no cross-thread call, one
//! lease per tracked application.
//!
//! # Rules
//!
//! - The lease wraps frame writes only. `Raise`, hide and reads take none.
//! - It stays open while the application has pending frames or wrote within
//!   [`LEASE_IDLE_GRACE`], so chained batches toggle the attribute once.
//! - Only a lease this shard took is restored, only to `true`, only on the
//!   exact application instance. An application that was already off is never
//!   written.
//! - A third party turning the attribute back on is counted, not fought: the
//!   lease closes without a write and no new lease opens until the burst ends.
//! - While `VoiceOver` runs no attribute is touched (see [`VoiceOverProbe`]).
//! - A hard kill during a lease leaves the attribute off until the application
//!   relaunches. No journal is kept (documented limit).

use std::fmt::Debug;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};

use super::events::EnhancedUiNote;
use crate::ax::{AxError, ELEMENT_TIMEOUT, PRESS_TIMEOUT, WindowAx};

/// How long a lease stays open after its application's last frame write, so
/// back-to-back batches reuse it instead of toggling the attribute again.
pub const LEASE_IDLE_GRACE: Duration = Duration::from_millis(50);

/// How long an application is left alone after a failed or unsupported read, a
/// failed set or a failed restore, so a persistent failure is neither retried
/// nor reported at frame rate.
const FAILURE_BACKOFF: Duration = Duration::from_secs(1);

/// How long a [`SystemVoiceOverProbe`] answer is cached.
const VOICE_OVER_CACHE: Duration = Duration::from_secs(1);

/// Reports whether `VoiceOver` is running. Read from a shard thread, so it must
/// be cheap, thread-safe and never wait on the main thread.
pub trait VoiceOverProbe: Send + Sync + Debug {
    /// Whether `VoiceOver` is running now.
    fn running(&self) -> bool;
}

/// A probe that always answers "not running".
#[derive(Clone, Copy, Debug, Default)]
pub struct NoVoiceOver;

impl VoiceOverProbe for NoVoiceOver {
    fn running(&self) -> bool { false }
}

/// The system probe, cached for one second.
///
/// It reads `voiceOverOnOffKey` of `com.apple.universalaccess` through
/// `CFPreferences` after `CFPreferencesAppSynchronize`, on the calling shard
/// thread, and never touches main.
///
/// Unverified natively: the key is the one `VoiceOver` itself maintains; no
/// native check has confirmed it on a supported macOS release.
#[derive(Debug, Default)]
pub struct SystemVoiceOverProbe {
    cached: Mutex<Option<(Instant, bool)>>,
}

impl SystemVoiceOverProbe {
    /// A probe with an empty cache.
    #[must_use]
    pub const fn new() -> Self { Self { cached: Mutex::new(None) } }
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFPreferencesAppSynchronize(application_id: CFStringRef) -> u8;
    fn CFPreferencesCopyAppValue(key: CFStringRef, application_id: CFStringRef) -> CFTypeRef;
}

fn read_voice_over() -> bool {
    let domain = CFString::from_static_string("com.apple.universalaccess");
    let key = CFString::from_static_string("voiceOverOnOffKey");
    // SAFETY: both arguments are live CFStrings for the duration of the calls;
    // the callee neither retains nor consumes them. `CFPreferencesCopyAppValue`
    // follows the Create rule: a non-null result is owned and wrapped exactly
    // once. Both functions are documented thread-safe.
    let value = unsafe {
        CFPreferencesAppSynchronize(domain.as_concrete_TypeRef());
        let raw =
            CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), domain.as_concrete_TypeRef());
        if raw.is_null() {
            return false;
        }
        CFType::wrap_under_create_rule(raw)
    };
    value.downcast_into::<CFBoolean>().is_some_and(bool::from)
}

impl VoiceOverProbe for SystemVoiceOverProbe {
    fn running(&self) -> bool {
        let now = Instant::now();
        {
            let held = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, answer)) = *held
                && now.saturating_duration_since(at) < VOICE_OVER_CACHE
            {
                return answer;
            }
        }
        let answer = read_voice_over();
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, answer));
        answer
    }
}

/// An open lease: this shard turned the attribute off and owes it back. It
/// lives in the state of one application of one epoch, so the epoch and the
/// exact application instance are structural: a replaced instance has a new
/// state and never inherits it.
#[derive(Debug)]
struct Lease {
    last_write: Instant,
    /// Not before this instant is a failed restore tried again.
    retry_at: Option<Instant>,
    failure_reported: bool,
}

/// What a settle step left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Settled {
    /// No lease is open.
    None,
    /// The lease stays open (pending frames); nothing to wait for.
    Open,
    /// Re-check at this instant.
    Wait(Instant),
    /// The lease was restored or dropped.
    Closed,
    /// The restore failed; the lease is still owed.
    Owed(Option<Instant>),
}

/// The per-application lease state, owned by the shard.
#[derive(Debug, Default)]
pub(super) struct Eui {
    lease: Option<Lease>,
    /// No attribute call before this instant: observed off, unsupported,
    /// failed, `VoiceOver`, or re-enabled during the current burst.
    hold_until: Option<Instant>,
    consecutive: u32,
    voice_over_noted: bool,
}

/// The facts a step needs besides the element.
pub(super) struct Context<'a> {
    pub now: Instant,
    pub voice_over: &'a dyn VoiceOverProbe,
}

impl Eui {
    /// Whether a lease is open (owed back).
    pub(super) const fn is_open(&self) -> bool { self.lease.is_some() }

    fn hold(&mut self, until: Instant) {
        self.hold_until = Some(self.hold_until.map_or(until, |held| held.max(until)));
    }

    /// Runs before a frame write of the application: takes the lease when the
    /// attribute is on, otherwise leaves the attribute alone.
    pub(super) fn before_write<A: WindowAx>(
        &mut self,
        ax: &A,
        element: &A::Element,
        context: &Context<'_>,
        notes: &mut Vec<EnhancedUiNote>,
    ) {
        let now = context.now;
        if let Some(lease) = &mut self.lease {
            lease.last_write = now;
            return;
        }
        if self.hold_until.is_some_and(|until| now < until) {
            self.hold(now + LEASE_IDLE_GRACE);
            return;
        }
        if context.voice_over.running() {
            if !self.voice_over_noted {
                self.voice_over_noted = true;
                notes.push(EnhancedUiNote::VoiceOverSkipped);
            }
            self.hold(now + LEASE_IDLE_GRACE);
            return;
        }
        let read = ax
            .set_messaging_timeout(element, ELEMENT_TIMEOUT)
            .and_then(|()| ax.enhanced_user_interface(element));
        match read {
            Ok(false) => self.hold(now + LEASE_IDLE_GRACE),
            Ok(true) => {
                let set = ax
                    .set_messaging_timeout(element, PRESS_TIMEOUT)
                    .and_then(|()| ax.set_enhanced_user_interface(element, false));
                match set {
                    Ok(()) => {
                        self.lease = Some(Lease {
                            last_write: now,
                            retry_at: None,
                            failure_reported: false,
                        });
                        notes.push(EnhancedUiNote::Leased);
                    }
                    Err(error) => {
                        self.hold(now + FAILURE_BACKOFF);
                        notes.push(EnhancedUiNote::SetFailed(error));
                    }
                }
            }
            Err(error) if error.is_absent() => self.hold(now + FAILURE_BACKOFF),
            Err(error) => {
                self.hold(now + FAILURE_BACKOFF);
                notes.push(EnhancedUiNote::ReadFailed(error));
            }
        }
    }

    /// Runs after a frame write was sent. A final or direct write ends a
    /// batch for the window: the attribute is read once more, and a third party
    /// that turned it back on is counted and not fought.
    pub(super) fn after_write<A: WindowAx>(
        &mut self,
        ax: &A,
        element: &A::Element,
        now: Instant,
        ends_batch: bool,
        notes: &mut Vec<EnhancedUiNote>,
    ) {
        let Some(lease) = &mut self.lease else { return };
        lease.last_write = now;
        if !ends_batch {
            return;
        }
        let read = ax
            .set_messaging_timeout(element, ELEMENT_TIMEOUT)
            .and_then(|()| ax.enhanced_user_interface(element));
        if read == Ok(true) {
            // It is already on again: close without a write, and leave it
            // alone until this burst has ended.
            self.lease = None;
            self.consecutive = self.consecutive.saturating_add(1);
            self.hold(now + LEASE_IDLE_GRACE);
            notes.push(EnhancedUiNote::ReEnabled { consecutive: self.consecutive });
        }
    }

    /// Restores the lease when its application is idle (or `force`d), and
    /// reports what is left. A dead application drops the lease with no write.
    pub(super) fn settle<A: WindowAx>(
        &mut self,
        ax: &A,
        element: &A::Element,
        now: Instant,
        pending: bool,
        force: bool,
        notes: &mut Vec<EnhancedUiNote>,
    ) -> Settled {
        let Some(lease) = &mut self.lease else {
            return Settled::None;
        };
        if !force {
            if pending {
                return Settled::Open;
            }
            let due = lease.last_write + LEASE_IDLE_GRACE;
            if now < due {
                return Settled::Wait(due);
            }
            if let Some(retry) = lease.retry_at
                && now < retry
            {
                return Settled::Wait(retry);
            }
        }
        // Only what this shard changed: read first, so a dead application is
        // dropped and one already on is not written.
        let current = ax
            .set_messaging_timeout(element, ELEMENT_TIMEOUT)
            .and_then(|()| ax.enhanced_user_interface(element));
        if current == Ok(true) {
            // A third party turned it back on during the grace: no write, no
            // `Restored`, and it counts as a re-enable.
            self.lease = None;
            self.consecutive = self.consecutive.saturating_add(1);
            notes.push(EnhancedUiNote::ReEnabled { consecutive: self.consecutive });
            return Settled::Closed;
        }
        let outcome = match current {
            Err(AxError::InvalidElement) => Ok(false),
            Ok(_) | Err(_) => ax
                .set_messaging_timeout(element, PRESS_TIMEOUT)
                .and_then(|()| ax.set_enhanced_user_interface(element, true))
                .map(|()| true),
        };
        match outcome {
            Ok(wrote) => {
                if wrote {
                    notes.push(EnhancedUiNote::Restored);
                }
                self.lease = None;
                if wrote {
                    self.consecutive = 0;
                }
                Settled::Closed
            }
            Err(AxError::InvalidElement) => {
                self.lease = None;
                Settled::Closed
            }
            Err(error) => {
                let retry = now + FAILURE_BACKOFF;
                if !lease.failure_reported {
                    lease.failure_reported = true;
                    notes.push(EnhancedUiNote::RestoreFailed(error));
                }
                lease.retry_at = Some(retry);
                Settled::Owed(Some(retry))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::testing::{FakeNode, FakeWindowAx};

    #[test]
    fn the_idle_grace_is_the_documented_fifty_milliseconds() {
        assert_eq!(LEASE_IDLE_GRACE, Duration::from_millis(50));
    }

    #[test]
    fn a_lease_with_pending_frames_stays_open_however_old_its_last_write() {
        let ax = FakeWindowAx::new();
        let node = ax.add_node(FakeNode {
            enhanced_ui: Ok(true),
            ..FakeNode::default()
        });
        let element = ax.element(node);
        let start = Instant::now();
        let mut eui = Eui::default();
        let mut notes = Vec::new();
        let context = Context {
            now: start,
            voice_over: &NoVoiceOver,
        };
        eui.before_write(&ax, &element, &context, &mut notes);
        assert_eq!(notes, [EnhancedUiNote::Leased]);

        let later = start + LEASE_IDLE_GRACE * 20;
        assert_eq!(
            eui.settle(&ax, &element, later, true, false, &mut notes),
            Settled::Open
        );
        assert_eq!(
            ax.enhanced_ui_writes(),
            [(node, false)],
            "nothing restored while frames wait"
        );
        assert_eq!(
            eui.settle(&ax, &element, later, false, false, &mut notes),
            Settled::Closed
        );
        assert_eq!(ax.enhanced_ui_writes(), [(node, false), (node, true)]);
        assert_eq!(
            eui.settle(&ax, &element, later, false, false, &mut notes),
            Settled::None
        );
    }
}
