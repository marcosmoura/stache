import type { IdleSample } from '../measure/types';

import type { PreflightRecord } from './types';

const TOP_PROCESSES = 5;

/**
 * Judges a whole-machine idle sample (the tree of pid 1, sampled with the
 * foundation tool's `sampleIdle`) against a CPU ceiling.
 */
export function assessBusyness(sample: IdleSample, maxAllowedCpuPercent: number): PreflightRecord {
  const topProcesses = [...sample.processes]
    .sort((left, right) => right.cpuPercent - left.cpuPercent)
    .slice(0, TOP_PROCESSES)
    .map((process) => ({
      pid: process.pid,
      executable: process.executable,
      cpuPercent: process.cpuPercent,
    }));

  return {
    durationSeconds: sample.durationSeconds,
    totalCpuPercent: sample.treeCpuPercent,
    maxAllowedCpuPercent,
    busy: sample.treeCpuPercent > maxAllowedCpuPercent,
    topProcesses,
  };
}

export function describeBusy(record: PreflightRecord): string {
  const top = record.topProcesses
    .map(
      (process) => `  ${process.cpuPercent.toFixed(1)}% pid ${process.pid} ${process.executable}`,
    )
    .join('\n');

  return [
    `Refusing to start: the machine is busy (${record.totalCpuPercent.toFixed(1)}% total CPU ` +
      `over ${record.durationSeconds} s, limit ${record.maxAllowedCpuPercent}%).`,
    'Busiest processes:',
    top,
    'Quit background work and rerun, or raise --max-busy-cpu knowingly.',
  ].join('\n');
}
