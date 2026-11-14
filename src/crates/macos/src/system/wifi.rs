//! Wi-Fi interfaces through `CoreWLAN`: interface names, power and RSSI only.
//!
//! # Location Services evidence
//!
//! The macOS SDK header `CWInterface.h` documents which `CWInterface` reads
//! need Location Services authorization, and this adapter calls none of them:
//!
//! * `ssid`, `ssidData`, `bssid` and the country code say "not available unless
//!   Location Services is enabled and the user has authorized the calling app";
//! * the scan methods return networks without BSSID unless authorized;
//! * `interfaceName` and `powerOn` carry no such note, and `rssiValue` says only
//!   that it returns `0` when the interface is not participating in a network.
//!
//! `CWWiFiClient.interfaceNames` (macOS 13+) carries no such note either. The
//! adapter therefore reads `interfaceNames`, `powerOn` and `rssiValue`, never
//! requests Location, never touches `CLLocationManager` and never uses a
//! private Apple80211 API. The SSID is not read here at all; it comes only from
//! the gated `ipconfig getsummary` compatibility path owned by `stache-bar`.
//!
//! The change signal ([`super::wifi_events`]) uses only the `CWWiFiClient`
//! event-monitoring calls, which carry no such note either; its module lists
//! them.
//!
//! The header documents the contract; whether macOS 26 shows a prompt for these
//! three reads is **unverified on hardware** until the approved live run.

use std::fmt;

use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSArray, NSString};

use super::SystemReadError;

#[link(name = "CoreWLAN", kind = "framework")]
unsafe extern "C" {}

/// The longest interface name accepted (`en0`, `awdl0`, `bridge100`).
pub const MAX_INTERFACE_NAME: usize = 16;

/// The most interfaces one reading may carry.
pub const MAX_INTERFACES: usize = 8;

/// A validated network interface name: an ASCII letter followed by ASCII
/// letters and digits, at most [`MAX_INTERFACE_NAME`] bytes. Only a validated
/// name can become an argument of a child process.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InterfaceName(String);

/// Why a text is not an [`InterfaceName`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceNameError {
    /// Empty or longer than [`MAX_INTERFACE_NAME`].
    Length,
    /// Not an ASCII letter followed by ASCII letters and digits.
    Characters,
}

impl fmt::Display for InterfaceNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Length => "an interface name must be 1 to 16 bytes",
            Self::Characters => "an interface name must be ASCII letters and digits",
        })
    }
}

impl std::error::Error for InterfaceNameError {}

impl InterfaceName {
    /// Validates `text`.
    ///
    /// # Errors
    /// [`InterfaceNameError`] for an empty, long or non-alphanumeric name.
    pub fn parse(text: &str) -> Result<Self, InterfaceNameError> {
        if text.is_empty() || text.len() > MAX_INTERFACE_NAME {
            return Err(InterfaceNameError::Length);
        }
        let mut bytes = text.bytes();
        let leading = bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic());
        if !leading || !bytes.all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(InterfaceNameError::Characters);
        }
        Ok(Self(text.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for InterfaceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// Whether the radio is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiPower {
    /// The power state could not be read.
    Unknown,
    /// The radio is off.
    Off,
    /// The radio is on.
    On,
}

/// One Wi-Fi interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiInterfaceReading {
    /// The interface.
    pub name: InterfaceName,
    /// The radio power.
    pub power: WifiPower,
    /// The RSSI in dBm; `None` when `CoreWLAN` reports `0` (not associated) or
    /// the radio is not on.
    pub rssi: Option<i32>,
}

/// Reads the Wi-Fi interfaces.
pub trait WifiBackend: Send + Sync {
    /// The Wi-Fi interfaces (at most [`MAX_INTERFACES`]); empty when there are
    /// none.
    ///
    /// # Errors
    /// Why the interfaces could not be listed.
    fn interfaces(&self) -> Result<Vec<WifiInterfaceReading>, SystemReadError>;
}

/// The production `CoreWLAN` source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWifi;

impl WifiBackend for SystemWifi {
    fn interfaces(&self) -> Result<Vec<WifiInterfaceReading>, SystemReadError> {
        autoreleasepool(|_| read_interfaces())
    }
}

fn read_interfaces() -> Result<Vec<WifiInterfaceReading>, SystemReadError> {
    let class = AnyClass::get(c"CWWiFiClient")
        .ok_or_else(|| SystemReadError::new("CoreWLAN is not available"))?;
    // SAFETY: `sharedWiFiClient` is a documented class method returning an
    // autoreleased `CWWiFiClient` (or nil); the result is retained.
    let client: Option<Retained<AnyObject>> = unsafe { msg_send![class, sharedWiFiClient] };
    let client = client.ok_or_else(|| SystemReadError::new("CoreWLAN returned no client"))?;
    // SAFETY: `interfaceNames` is a documented instance method (macOS 13+)
    // returning `NSArray<NSString *> *` or nil.
    let names: Option<Retained<NSArray<NSString>>> = unsafe { msg_send![&*client, interfaceNames] };
    let Some(names) = names else {
        return Ok(Vec::new());
    };
    let mut readings = Vec::new();
    for index in 0..names.count().min(MAX_INTERFACES) {
        let name = names.objectAtIndex(index);
        let Ok(parsed) = InterfaceName::parse(&name.to_string()) else {
            continue;
        };
        // SAFETY: `interfaceWithName:` takes an `NSString *` and returns a
        // `CWInterface *` or nil.
        let interface: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*client, interfaceWithName: &*name] };
        let (power, rssi) = interface.map_or((WifiPower::Unknown, None), |interface| {
            // SAFETY: `powerOn` returns BOOL and `rssiValue` returns NSInteger;
            // neither needs Location Services (see the module documentation).
            let on: bool = unsafe { msg_send![&*interface, powerOn] };
            if !on {
                return (WifiPower::Off, None);
            }
            let rssi: isize = unsafe { msg_send![&*interface, rssiValue] };
            let rssi = i32::try_from(rssi).ok().filter(|value| *value != 0);
            (WifiPower::On, rssi)
        });
        readings.push(WifiInterfaceReading { name: parsed, power, rssi });
    }
    Ok(readings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_names_are_validated() {
        assert!(InterfaceName::parse("en0").is_ok());
        assert!(InterfaceName::parse("bridge100").is_ok());
        assert_eq!(InterfaceName::parse(""), Err(InterfaceNameError::Length));
        assert_eq!(
            InterfaceName::parse("abcdefghijklmnopq"),
            Err(InterfaceNameError::Length)
        );
        for bad in ["0en", "en 0", "en0;ls", "en0\n", "-n", "é0", "en/0", "en0 "] {
            assert!(InterfaceName::parse(bad).is_err(), "{bad:?}");
        }
    }
}
