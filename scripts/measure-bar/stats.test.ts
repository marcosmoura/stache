import { describe, expect, it } from 'vitest';

import { buildMetrics, compareArms, hostCpuSeries, summarize, summarizeTrials } from './stats';
import type { Arm, TrialRecord } from './types';
import { parseTopWakeups, treeWakeupsPerSecond } from './wakeups';

describe('summarize and compareArms', () => {
  it('computes mean, sample stdev and range', () => {
    const stats = summarize([2, 4, 4, 4, 5, 5, 7, 9]);

    expect(stats?.mean).toBeCloseTo(5);
    expect(stats?.stdev).toBeCloseTo(2.138, 3);
    expect([stats?.min, stats?.max, stats?.n]).toEqual([2, 9, 8]);
    expect(summarize([])).toBeNull();
    expect(summarize([3])?.stdev).toBe(0);
  });

  it('reports the on-minus-off delta with a Welch standard error', () => {
    const comparison = compareArms([1, 1.2, 0.8, 1, 1], [2, 2.2, 1.8, 2, 2]);

    expect(comparison.delta).toBeCloseTo(1);
    expect(comparison.standardError).toBeCloseTo(Math.sqrt((0.02 / 5) * 2), 5);
    expect(comparison.onAboveOffEnvelope).toBe(true);
  });

  it('keeps a delta inside the envelope when the arms overlap', () => {
    expect(compareArms([1, 3], [2, 2]).onAboveOffEnvelope).toBe(false);
  });

  it('gives no delta when an arm is empty', () => {
    expect(compareArms([1], [])).toMatchObject({ delta: null, standardError: null });
  });
});

describe('hostCpuSeries', () => {
  it('derives per-interval host CPU from cumulative CPU time', () => {
    const row = (cpuTimeSeconds: number) => ({
      pid: 9,
      ppid: 1,
      rssKiB: 1,
      cpuTimeSeconds,
      executable: '/x',
    });
    const series = hostCpuSeries(
      [
        { takenAt: '2026-01-01T00:00:00.000Z', processes: [row(1)] },
        { takenAt: '2026-01-01T00:00:05.000Z', processes: [row(1.5)] },
        { takenAt: '2026-01-01T00:00:10.000Z', processes: [row(1.55)] },
      ],
      9,
    );

    expect(series[0]).toBeCloseTo(10);
    expect(series[1]).toBeCloseTo(1);
    expect(
      buildMetrics(
        {
          pid: 9,
          ppid: 1,
          executable: '/x',
          role: 'host',
          cpuPercent: 5.5,
          rssFinalKiB: 1,
          rssPeakKiB: 1,
          presentInAllSnapshots: true,
          exitedDuringSampling: false,
        },
        7,
        series,
        3,
      ),
    ).toEqual({
      hostCpuAvgPercent: 5.5,
      hostCpuPeakPercent: series[0],
      treeCpuPercent: 7,
      idleWakeupsPerSecond: 3,
    });
  });
});

function trial(arm: Arm, valid: boolean, avg: number, wakeups: number | null = 1): TrialRecord {
  return {
    index: 0,
    pair: 0,
    arm,
    valid,
    invalidReason: valid ? null : 'access-blocked',
    message: '',
    readyMs: 1,
    accessState: 'open',
    windowsAfterWarmup: null,
    windowsAfterSample: null,
    metrics: valid
      ? {
          hostCpuAvgPercent: avg,
          hostCpuPeakPercent: avg * 2,
          treeCpuPercent: avg,
          idleWakeupsPerSecond: wakeups,
        }
      : null,
    hostCpuSeries: [],
    idle: null,
    topOutput: null,
    stderrTail: [],
  };
}

describe('summarizeTrials', () => {
  it('never counts an invalid trial as a sample', () => {
    const summary = summarizeTrials(
      [trial('off', true, 1), trial('on', false, 99), trial('on', true, 2)],
      1,
    );

    expect(summary.validPerArm).toEqual({ off: 1, on: 1 });
    expect(summary.invalidTrials).toBe(1);
    expect(summary.hostCpuAvg.on?.max).toBe(2);
    expect(summary.sufficient).toBe(true);
  });

  it('is insufficient when an arm lacks the requested valid launches', () => {
    expect(summarizeTrials([trial('off', true, 1), trial('on', false, 0)], 1).sufficient).toBe(
      false,
    );
  });

  it('skips missing wakeups instead of treating them as zero', () => {
    const summary = summarizeTrials([trial('off', true, 1, null), trial('on', true, 1, 4)], 1);

    expect(summary.idleWakeups.off).toBeNull();
    expect(summary.idleWakeups.on?.mean).toBe(4);
  });
});

describe('top wakeups', () => {
  const output = [
    'Processes: 1',
    'PID    IDLEW',
    '1      9999',
    '5      100',
    '',
    'Processes: 1',
    'PID    IDLEW',
    '1      12',
    '5      30+',
    '6      0',
  ].join('\n');

  it('reads the last sample block and strips the rounding mark', () => {
    const wakeups = parseTopWakeups(output);

    expect([...wakeups.entries()]).toEqual([
      [1, 12],
      [5, 30],
      [6, 0],
    ]);
    expect(parseTopWakeups('nothing')).toEqual(new Map());
  });

  it('sums the tree pids per second, deduplicated, and reports null when none was seen', () => {
    const wakeups = parseTopWakeups(output);

    expect(treeWakeupsPerSecond(wakeups, [5, 6, 5], 10)).toBe(3);
    expect(treeWakeupsPerSecond(wakeups, [42], 10)).toBeNull();
    expect(treeWakeupsPerSecond(wakeups, [5], 0)).toBeNull();
  });
});
