import {
  Wifi01Icon,
  WifiDisconnected04Icon,
  WifiFullSignalIcon,
  WifiLowSignalIcon,
  WifiMediumSignalIcon,
  WifiOff02Icon,
} from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';

import type { BarSnapshotOutcome, WifiReading } from '@/ipc';

import type { IconTone } from '../../styles/widgetIcon.style';

/** The Wi-Fi reading while the Bar is available, otherwise `null`. */
export function selectWifi(outcome: BarSnapshotOutcome): WifiReading | null {
  return outcome.kind === 'available' ? (outcome.view.wifi ?? null) : null;
}

/** What the Wi-Fi item draws; `null` while the status is unknown (hidden). */
export type WifiLook = { icon: IconSvgElement; tone: IconTone; label: string | null };

/** Signal icon by RSSI: full at -60 dBm or better, medium to -70, low to -80, weakest below or unknown. */
export function signalIcon(rssi: number | null | undefined): IconSvgElement {
  if (rssi == null) {
    return Wifi01Icon;
  }

  if (rssi >= -60) {
    return WifiFullSignalIcon;
  }

  if (rssi >= -70) {
    return WifiMediumSignalIcon;
  }

  if (rssi >= -80) {
    return WifiLowSignalIcon;
  }

  return Wifi01Icon;
}

/** The icon, tone and label of a reading; `null` for an absent or `Unknown` reading. */
export function wifiLook(reading: WifiReading | null): WifiLook | null {
  switch (reading?.status) {
    case 'Connected':
      return {
        icon: signalIcon(reading.signalStrength),
        tone: 'highlight',
        label: reading.networkName || null,
      };
    case 'Disconnected':
      return { icon: WifiDisconnected04Icon, tone: 'warning', label: 'Disconnected' };
    case 'Off':
      return { icon: WifiOff02Icon, tone: 'danger', label: 'Wi-Fi Off' };
    default:
      return null;
  }
}
