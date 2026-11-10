import { describe, expect, it } from 'vitest';

import type { IdleProcess, IdleSample } from '../measure/types';

import { assessBusyness, describeBusy } from './busy';
import { renderBarReport } from './report';
import { summarizeTrials } from './stats';
import type { BarRun, TrialRecord } from './types';

function trial(index: number, arm: 'off' | 'on', valid: boolean, avg: number): TrialRecord {
  return {
    index,
    pair: Math.floor(index / 2),
    arm,
    valid,
    invalidReason: valid ? null : 'access-blocked',
    message: valid ? 'ok' : 'grant Accessibility to this exact .app',
    readyMs: 100,
    accessState: valid ? 'open' : 'blocked',
    windowsAfterWarmup: null,
    windowsAfterSample: null,
    metrics: valid
      ? {
          hostCpuAvgPercent: avg,
          hostCpuPeakPercent: avg + 1,
          treeCpuPercent: avg,
          idleWakeupsPerSecond: 2,
        }
      : null,
    hostCpuSeries: [],
    idle: null,
    topOutput: null,
    stderrTail: [],
  };
}

function makeRun(trials: TrialRecord[]): BarRun {
  return {
    startedAt: '2026-11-10T10:00:00.000Z',
    finishedAt: '2026-11-10T11:00:00.000Z',
    status: 'completed',
    failure: null,
    commandLine: ['bun', 'scripts/measure-bar.ts'],
    options: {
      appPath: 'a.app',
      launchesPerArm: 1,
      warmupSeconds: 10,
      sampleSeconds: 60,
      height: 28,
      padding: 12,
      ssid: 'default',
      maxBusyCpuPercent: 30,
      preflightSeconds: 10,
    },
    appPath: '/r/Stache Next.app',
    executablePath: '/r/Stache Next.app/Contents/MacOS/stache',
    series: 'default (SSID path on)',
    environment: null,
    preflight: {
      durationSeconds: 10,
      totalCpuPercent: 4,
      maxAllowedCpuPercent: 30,
      busy: false,
      topProcesses: [],
    },
    armOrder: ['off', 'on'],
    trials,
    summary: summarizeTrials(trials, 1),
    safety: {
      guardChecks: ['no Stache process'],
      tempRoots: ['/t/abcd'],
      tempRootsRemoved: true,
      signals: [{ at: 'now', pid: 7, signal: 'SIGTERM', reason: 'stop measured process tree' }],
      notes: [],
    },
    limitations: ['a limitation'],
  };
}

describe('renderBarReport', () => {
  it('shows the Bar-off envelope, the delta with spread and every trial', () => {
    const report = renderBarReport(makeRun([trial(0, 'off', true, 1), trial(1, 'on', true, 3)]));

    expect(report).toContain('# Bar performance measurement - 2026-11-10T10:00:00.000Z');
    expect(report).toContain('Series: default (SSID path on)');
    expect(report).toContain('| Host CPU avg (%) | Bar off | 1.00 | 0.00 | 1.00 | 1.00 | 1 |');
    expect(report).toContain('on - off = 2.00 %');
    expect(report).toContain('standard error');
    expect(report).toContain('ABOVE the largest Bar-off value');
    expect(report).toContain('it does not declare the budget met');
    expect(report).toContain('| 2 | 1 | on | valid |');
    expect(report).toContain('pid 7 SIGTERM');
  });

  it('keeps invalid trials visible and marks insufficient data', () => {
    const report = renderBarReport(makeRun([trial(0, 'off', true, 1), trial(1, 'on', false, 0)]));

    expect(report).toContain('INVALID');
    expect(report).toContain('Insufficient valid launches');
    expect(report).toContain('grant Accessibility to this exact .app');
  });

  it('renders a failed run without a summary', () => {
    const run = makeRun([]);

    run.status = 'failed';
    run.failure = 'busy';
    run.summary = null;
    run.preflight = null;

    const report = renderBarReport(run);

    expect(report).toContain('Status: **failed** - busy');
    expect(report).toContain('No summary');
  });
});

describe('pre-flight idle check', () => {
  const process = (pid: number, cpuPercent: number): IdleProcess => ({
    pid,
    ppid: 1,
    executable: `/bin/p${pid}`,
    role: 'descendant',
    cpuPercent,
    rssFinalKiB: 1,
    rssPeakKiB: 1,
    presentInAllSnapshots: true,
    exitedDuringSampling: false,
  });
  const sample = (total: number, processes: IdleProcess[]): IdleSample =>
    ({ durationSeconds: 10, treeCpuPercent: total, processes }) as IdleSample;

  it('accepts a quiet machine and refuses a busy one naming the busiest processes', () => {
    expect(assessBusyness(sample(12, [process(2, 12)]), 30).busy).toBe(false);

    const busy = assessBusyness(sample(80, [process(2, 5), process(3, 75)]), 30);

    expect(busy.busy).toBe(true);
    expect(busy.topProcesses[0].pid).toBe(3);
    expect(describeBusy(busy)).toContain('Refusing to start: the machine is busy');
    expect(describeBusy(busy)).toContain('pid 3 /bin/p3');
  });
});
