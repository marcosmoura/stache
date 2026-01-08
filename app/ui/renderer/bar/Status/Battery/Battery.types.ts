export type BatteryState = 'Unknown' | 'Charging' | 'Discharging' | 'Empty' | 'Full';

export type BatteryTechnology =
  | 'Unknown'
  | 'LithiumIon'
  | 'LeadAcid'
  | 'LithiumPolymer'
  | 'NickelMetalHydride'
  | 'NickelCadmium'
  | 'NickelZinc'
  | 'LithiumIronPhosphate'
  | 'RechargeableAlkalineManganese';

export type BatteryInfo = {
  /** Battery charge percentage (0-100) */
  percentage: number;
  /** Current battery state (charging, discharging, etc.) */
  state: BatteryState;
  /** Battery health percentage (0-100) */
  health: number;
  /** Battery technology type */
  technology: BatteryTechnology;
  /** Current energy in watt-hours */
  energy: number;
  /** Energy when fully charged in watt-hours */
  energy_full: number;
  /** Design energy capacity in watt-hours */
  energy_full_design: number;
  /** Current power draw/charge rate in watts */
  energy_rate: number;
  /** Current voltage in volts */
  voltage: number;
  /** Battery temperature in celsius (if available) */
  temperature: number | null;
  /** Number of charge cycles (if available) */
  cycle_count: number | null;
  /** Time until fully charged in seconds (if charging) */
  time_to_full: number | null;
  /** Time until empty in seconds (if discharging) */
  time_to_empty: number | null;
  /** Battery vendor (if available) */
  vendor: string | null;
  /** Battery model (if available) */
  model: string | null;
  /** Battery serial number (if available) */
  serial_number: string | null;
};

export type BatteryData = {
  state: BatteryState;
  label: string;
  percentage: number;
};
