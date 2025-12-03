export type BatteryState = 'Unknown' | 'Charging' | 'Discharging' | 'Empty' | 'Full';

export type BatteryInfo = {
  percentage: number;
  state: BatteryState;
};

export type BatteryData = {
  state: BatteryState;
  label: string;
  percentage: number;
};
