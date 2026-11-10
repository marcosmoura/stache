import {
  Wifi01Icon,
  WifiDisconnected04Icon,
  WifiFullSignalIcon,
  WifiLowSignalIcon,
  WifiMediumSignalIcon,
  WifiOff02Icon,
} from '@hugeicons/core-free-icons';

import { colors } from '../../../../sdk/tokens';
import type { AnyIcon } from '../../../components/Icon';
import { useBarData } from '../Bar.data';

import type { WifiState, WifiStatus } from './Wifi.types';

const STATUS_LABELS: Record<WifiStatus, string | null> = {
  Unknown: null,
  Off: 'Wi-Fi Off',
  Disconnected: 'Disconnected',
  Connected: null,
};

function getWifiLabel(status: WifiStatus, networkName: string | null): string | null {
  if (status === 'Connected') {
    return networkName;
  }

  return STATUS_LABELS[status];
}

export function getSignalIcon(signalStrength: number | null): AnyIcon {
  if (signalStrength == null) {
    return Wifi01Icon;
  }

  if (signalStrength >= -60) {
    return WifiFullSignalIcon;
  }

  if (signalStrength >= -70) {
    return WifiMediumSignalIcon;
  }

  if (signalStrength >= -80) {
    return WifiLowSignalIcon;
  }

  return Wifi01Icon;
}

function getWifiIcon(status: WifiStatus, signalStrength: number | null): AnyIcon {
  switch (status) {
    case 'Off':
      return WifiOff02Icon;
    case 'Disconnected':
      return WifiDisconnected04Icon;
    default:
      return getSignalIcon(signalStrength);
  }
}

function getWifiColor(status: WifiStatus): string {
  switch (status) {
    case 'Connected':
      return colors.sky;
    case 'Disconnected':
      return colors.yellow;
    case 'Off':
      return colors.red;
    default:
      return colors.text;
  }
}

export function useWifi(): WifiState {
  const { wifi } = useBarData();

  const status: WifiStatus = wifi?.status ?? 'Unknown';
  const networkName = wifi?.networkName ?? null;
  const signalStrength = wifi?.signalStrength ?? null;

  const label = getWifiLabel(status, networkName);
  const icon = getWifiIcon(status, signalStrength);
  const color = getWifiColor(status);

  return {
    label,
    icon,
    color,
    status,
  };
}
