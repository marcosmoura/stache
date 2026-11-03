import { useCallback } from 'react';

import {
  Wifi01Icon,
  WifiDisconnected04Icon,
  WifiFullSignalIcon,
  WifiLowSignalIcon,
  WifiMediumSignalIcon,
  WifiOff02Icon,
} from '@hugeicons/core-free-icons';
import { useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import type { AnyIcon } from '@/components/Icon';
import { colors } from '@/design-system';

import type { WifiInfo, WifiState, WifiStatus } from './Wifi.types';

const STATUS_LABELS: Record<WifiStatus, string | null> = {
  Unknown: null,
  Off: 'Wi-Fi Off',
  Disconnected: 'Disconnected',
  Connected: null,
};

function fetchWifi(): Promise<WifiInfo> {
  return invoke<WifiInfo>('get_wifi_info');
}

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
  const { data: wifi } = useSuspenseQuery({
    queryKey: ['wifi'],
    queryFn: fetchWifi,
    refetchInterval: 5000,
    refetchOnMount: true,
  });

  const status: WifiStatus = wifi?.status ?? 'Unknown';
  const networkName = wifi?.networkName ?? null;
  const signalStrength = wifi?.signalStrength ?? null;

  const onClick = useCallback(() => invoke('open_app', { name: 'Wi-Fi' }), []);

  const label = getWifiLabel(status, networkName);
  const icon = getWifiIcon(status, signalStrength);
  const color = getWifiColor(status);

  return {
    label,
    icon,
    color,
    status,
    onClick,
  };
}
