use std::process::Command;
use std::sync::OnceLock;

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use serde::Serialize;

use crate::platform::objc::nsstring;

#[link(name = "CoreWLAN", kind = "framework")]
unsafe extern "C" {}

const IFCONFIG: &str = "ifconfig";
const IPCONFIG: &str = "ipconfig";
const NETWORKSETUP: &str = "networksetup";
const WHICH: &str = "which";

static IFCONFIG_PATH: OnceLock<String> = OnceLock::new();
static IPCONFIG_PATH: OnceLock<String> = OnceLock::new();
static NETWORKSETUP_PATH: OnceLock<String> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum WifiStatus {
    Unknown,
    Off,
    Disconnected,
    Connected,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiInfo {
    pub status: WifiStatus,
    pub network_name: Option<String>,
    pub signal_strength: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct IpconfigSummary {
    is_wifi: bool,
    ssid: Option<String>,
    signal_strength: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WifiInterfaceSummary {
    name: String,
    summary: IpconfigSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WifiPower {
    Unknown,
    Off,
    On,
}

fn raw_command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        tracing::debug!(command, ?args, status = ?output.status, "wifi command failed");
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_command_path(output: &str) -> Option<String> {
    output.lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_string)
}

fn resolve_command_path(command: &str) -> String {
    raw_command_output(WHICH, &[command])
        .and_then(|output| parse_command_path(&output))
        .unwrap_or_else(|| command.to_string())
}

fn ifconfig_path() -> &'static str {
    IFCONFIG_PATH.get_or_init(|| resolve_command_path(IFCONFIG)).as_str()
}

fn ipconfig_path() -> &'static str {
    IPCONFIG_PATH.get_or_init(|| resolve_command_path(IPCONFIG)).as_str()
}

fn networksetup_path() -> &'static str {
    NETWORKSETUP_PATH.get_or_init(|| resolve_command_path(NETWORKSETUP)).as_str()
}

const fn empty_wifi_info(status: WifiStatus) -> WifiInfo {
    WifiInfo {
        status,
        network_name: None,
        signal_strength: None,
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn parse_wifi_interface_names(output: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut is_wifi_port = false;

    for line in output.lines().map(str::trim) {
        if let Some(port) = line.strip_prefix("Hardware Port: ") {
            is_wifi_port = matches!(port, "Wi-Fi" | "AirPort");
            continue;
        }

        if is_wifi_port && let Some(device) = line.strip_prefix("Device: ") {
            push_unique(&mut names, device);
            is_wifi_port = false;
        }
    }

    names
}

fn parse_interface_names(output: &str) -> Vec<String> {
    output.split_whitespace().map(str::to_string).collect()
}

fn get_wifi_hardware_interface_names() -> Vec<String> {
    raw_command_output(networksetup_path(), &["-listallhardwareports"])
        .map_or_else(Vec::new, |output| parse_wifi_interface_names(&output))
}

fn get_candidate_interface_names(wifi_hardware_interfaces: &[String]) -> Vec<String> {
    let mut names = Vec::new();

    for interface_name in wifi_hardware_interfaces {
        push_unique(&mut names, interface_name);
    }

    if !names.is_empty() {
        return names;
    }

    if let Some(output) = raw_command_output(ifconfig_path(), &["-l"]) {
        for interface_name in parse_interface_names(&output) {
            push_unique(&mut names, &interface_name);
        }
    }

    names
}

fn parse_key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.trim().split_once(':')?;
    Some((key.trim(), value.trim()))
}

fn parse_ssid(value: &str) -> Option<String> {
    let ssid = value.trim().trim_matches('"').trim();
    if ssid.is_empty() {
        None
    } else {
        Some(ssid.to_string())
    }
}

fn parse_signal_strength(value: &str) -> Option<i32> {
    value
        .split(|ch: char| !(ch == '-' || ch.is_ascii_digit()))
        .filter(|part| !part.is_empty() && *part != "-")
        .find_map(|part| part.parse().ok())
}

fn parse_ipconfig_summary(output: &str) -> IpconfigSummary {
    let mut summary = IpconfigSummary::default();

    for line in output.lines() {
        let Some((key, value)) = parse_key_value(line) else {
            continue;
        };

        if key.eq_ignore_ascii_case("InterfaceType") {
            summary.is_wifi = value.eq_ignore_ascii_case("WiFi")
                || value.eq_ignore_ascii_case("AirPort")
                || value.eq_ignore_ascii_case("IEEE80211");
            continue;
        }

        if key.eq_ignore_ascii_case("SSID") || key.eq_ignore_ascii_case("SSID_STR") {
            summary.ssid = parse_ssid(value);
            continue;
        }

        if key.to_ascii_lowercase().contains("rssi")
            && let Some(signal_strength) = parse_signal_strength(value)
        {
            summary.signal_strength = Some(signal_strength);
        }
    }

    if summary.ssid.is_some() {
        summary.is_wifi = true;
    }

    summary
}

fn get_ipconfig_summary(interface_name: &str) -> Option<IpconfigSummary> {
    raw_command_output(ipconfig_path(), &["getsummary", interface_name])
        .map(|output| parse_ipconfig_summary(&output))
}

fn collect_wifi_summaries(interface_names: &[String]) -> Vec<WifiInterfaceSummary> {
    interface_names
        .iter()
        .filter_map(|name| {
            let summary = get_ipconfig_summary(name)?;
            if summary.is_wifi || summary.ssid.is_some() || summary.signal_strength.is_some() {
                Some(WifiInterfaceSummary { name: name.clone(), summary })
            } else {
                None
            }
        })
        .collect()
}

fn select_connected_wifi_interface(
    summaries: &[WifiInterfaceSummary],
) -> Option<&WifiInterfaceSummary> {
    summaries
        .iter()
        .find(|interface| interface.summary.is_wifi && interface.summary.ssid.is_some())
}

fn fallback_interface_names(
    wifi_hardware_interfaces: &[String],
    summaries: &[WifiInterfaceSummary],
) -> Vec<String> {
    let mut names = Vec::new();

    for interface_name in wifi_hardware_interfaces {
        push_unique(&mut names, interface_name);
    }

    for interface in summaries.iter().filter(|interface| interface.summary.is_wifi) {
        push_unique(&mut names, &interface.name);
    }

    names
}

fn parse_wifi_power(output: &str) -> WifiPower {
    output
        .lines()
        .filter_map(parse_key_value)
        .find_map(|(_key, value)| match value {
            v if v.eq_ignore_ascii_case("On") => Some(WifiPower::On),
            v if v.eq_ignore_ascii_case("Off") => Some(WifiPower::Off),
            _ => None,
        })
        .unwrap_or(WifiPower::Unknown)
}

fn get_wifi_power(interface_name: &str) -> WifiPower {
    raw_command_output(networksetup_path(), &["-getairportpower", interface_name])
        .map_or(WifiPower::Unknown, |output| parse_wifi_power(&output))
}

fn get_corewlan_rssi(interface_name: &str) -> Option<i32> {
    let interface_name = interface_name.to_string();
    crate::platform::thread::dispatch_on_main_sync(move || {
        get_corewlan_rssi_on_main(&interface_name)
    })
}

fn get_corewlan_rssi_on_main(interface_name: &str) -> Option<i32> {
    std::panic::catch_unwind(|| unsafe { get_corewlan_rssi_unchecked(interface_name) })
        .unwrap_or_else(|_| {
            tracing::warn!(interface_name, "CoreWLAN RSSI query panicked");
            None
        })
}

unsafe fn get_corewlan_rssi_unchecked(interface_name: &str) -> Option<i32> {
    let client: *mut Object = unsafe { msg_send![class!(CWWiFiClient), sharedWiFiClient] };
    if client.is_null() {
        tracing::debug!(interface_name, "CWWiFiClient returned null");
        return None;
    }

    let interface_name = unsafe { nsstring(interface_name) };
    let interface: *mut Object = unsafe { msg_send![client, interfaceWithName: interface_name] };
    if interface.is_null() {
        tracing::debug!("CoreWLAN interfaceWithName returned null");
        return None;
    }

    let power_on: bool = unsafe { msg_send![interface, powerOn] };
    if !power_on {
        return None;
    }

    let rssi: isize = unsafe { msg_send![interface, rssiValue] };
    let Ok(rssi) = i32::try_from(rssi) else {
        return None;
    };

    (rssi != 0).then_some(rssi)
}

fn connected_wifi_info(interface: &WifiInterfaceSummary) -> WifiInfo {
    WifiInfo {
        status: WifiStatus::Connected,
        network_name: interface.summary.ssid.clone(),
        signal_strength: interface
            .summary
            .signal_strength
            .or_else(|| get_corewlan_rssi(&interface.name)),
    }
}

fn non_connected_wifi_info(
    wifi_hardware_interfaces: &[String],
    summaries: &[WifiInterfaceSummary],
) -> WifiInfo {
    let fallback_names = fallback_interface_names(wifi_hardware_interfaces, summaries);
    if fallback_names.is_empty() {
        return empty_wifi_info(WifiStatus::Unknown);
    }

    let mut saw_off = false;
    let mut saw_unknown = false;

    for interface_name in fallback_names {
        match get_wifi_power(&interface_name) {
            WifiPower::On => {
                return WifiInfo {
                    status: WifiStatus::Disconnected,
                    network_name: None,
                    signal_strength: get_corewlan_rssi(&interface_name),
                };
            }
            WifiPower::Off => saw_off = true,
            WifiPower::Unknown => saw_unknown = true,
        }
    }

    if saw_off {
        return empty_wifi_info(WifiStatus::Off);
    }

    if saw_unknown || summaries.iter().any(|interface| interface.summary.is_wifi) {
        return empty_wifi_info(WifiStatus::Disconnected);
    }

    empty_wifi_info(WifiStatus::Unknown)
}

/// Gets the current Wi-Fi status.
///
/// The shell/CoreWLAN work is blocking, so it runs on a blocking thread via
/// `spawn_blocking`. A synchronous Tauri command would execute on the main
/// thread and stall the UI; an `async` command without `spawn_blocking`
/// would occupy an async-runtime worker with blocking I/O.
#[tauri::command]
pub async fn get_wifi_info() -> WifiInfo {
    tauri::async_runtime::spawn_blocking(get_wifi_info_blocking)
        .await
        .unwrap_or_else(|_| empty_wifi_info(WifiStatus::Unknown))
}

/// Blocking Wi-Fi status computation. Runs on the blocking thread pool.
#[must_use]
fn get_wifi_info_blocking() -> WifiInfo {
    let wifi_hardware_interfaces = get_wifi_hardware_interface_names();
    let candidate_interfaces = get_candidate_interface_names(&wifi_hardware_interfaces);
    let summaries = collect_wifi_summaries(&candidate_interfaces);

    if let Some(interface) = select_connected_wifi_interface(&summaries) {
        return connected_wifi_info(interface);
    }

    non_connected_wifi_info(&wifi_hardware_interfaces, &summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wifi_interface_names_finds_wifi_device() {
        let output = "Hardware Port: Ethernet\nDevice: en4\n\nHardware Port: Wi-Fi\nDevice: en0\n";
        assert_eq!(parse_wifi_interface_names(output), vec!["en0".to_string()]);
    }

    #[test]
    fn parse_wifi_interface_names_finds_airport_device() {
        let output = "Hardware Port: AirPort\nDevice: en1\n";
        assert_eq!(parse_wifi_interface_names(output), vec!["en1".to_string()]);
    }

    #[test]
    fn parse_wifi_interface_names_finds_all_wifi_devices() {
        let output = "Hardware Port: Ethernet\nDevice: en4\n\nHardware Port: Wi-Fi\nDevice: en0\n\nHardware Port: AirPort\nDevice: en7\n";
        assert_eq!(parse_wifi_interface_names(output), vec![
            "en0".to_string(),
            "en7".to_string()
        ]);
    }

    #[test]
    fn parse_interface_names_splits_ifconfig_list() {
        let output = "lo0 gif0 en4 en5 en0 awdl0 utun0\n";
        assert_eq!(parse_interface_names(output), vec![
            "lo0".to_string(),
            "gif0".to_string(),
            "en4".to_string(),
            "en5".to_string(),
            "en0".to_string(),
            "awdl0".to_string(),
            "utun0".to_string(),
        ]);
    }

    #[test]
    fn parse_command_path_uses_first_non_empty_line() {
        assert_eq!(
            parse_command_path("\n/usr/sbin/ipconfig\n"),
            Some("/usr/sbin/ipconfig".to_string())
        );
        assert_eq!(parse_command_path("\n\n"), None);
    }

    #[test]
    fn parse_ipconfig_summary_finds_current_network() {
        let output = "<dictionary> {\n  InterfaceType : WiFi\n  SSID : Ascott Star Rewards\n}\n";
        assert_eq!(
            parse_ipconfig_summary(output).ssid,
            Some("Ascott Star Rewards".to_string())
        );
    }

    #[test]
    fn parse_ipconfig_summary_ignores_empty_ssid() {
        let output = "<dictionary> {\n  InterfaceType : WiFi\n  SSID :   \n}\n";
        assert_eq!(parse_ipconfig_summary(output).ssid, None);
    }

    #[test]
    fn parse_ipconfig_summary_finds_wifi_ssid_and_signal() {
        let output = "<dictionary> {\n  InterfaceType : WiFi\n  SSID : Ascott Star Rewards\n  RSSI : -61\n}\n";
        assert_eq!(parse_ipconfig_summary(output), IpconfigSummary {
            is_wifi: true,
            ssid: Some("Ascott Star Rewards".to_string()),
            signal_strength: Some(-61),
        });
    }

    #[test]
    fn parse_ipconfig_summary_trims_keys_and_ignores_empty_ssid() {
        let output = "<dictionary> {\n    InterfaceType     :   WiFi\n    SSID     :      \n}\n";
        assert_eq!(parse_ipconfig_summary(output), IpconfigSummary {
            is_wifi: true,
            ssid: None,
            signal_strength: None,
        });
    }

    #[test]
    fn select_connected_wifi_interface_skips_disconnected_wifi_interfaces() {
        let summaries = vec![
            WifiInterfaceSummary {
                name: "en0".to_string(),
                summary: IpconfigSummary {
                    is_wifi: true,
                    ssid: None,
                    signal_strength: None,
                },
            },
            WifiInterfaceSummary {
                name: "en5".to_string(),
                summary: IpconfigSummary {
                    is_wifi: true,
                    ssid: Some("OfficeNet".to_string()),
                    signal_strength: Some(-48),
                },
            },
        ];

        let selected = select_connected_wifi_interface(&summaries).expect("connected wifi");
        assert_eq!(selected.name, "en5");
    }

    #[test]
    fn parse_wifi_power_handles_on_off_and_unknown() {
        assert_eq!(parse_wifi_power("Wi-Fi Power (en0): On\n"), WifiPower::On);
        assert_eq!(parse_wifi_power("Wi-Fi Power (en0): Off\n"), WifiPower::Off);
        assert_eq!(parse_wifi_power("unexpected output\n"), WifiPower::Unknown);
    }

    #[test]
    fn test_wifi_info_off() {
        let info = WifiInfo {
            status: WifiStatus::Off,
            network_name: None,
            signal_strength: None,
        };
        assert_eq!(info.status, WifiStatus::Off);
        assert!(info.network_name.is_none());
        assert!(info.signal_strength.is_none());
    }

    #[test]
    fn test_wifi_info_connected() {
        let info = WifiInfo {
            status: WifiStatus::Connected,
            network_name: Some("HomeWiFi".to_string()),
            signal_strength: Some(-50),
        };
        assert_eq!(info.status, WifiStatus::Connected);
        assert_eq!(info.network_name, Some("HomeWiFi".to_string()));
        assert_eq!(info.signal_strength, Some(-50));
    }

    #[test]
    fn test_wifi_info_disconnected() {
        let info = WifiInfo {
            status: WifiStatus::Disconnected,
            network_name: None,
            signal_strength: None,
        };
        assert_eq!(info.status, WifiStatus::Disconnected);
        assert!(info.network_name.is_none());
        assert!(info.signal_strength.is_none());
    }

    #[test]
    fn test_wifi_info_unknown() {
        let info = WifiInfo {
            status: WifiStatus::Unknown,
            network_name: None,
            signal_strength: None,
        };
        assert_eq!(info.status, WifiStatus::Unknown);
    }

    #[test]
    fn test_wifi_info_serialization() {
        let info = WifiInfo {
            status: WifiStatus::Connected,
            network_name: Some("My Network".to_string()),
            signal_strength: Some(-60),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(json["status"], "Connected");
        assert_eq!(json["networkName"], "My Network");
        assert_eq!(json["signalStrength"], -60);
    }

    #[test]
    fn wifi_status_serializes_with_frontend_variant_names() {
        assert_eq!(
            serde_json::to_string(&WifiStatus::Unknown).unwrap(),
            r#""Unknown""#
        );
        assert_eq!(serde_json::to_string(&WifiStatus::Off).unwrap(), r#""Off""#);
        assert_eq!(
            serde_json::to_string(&WifiStatus::Disconnected).unwrap(),
            r#""Disconnected""#
        );
        assert_eq!(
            serde_json::to_string(&WifiStatus::Connected).unwrap(),
            r#""Connected""#
        );
    }
}
