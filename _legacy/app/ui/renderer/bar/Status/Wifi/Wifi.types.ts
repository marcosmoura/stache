import type { AnyIcon } from '@/components/Icon';

export type WifiStatus = 'Unknown' | 'Off' | 'Disconnected' | 'Connected';

export type WifiInfo = {
  status: WifiStatus;
  networkName: string | null;
  signalStrength: number | null;
};

export type WifiState = {
  status: WifiStatus;
  label: string | null;
  icon: AnyIcon;
  color: string;
  onClick: () => Promise<unknown>;
};
