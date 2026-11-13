import type { BarSnapshotOutcome, CpuReading } from '@/ipc';

/** The CPU reading while the Bar is available, otherwise `null`. */
export function selectCpu(outcome: BarSnapshotOutcome): CpuReading | null {
  return outcome.kind === 'available' ? (outcome.view.cpu ?? null) : null;
}

/** The temperature to show: present and non-zero, otherwise `null`. */
export function cpuTemperature(reading: CpuReading): number | null {
  return reading.temperature ? reading.temperature : null;
}

/** Whether the temperature is at or above the `temperature_alert` option. */
export function isCpuAlert(reading: CpuReading, alert: number): boolean {
  const temperature = cpuTemperature(reading);

  return temperature !== null && temperature >= alert;
}
