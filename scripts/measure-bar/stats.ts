import type { IdleProcess, ProcessTreeSnapshot } from '../measure/types';

import type { Arm, ArmComparison, ArmStats, BarSummary, TrialMetrics, TrialRecord } from './types';

export function summarize(values: number[]): ArmStats | null {
  if (values.length === 0) {
    return null;
  }

  const mean = values.reduce((total, value) => total + value, 0) / values.length;
  const variance =
    values.length < 2
      ? 0
      : values.reduce((total, value) => total + (value - mean) ** 2, 0) / (values.length - 1);

  return {
    n: values.length,
    mean,
    stdev: Math.sqrt(variance),
    min: Math.min(...values),
    max: Math.max(...values),
  };
}

/** Bar-off envelope, Bar-on stats and the on-minus-off delta with its spread. */
export function compareArms(off: number[], on: number[]): ArmComparison {
  const offStats = summarize(off);
  const onStats = summarize(on);

  if (offStats === null || onStats === null) {
    return {
      off: offStats,
      on: onStats,
      delta: null,
      standardError: null,
      onAboveOffEnvelope: null,
    };
  }

  return {
    off: offStats,
    on: onStats,
    delta: onStats.mean - offStats.mean,
    standardError: Math.sqrt(offStats.stdev ** 2 / offStats.n + onStats.stdev ** 2 / onStats.n),
    onAboveOffEnvelope: onStats.mean > offStats.max,
  };
}

/**
 * Per-interval CPU percent of the host between consecutive tree snapshots.
 *
 * Intervals where the host row is missing in either snapshot, or whose time did
 * not advance, are skipped.
 */
export function hostCpuSeries(snapshots: ProcessTreeSnapshot[], hostPid: number): number[] {
  const series: number[] = [];

  for (let index = 1; index < snapshots.length; index += 1) {
    const before = snapshots[index - 1].processes.find((row) => row.pid === hostPid);
    const after = snapshots[index].processes.find((row) => row.pid === hostPid);
    const seconds =
      (Date.parse(snapshots[index].takenAt) - Date.parse(snapshots[index - 1].takenAt)) / 1_000;

    if (before === undefined || after === undefined || !(seconds > 0)) {
      continue;
    }

    series.push((Math.max(after.cpuTimeSeconds - before.cpuTimeSeconds, 0) / seconds) * 100);
  }

  return series;
}

export function buildMetrics(
  host: IdleProcess,
  treeCpuPercent: number,
  series: number[],
  idleWakeupsPerSecond: number | null,
): TrialMetrics {
  return {
    hostCpuAvgPercent: host.cpuPercent,
    hostCpuPeakPercent: series.length === 0 ? host.cpuPercent : Math.max(...series),
    treeCpuPercent,
    idleWakeupsPerSecond,
  };
}

function valuesFor(
  trials: TrialRecord[],
  arm: Arm,
  pick: (metrics: TrialMetrics) => number | null,
): number[] {
  return trials
    .filter((trial) => trial.valid && trial.arm === arm && trial.metrics !== null)
    .map((trial) => pick(trial.metrics as TrialMetrics))
    .filter((value): value is number => value !== null);
}

/** Summarizes only valid trials; an invalid trial is never a sample. */
export function summarizeTrials(trials: TrialRecord[], launchesPerArm: number): BarSummary {
  const compare = (pick: (metrics: TrialMetrics) => number | null): ArmComparison =>
    compareArms(valuesFor(trials, 'off', pick), valuesFor(trials, 'on', pick));
  const validCount = (arm: Arm): number =>
    trials.filter((trial) => trial.valid && trial.arm === arm).length;
  const validPerArm = { off: validCount('off'), on: validCount('on') };

  return {
    validPerArm,
    invalidTrials: trials.filter((trial) => !trial.valid).length,
    hostCpuAvg: compare((metrics) => metrics.hostCpuAvgPercent),
    hostCpuPeak: compare((metrics) => metrics.hostCpuPeakPercent),
    treeCpu: compare((metrics) => metrics.treeCpuPercent),
    idleWakeups: compare((metrics) => metrics.idleWakeupsPerSecond),
    sufficient: validPerArm.off >= launchesPerArm && validPerArm.on >= launchesPerArm,
  };
}
