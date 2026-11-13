import {
  BatteryChargingIcon,
  BatteryEmptyIcon,
  BatteryFullIcon,
  BatteryLowIcon,
  BatteryMedium02Icon,
  BatteryMediumIcon,
} from '@hugeicons/core-free-icons';
import type { IconSvgElement } from '@hugeicons/react';

import type { BarSnapshotOutcome, BatteryChargeState, BatteryReading } from '@/ipc';

import type { IconTone } from '../../styles/widgetIcon.style';

/**
 * The battery reading while the Bar is available, otherwise `null`. The host
 * publishes no reading both before the first sample and on a machine without
 * a battery; both hide the item.
 */
export function selectBattery(outcome: BarSnapshotOutcome): BatteryReading | null {
  return outcome.kind === 'available' ? (outcome.view.battery ?? null) : null;
}

/** A reading that may lack its percentage or state (shown as `Loading...`). */
export type PartialBattery = {
  percentage?: number | null;
  state?: BatteryChargeState | null;
};

/** `Loading...` without a percentage or state; `100%` when full; `NN%` when unknown or compact; else `NN% (State)`. */
export function batteryLabel({ percentage, state }: PartialBattery, compact: boolean): string {
  if (typeof percentage !== 'number' || !state) {
    return 'Loading...';
  }

  if (state === 'Full') {
    return '100%';
  }

  if (state === 'Unknown' || compact) {
    return `${percentage}%`;
  }

  return `${percentage}% (${state})`;
}

/** Charging icon while charging; otherwise full at 100, steps at 75, 50 and 25, empty below. */
export function batteryIcon({ percentage, state }: PartialBattery): IconSvgElement {
  if (typeof percentage !== 'number') {
    return BatteryEmptyIcon;
  }

  if (state === 'Charging') {
    return BatteryChargingIcon;
  }

  if (percentage >= 100) {
    return BatteryFullIcon;
  }

  if (percentage >= 75) {
    return BatteryMedium02Icon;
  }

  if (percentage >= 50) {
    return BatteryMediumIcon;
  }

  if (percentage >= 25) {
    return BatteryLowIcon;
  }

  return BatteryEmptyIcon;
}

/** Success while charging, warning while discharging, danger when empty, text otherwise. */
export function batteryTone({ state }: PartialBattery): IconTone {
  switch (state) {
    case 'Charging':
      return 'success';
    case 'Discharging':
      return 'warning';
    case 'Empty':
      return 'danger';
    default:
      return 'text';
  }
}
