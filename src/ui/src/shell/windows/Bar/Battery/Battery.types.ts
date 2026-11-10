import type { AnyIcon } from '../../../components/Icon';

export type BatteryChargeState = 'Unknown' | 'Charging' | 'Discharging' | 'Empty' | 'Full';

/** The fields of legacy `BatteryInfo` the Bar renders. */
export type BatteryReading = {
  /** Battery charge percentage (0-100) */
  percentage: number;
  /** Current battery state (charging, discharging, etc.) */
  state: BatteryChargeState;
};

export type BatteryState = {
  icon: AnyIcon;
  label: string;
  color: string;
  percentage: number | undefined;
};
