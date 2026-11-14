//! Wi-Fi change signals through `CoreWLAN` event monitoring.
//!
//! [`WifiEventMonitor::start`] creates a `CWWiFiClient` instance Stache owns
//! (never the shared client, whose delegate belongs to nobody in particular),
//! sets a delegate object on it and starts monitoring the power, SSID, BSSID,
//! link and link-quality events. The delegate only classifies each callback
//! into a [`WifiSignalKind`] and calls the subscriber: it reads nothing,
//! allocates nothing and holds no lock. Dropping the monitor stops all
//! monitoring and clears the delegate.
//!
//! `clientConnectionInterrupted` is an identity signal: the header says the
//! registrations are restored and local state should be re-synced.
//! `clientConnectionInvalidated` is [`WifiSignalKind::Lost`]: the header says
//! the connection is permanently invalidated, so no further event arrives.
//!
//! # Location Services evidence
//!
//! In the macOS SDK header `CWWiFiClient.h`, `-startMonitoringEventWithType:error:`,
//! `-stopMonitoringEventWithType:error:`, `-stopMonitoringAllEventsAndReturnError:`,
//! the `delegate` property and every `CWEventDelegate` method used here carry no
//! entitlement or Location Services note (unlike the `CWInterface` reads listed
//! in [`super::wifi`]). The callbacks deliver only an interface name (and, for
//! link quality, an RSSI and transmit rate) that this source ignores. The source
//! never reads `ssid` or `bssid`, never scans and never touches
//! `CLLocationManager`; the network name still comes only from the gated
//! `ipconfig getsummary` path in `stache-bar`.
//!
//! A standalone probe on macOS 27.3 (2026-11-13) installed all five event types
//! without an entitlement and received link-quality callbacks on `CoreWLAN`'s
//! own serial queue with no run loop on any thread. The same calls inside the
//! app, and the power, SSID and link callbacks on a real network change, are
//! **unverified on hardware**.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{Arc, Once};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString};

use super::SystemReadError;

/// What a Wi-Fi change signal may mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WifiSignalKind {
    /// The network may have changed: power, SSID or link, or the connection
    /// to the Wi-Fi daemon was interrupted (events are re-registered).
    Identity,
    /// Only the signal quality changed: link quality (RSSI) or BSSID.
    Quality,
    /// The connection to the Wi-Fi daemon was permanently invalidated: no
    /// further signal will arrive from this source.
    Lost,
}

/// One `CWEventDelegate` callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiEvent {
    /// `clientConnectionInterrupted`.
    ConnectionInterrupted,
    /// `clientConnectionInvalidated`.
    ConnectionInvalidated,
    /// `powerStateDidChangeForWiFiInterfaceWithName:`.
    Power,
    /// `ssidDidChangeForWiFiInterfaceWithName:`.
    Ssid,
    /// `bssidDidChangeForWiFiInterfaceWithName:`.
    Bssid,
    /// `linkDidChangeForWiFiInterfaceWithName:`.
    Link,
    /// `linkQualityDidChangeForWiFiInterfaceWithName:rssi:transmitRate:`.
    LinkQuality,
}

/// The kind of signal one callback is.
#[must_use]
pub const fn classify(event: WifiEvent) -> WifiSignalKind {
    match event {
        WifiEvent::ConnectionInterrupted | WifiEvent::Power | WifiEvent::Ssid | WifiEvent::Link => {
            WifiSignalKind::Identity
        }
        WifiEvent::Bssid | WifiEvent::LinkQuality => WifiSignalKind::Quality,
        WifiEvent::ConnectionInvalidated => WifiSignalKind::Lost,
    }
}

/// Receives each signal. Called on a `CoreWLAN` thread: must be cheap and must
/// not block.
pub type WifiSignalSink = Arc<dyn Fn(WifiSignalKind) + Send + Sync>;

/// The `CWEventType` values monitored: power 1, SSID 2, BSSID 3, link 5, link
/// quality 6 (`CoreWLANTypes.h`).
const MONITORED_EVENTS: [isize; 5] = [1, 2, 3, 5, 6];

/// A running `CoreWLAN` event subscription. Dropping it stops all monitoring
/// and clears the delegate.
pub struct WifiEventMonitor {
    client: Retained<AnyObject>,
    // Keeps the delegate alive: `CWWiFiClient.delegate` is weak.
    _delegate: Retained<WifiEventDelegate>,
}

// SAFETY: the monitor only owns retained references; `CWWiFiClient` is not tied
// to a thread (it is a client of the Wi-Fi daemon's XPC connection), and the
// owner creates and drops it on the same provider worker.
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the retained client and delegate are only moved, never shared; see SAFETY"
)]
unsafe impl Send for WifiEventMonitor {}

impl WifiEventMonitor {
    /// Starts monitoring with `sink` as the subscriber.
    ///
    /// # Errors
    /// `CoreWLAN` is unavailable, no client could be created, or an event type
    /// could not be monitored. Nothing is left monitoring.
    pub fn start(sink: WifiSignalSink) -> Result<Self, SystemReadError> {
        autoreleasepool(|_| Self::start_in_pool(sink))
    }

    fn start_in_pool(sink: WifiSignalSink) -> Result<Self, SystemReadError> {
        let class = AnyClass::get(c"CWWiFiClient")
            .ok_or_else(|| SystemReadError::new("CoreWLAN is not available"))?;
        // SAFETY: `new` on `CWWiFiClient` returns an owned client or nil. This
        // is a fresh instance, never the shared client.
        let client: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
        let client = client.ok_or_else(|| SystemReadError::new("CoreWLAN returned no client"))?;
        let delegate = WifiEventDelegate::new(sink);
        // SAFETY: `delegate` is a weak `id` property; the monitor keeps the
        // delegate alive until `Drop` clears the property.
        let _: () = unsafe { msg_send![&*client, setDelegate: &*delegate] };
        let monitor = Self { client, _delegate: delegate };
        for event in MONITORED_EVENTS {
            let mut error: *mut NSError = ptr::null_mut();
            // SAFETY: documented instance method taking a `CWEventType`
            // (NSInteger) and an optional out-error.
            let started: Bool = unsafe {
                msg_send![&*monitor.client, startMonitoringEventWithType: event, error: &mut error]
            };
            if !started.as_bool() {
                // `monitor` drops here: stops everything and clears the delegate.
                return Err(SystemReadError::new(format!(
                    "Wi-Fi event {event} could not be monitored"
                )));
            }
        }
        Ok(monitor)
    }
}

impl Drop for WifiEventMonitor {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            let mut error: *mut NSError = ptr::null_mut();
            // SAFETY: documented instance methods on the owned client.
            unsafe {
                let _: Bool =
                    msg_send![&*self.client, stopMonitoringAllEventsAndReturnError: &mut error];
                let _: () = msg_send![&*self.client, setDelegate: ptr::null::<AnyObject>()];
            }
        });
    }
}

struct DelegateIvars {
    sink: WifiSignalSink,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class owns only the
    // subscriber, which is `Send + Sync`; `CoreWLAN` calls the selectors with
    // valid arguments on its own queue.
    #[unsafe(super(NSObject))]
    #[name = "StacheWifiEventDelegate"]
    #[ivars = DelegateIvars]
    struct WifiEventDelegate;

    unsafe impl NSObjectProtocol for WifiEventDelegate {}

    impl WifiEventDelegate {
        #[unsafe(method(clientConnectionInterrupted))]
        fn connection_interrupted(&self) { self.signal(WifiEvent::ConnectionInterrupted); }

        #[unsafe(method(clientConnectionInvalidated))]
        fn connection_invalidated(&self) { self.signal(WifiEvent::ConnectionInvalidated); }

        #[unsafe(method(powerStateDidChangeForWiFiInterfaceWithName:))]
        fn power_changed(&self, _interface: Option<&NSString>) { self.signal(WifiEvent::Power); }

        #[unsafe(method(ssidDidChangeForWiFiInterfaceWithName:))]
        fn ssid_changed(&self, _interface: Option<&NSString>) { self.signal(WifiEvent::Ssid); }

        #[unsafe(method(bssidDidChangeForWiFiInterfaceWithName:))]
        fn bssid_changed(&self, _interface: Option<&NSString>) { self.signal(WifiEvent::Bssid); }

        #[unsafe(method(linkDidChangeForWiFiInterfaceWithName:))]
        fn link_changed(&self, _interface: Option<&NSString>) { self.signal(WifiEvent::Link); }

        #[unsafe(method(linkQualityDidChangeForWiFiInterfaceWithName:rssi:transmitRate:))]
        fn link_quality_changed(&self, _interface: Option<&NSString>, _rssi: isize, _rate: f64) {
            self.signal(WifiEvent::LinkQuality);
        }
    }
);

impl WifiEventDelegate {
    fn new(sink: WifiSignalSink) -> Retained<Self> {
        declare_protocol();
        let allocated = Self::alloc().set_ivars(DelegateIvars { sink });
        // SAFETY: `alloc` returns an uninitialized instance of this exact
        // subclass and `init` is NSObject's designated initializer.
        unsafe { msg_send![super(allocated), init] }
    }

    /// Classifies and calls the subscriber; a panic never unwinds into
    /// Objective-C (unwind-enabled builds only; release aborts).
    fn signal(&self, event: WifiEvent) {
        let sink = &self.ivars().sink;
        let _ = catch_unwind(AssertUnwindSafe(|| sink(classify(event))));
    }
}

/// Declares `CWEventDelegate` conformance at runtime when the protocol is
/// registered, in case `CoreWLAN` checks conformance before dispatching.
fn declare_protocol() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Some(protocol) = AnyProtocol::get(c"CWEventDelegate") {
            let class: *const AnyClass = WifiEventDelegate::class();
            // SAFETY: the class is registered and the protocol is a live
            // runtime protocol; adding conformance only affects
            // `conformsToProtocol:`. The delegate implements every selector it
            // claims (all protocol methods are optional).
            let _ = unsafe { objc2::ffi::class_addProtocol(class.cast_mut(), protocol) };
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn identity_and_quality_events_are_classified() {
        for event in [
            WifiEvent::ConnectionInterrupted,
            WifiEvent::Power,
            WifiEvent::Ssid,
            WifiEvent::Link,
        ] {
            assert_eq!(classify(event), WifiSignalKind::Identity, "{event:?}");
        }
        for event in [WifiEvent::Bssid, WifiEvent::LinkQuality] {
            assert_eq!(classify(event), WifiSignalKind::Quality, "{event:?}");
        }
        assert_eq!(classify(WifiEvent::ConnectionInvalidated), WifiSignalKind::Lost);
        assert_eq!(MONITORED_EVENTS, [1, 2, 3, 5, 6]);
    }

    /// Sends each delegate selector to a delegate object directly: no client,
    /// no monitoring, no system state.
    #[test]
    fn each_delegate_selector_calls_the_subscriber_with_its_kind() {
        use WifiSignalKind::{Identity, Lost, Quality};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink: WifiSignalSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |kind| seen.lock().unwrap().push(kind))
        };
        let delegate = WifiEventDelegate::new(sink);
        let name = NSString::from_str("en0");
        // SAFETY: each selector is implemented by the delegate with these
        // argument types.
        unsafe {
            let _: () = msg_send![&*delegate, clientConnectionInterrupted];
            let _: () = msg_send![&*delegate, clientConnectionInvalidated];
            let _: () = msg_send![&*delegate, powerStateDidChangeForWiFiInterfaceWithName: &*name];
            let _: () = msg_send![&*delegate, ssidDidChangeForWiFiInterfaceWithName: &*name];
            let _: () = msg_send![&*delegate, bssidDidChangeForWiFiInterfaceWithName: &*name];
            let _: () = msg_send![&*delegate, linkDidChangeForWiFiInterfaceWithName: &*name];
            let _: () = msg_send![
                &*delegate,
                linkQualityDidChangeForWiFiInterfaceWithName: &*name,
                rssi: -60_isize,
                transmitRate: 300.0_f64
            ];
        }
        assert_eq!(*seen.lock().unwrap(), [
            Identity, Lost, Identity, Identity, Quality, Identity, Quality
        ]);
    }
}
