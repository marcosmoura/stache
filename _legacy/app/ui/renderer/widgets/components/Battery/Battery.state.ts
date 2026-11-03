import { useMemo } from 'react';

import { colors } from '@/design-system';
import { useBatteryStore } from '@/stores/BatteryStore';
import type { BatteryState } from '@/stores/BatteryStore';

function formatTime(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);

  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }

  return `${minutes}m`;
}

function formatTemperature(celsius: number | null): string {
  if (celsius === null) {
    return 'N/A';
  }

  return `${celsius.toFixed(1)}°C`;
}

function formatVoltage(volts: number): string {
  return `${volts.toFixed(2)}V`;
}

function formatCycles(cycles: number | null): string {
  if (cycles === null) {
    return 'N/A';
  }

  return cycles.toLocaleString();
}

function getStateLabel(state: BatteryState): string {
  switch (state) {
    case 'Charging':
      return 'Charging';
    case 'Discharging':
      return 'On Battery';
    case 'Full':
      return 'Fully Charged';
    case 'Empty':
      return 'Empty';
    default:
      return 'Unknown';
  }
}

function getProgressColor(percentage: number, state: BatteryState): string {
  if (state === 'Charging') {
    return colors.green;
  }

  if (percentage <= 10) {
    return colors.red;
  }

  if (percentage <= 20) {
    return colors.peach;
  }

  if (percentage <= 40) {
    return colors.yellow;
  }

  return colors.green;
}

function getHealthColor(health: number): string {
  if (health >= 80) {
    return colors.green;
  }

  if (health >= 60) {
    return colors.yellow;
  }

  if (health >= 40) {
    return colors.peach;
  }

  return colors.red;
}

export function useBatteryWidget() {
  // Get state from hook-based store (uses React Query internally)
  const { battery } = useBatteryStore();

  const formattedData = useMemo(() => {
    if (!battery) {
      return null;
    }

    const {
      percentage,
      state,
      health,
      temperature,
      voltage,
      cycle_count,
      time_to_full,
      time_to_empty,
    } = battery;

    let timeRemaining: string | null = null;
    if (state === 'Charging' && time_to_full) {
      timeRemaining = `${formatTime(time_to_full)} until full`;
    } else if (state === 'Discharging' && time_to_empty) {
      timeRemaining = `${formatTime(time_to_empty)} remaining`;
    }

    return {
      percentage,
      state,
      stateLabel: getStateLabel(state),
      health,
      healthFormatted: `${health}%`,
      healthColor: getHealthColor(health),
      temperature: formatTemperature(temperature),
      voltage: formatVoltage(voltage),
      cycles: formatCycles(cycle_count),
      timeRemaining,
      progressColor: getProgressColor(percentage, state),
    };
  }, [battery]);

  return formattedData;
}
