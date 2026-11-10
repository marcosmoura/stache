import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { runCommand } from '../measure/process';

import type { ArmComparison, ArmStats, BarRun, TrialRecord } from './types';

export const BAR_MEASUREMENTS_DIRECTORY = 'docs/specs/2026-11-03-bar/measurements';

export interface BarOutputPaths {
  directory: string;
  reportPath: string;
  samplesPath: string;
}

function fixed(value: number | null, digits = 2): string {
  return value === null ? 'n/a' : value.toFixed(digits);
}

function statsCells(stats: ArmStats | null): string {
  return stats === null
    ? 'n/a | n/a | n/a | n/a | 0'
    : `${fixed(stats.mean)} | ${fixed(stats.stdev)} | ${fixed(stats.min)} | ${fixed(stats.max)} | ${stats.n}`;
}

function comparisonRows(label: string, unit: string, comparison: ArmComparison): string[] {
  return [
    `| ${label} (${unit}) | Bar off | ${statsCells(comparison.off)} |`,
    `| ${label} (${unit}) | Bar on | ${statsCells(comparison.on)} |`,
  ];
}

function deltaLine(label: string, unit: string, comparison: ArmComparison): string {
  if (comparison.delta === null || comparison.standardError === null) {
    return `- ${label}: no delta (an arm has no valid samples).`;
  }

  const above =
    comparison.onAboveOffEnvelope === true
      ? 'the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope)'
      : 'the Bar-on mean is within the Bar-off range (inside the recorded noise envelope)';

  return (
    `- ${label}: on - off = ${fixed(comparison.delta)} ${unit} ` +
    `(spread: standard error ${fixed(comparison.standardError)} ${unit}); ${above}.`
  );
}

function trialRow(trial: TrialRecord): string {
  const metrics = trial.metrics;
  const cells =
    metrics === null
      ? ['-', '-', '-', '-']
      : [
          fixed(metrics.hostCpuAvgPercent),
          fixed(metrics.hostCpuPeakPercent),
          fixed(metrics.treeCpuPercent),
          fixed(metrics.idleWakeupsPerSecond),
        ];

  return (
    `| ${trial.index + 1} | ${trial.pair + 1} | ${trial.arm} | ` +
    `${trial.valid ? 'valid' : 'INVALID'} | ${cells.join(' | ')} | ${trial.message.replaceAll('|', '/')} |`
  );
}

/** Renders report.md from one run; never calls a budget met or missed. */
export function renderBarReport(run: BarRun): string {
  const lines: string[] = [];
  const summary = run.summary;

  lines.push(`# Bar performance measurement - ${run.startedAt}`);
  lines.push('');
  lines.push('Read when judging the Bar-on cost against the Bar-off noise envelope.');
  lines.push('');
  lines.push(`- Status: **${run.status}**${run.failure === null ? '' : ` - ${run.failure}`}`);
  lines.push(`- Series: ${run.series}`);
  lines.push(`- App: \`${run.appPath}\` (\`${run.executablePath}\`)`);
  lines.push(
    `- Plan: ${run.options.launchesPerArm} valid launches per arm, warmup ` +
      `${run.options.warmupSeconds} s, sample ${run.options.sampleSeconds} s, Bar height ` +
      `${run.options.height} / padding ${run.options.padding}`,
  );
  lines.push(`- Arm order: ${run.armOrder.join(' ')}`);
  lines.push(`- Command: \`${run.commandLine.join(' ')}\``);

  const environment = run.environment;

  if (environment !== null) {
    lines.push(
      `- Machine: ${environment.machine.model}, ${environment.machine.chip}; macOS ` +
        `${environment.os.productVersion} (${environment.os.buildVersion}); power ` +
        `${environment.power.source}${environment.power.batteryPercent === null ? '' : ` ${environment.power.batteryPercent}%`}`,
    );
    lines.push(
      `- Repository: ${environment.repository.commit} on ${environment.repository.branch}` +
        `${environment.repository.dirty ? ' (dirty)' : ''}`,
    );
  }

  lines.push('');
  lines.push('## Pre-flight');
  lines.push('');

  if (run.preflight === null) {
    lines.push('Not completed.');
  } else {
    lines.push(
      `Whole-machine CPU ${fixed(run.preflight.totalCpuPercent, 1)}% over ` +
        `${run.preflight.durationSeconds} s (limit ${run.preflight.maxAllowedCpuPercent}%): ` +
        `${run.preflight.busy ? 'BUSY, run refused' : 'idle enough'}.`,
    );
  }

  lines.push('');
  lines.push('## Result');
  lines.push('');

  if (summary === null) {
    lines.push('No summary: the run stopped before any trial finished.');
  } else {
    lines.push(
      `Valid launches: off ${summary.validPerArm.off}, on ${summary.validPerArm.on} ` +
        `(requested ${run.options.launchesPerArm} per arm); invalid trials: ${summary.invalidTrials}.`,
    );
    lines.push('');

    if (!summary.sufficient) {
      lines.push(
        '**Insufficient valid launches.** The statistics below are incomplete and must not be ' +
          'used to claim the budget; the budget stays unverified.',
      );
      lines.push('');
    }

    lines.push('| Metric | Arm | Mean | Stdev | Min | Max | N |');
    lines.push('| --- | --- | --- | --- | --- | --- | --- |');
    lines.push(...comparisonRows('Host CPU avg', '%', summary.hostCpuAvg));
    lines.push(...comparisonRows('Host CPU peak', '%', summary.hostCpuPeak));
    lines.push(...comparisonRows('Tree CPU avg', '%', summary.treeCpu));
    lines.push(...comparisonRows('Idle wakeups', '/s', summary.idleWakeups));
    lines.push('');
    lines.push('Bar-off noise envelope = the Bar-off min..max and stdev above.');
    lines.push('');
    lines.push(deltaLine('Host CPU avg', '%', summary.hostCpuAvg));
    lines.push(deltaLine('Host CPU peak', '%', summary.hostCpuPeak));
    lines.push(deltaLine('Tree CPU avg', '%', summary.treeCpu));
    lines.push(deltaLine('Idle wakeups', '/s', summary.idleWakeups));
    lines.push('');
    lines.push(
      'The budget is "no statistically distinguishable regression beyond the recorded noise ' +
        'envelope". This report records the evidence; it does not declare the budget met.',
    );
  }

  lines.push('');
  lines.push('## Trials');
  lines.push('');
  lines.push(
    '| # | Pair | Arm | Verdict | Host CPU avg % | Host CPU peak % | Tree CPU % | Wakeups/s | Note |',
  );
  lines.push('| --- | --- | --- | --- | --- | --- | --- | --- | --- |');
  lines.push(...run.trials.map(trialRow));
  lines.push('');
  lines.push('## Safety');
  lines.push('');
  lines.push(
    `- Temporary roots: ${run.safety.tempRoots.length}; removed: ${run.safety.tempRootsRemoved}`,
  );
  lines.push(`- Signals sent (owned children only): ${run.safety.signals.length}`);

  for (const signal of run.safety.signals) {
    lines.push(`  - ${signal.at} pid ${signal.pid} ${signal.signal}: ${signal.reason}`);
  }

  for (const check of run.safety.guardChecks) {
    lines.push(`- Guard: ${check}`);
  }

  for (const note of run.safety.notes) {
    lines.push(`- Note: ${note}`);
  }

  lines.push('');
  lines.push('## Limitations');
  lines.push('');
  lines.push(...run.limitations.map((limitation) => `- ${limitation}`));
  lines.push('');

  return lines.join('\n');
}

/** Writes report.md and samples.json to a timestamped directory, even for a failed run. */
export function writeBarOutputs(
  run: BarRun,
  report: string,
  repositoryRoot: string,
): BarOutputPaths {
  const timestamp = run.startedAt.replace(/[:.]/g, '-');
  let directory = path.join(repositoryRoot, BAR_MEASUREMENTS_DIRECTORY, timestamp);
  let suffix = 2;

  while (existsSync(directory)) {
    directory = path.join(repositoryRoot, BAR_MEASUREMENTS_DIRECTORY, `${timestamp}-${suffix}`);
    suffix += 1;
  }

  mkdirSync(directory, { recursive: true });
  const reportPath = path.join(directory, 'report.md');
  const samplesPath = path.join(directory, 'samples.json');

  writeFileSync(reportPath, report, 'utf8');
  writeFileSync(samplesPath, `${JSON.stringify(run, null, 2)}\n`, 'utf8');

  try {
    runCommand(path.join(repositoryRoot, 'node_modules/.bin/oxfmt'), [
      '--disable-nested-config',
      directory,
    ]);
  } catch (error) {
    // The raw files are already on disk; formatting is cosmetic.
    run.safety.notes.push(
      `could not format the output: ${error instanceof Error ? error.message : String(error)}`,
    );
  }

  return { directory, reportPath, samplesPath };
}
