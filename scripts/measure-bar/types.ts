/**
 * Data captured by the Bar performance measurement (Bar off versus Bar on).
 *
 * Everything here is already validated by the module that read it; the shapes
 * are written to samples.json and rendered into report.md.
 */
import type { EnvironmentSnapshot, IdleSample, SignalRecord } from '../measure/types';

export type Arm = 'off' | 'on';

/** Whether the SSID `ipconfig` path ran (default) or was switched off for the series. */
export type SsidSeries = 'default' | 'off';

export interface BarOptions {
  appPath: string;
  /** Valid launches wanted per arm (at least 5). */
  launchesPerArm: number;
  warmupSeconds: number;
  sampleSeconds: number;
  height: number;
  padding: number;
  ssid: SsidSeries;
  /** Whole-machine CPU (sum of per-process percent) above which the machine counts as busy. */
  maxBusyCpuPercent: number;
  preflightSeconds: number;
}

export type AccessState = 'open' | 'blocked' | 'checking';

/** One window owned by the measured host, from `list-windows.swift`. */
export interface HostWindow {
  layer: number;
  x: number;
  y: number;
  width: number;
  height: number;
  onScreen: boolean;
}

export type InvalidReason =
  | 'access-blocked'
  | 'access-unreported'
  | 'bar-panel-missing'
  | 'bar-panel-lost'
  | 'unexpected-window'
  | 'not-ready'
  | 'host-exited'
  | 'measurement-error';

export type TrialVerdict =
  | { valid: true }
  | { valid: false; reason: InvalidReason; message: string };

export interface TrialMetrics {
  /** Host process CPU averaged over the sample window, percent of one core. */
  hostCpuAvgPercent: number;
  /** Highest host CPU over one snapshot interval, percent of one core. */
  hostCpuPeakPercent: number;
  /** Host plus descendants, percent of one core. */
  treeCpuPercent: number;
  /** Idle wakeups per second summed over the host tree; null when `top` gave none. */
  idleWakeupsPerSecond: number | null;
}

export interface TrialRecord {
  /** Position in the schedule, starting at 0. */
  index: number;
  /** Pair number: each pair holds one launch of each arm. */
  pair: number;
  arm: Arm;
  valid: boolean;
  invalidReason: InvalidReason | null;
  /** Human explanation, also for a valid trial ("ok"). */
  message: string;
  readyMs: number | null;
  accessState: AccessState | 'unreported' | null;
  windowsAfterWarmup: HostWindow[] | null;
  windowsAfterSample: HostWindow[] | null;
  /** Null for an invalid trial: it is never a sample. */
  metrics: TrialMetrics | null;
  /** Per-interval host CPU percentages (the raw series behind the peak). */
  hostCpuSeries: number[];
  /** Raw samples kept even for an invalid trial when they exist. */
  idle: IdleSample | null;
  /** Raw `top` output used for the wakeups, kept verbatim. */
  topOutput: string | null;
  stderrTail: string[];
}

export interface PreflightRecord {
  durationSeconds: number;
  totalCpuPercent: number;
  maxAllowedCpuPercent: number;
  busy: boolean;
  topProcesses: { pid: number; executable: string; cpuPercent: number }[];
}

export interface ArmStats {
  n: number;
  mean: number;
  stdev: number;
  min: number;
  max: number;
}

export interface ArmComparison {
  off: ArmStats | null;
  on: ArmStats | null;
  /** on minus off, of the means. */
  delta: number | null;
  /** Welch standard error of the delta. */
  standardError: number | null;
  /** True when the Bar-on mean lies above the largest Bar-off value. */
  onAboveOffEnvelope: boolean | null;
}

export interface BarSummary {
  validPerArm: { off: number; on: number };
  invalidTrials: number;
  hostCpuAvg: ArmComparison;
  hostCpuPeak: ArmComparison;
  treeCpu: ArmComparison;
  idleWakeups: ArmComparison;
  /** False when either arm lacks the requested number of valid launches. */
  sufficient: boolean;
}

export interface BarRun {
  startedAt: string;
  finishedAt: string;
  status: 'completed' | 'failed' | 'interrupted';
  failure: string | null;
  commandLine: string[];
  options: BarOptions;
  appPath: string;
  executablePath: string;
  series: string;
  environment: EnvironmentSnapshot | null;
  preflight: PreflightRecord | null;
  armOrder: Arm[];
  trials: TrialRecord[];
  summary: BarSummary | null;
  safety: {
    guardChecks: string[];
    tempRoots: string[];
    tempRootsRemoved: boolean;
    signals: SignalRecord[];
    notes: string[];
  };
  limitations: string[];
}
