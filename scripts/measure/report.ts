import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import {
  MEASUREMENTS_DIRECTORY,
  NEAR_ZERO_CPU_PERCENT,
  PS_CPU_TIME_RESOLUTION_SECONDS,
  TARGET_TRAY_READY_MS,
} from './paths';
import { runCommand } from './process';
import type { BuildInfo, IdleSample, LaunchRecord, MeasurementRun, ScenarioResult } from './types';

export interface OutputPaths {
  directory: string;
  reportPath: string;
  samplesPath: string;
}

export type TargetStatus = 'met' | 'miss' | 'not measured' | 'unverified';

export interface TargetCheck {
  target: string;
  status: TargetStatus;
  detail: string;
}

interface TimingStats {
  coldMs: number | null;
  warmMedianMs: number | null;
  warmMinMs: number | null;
  warmMaxMs: number | null;
}

/** Compares the measured run with the provisional targets from the spec. */
export function evaluateTargets(run: MeasurementRun): TargetCheck[] {
  const checks: TargetCheck[] = [];
  const nextScenarios = run.scenarios.filter((scenario) => scenario.target.kind === 'next');
  const legacyScenario = run.scenarios.find(
    (scenario) => scenario.target.kind === 'legacy' && scenario.complete,
  );

  checks.push(trayReadyCheck(nextScenarios));
  checks.push(idleCpuCheck(nextScenarios));
  checks.push(webviewCheck(nextScenarios));
  checks.push(treeRssCheck(nextScenarios, legacyScenario));

  return checks;
}

function trayReadyCheck(scenarios: ScenarioResult[]): TargetCheck {
  const parts: string[] = [];
  let failed = false;
  let measured = false;
  let incomplete = false;

  for (const scenario of scenarios) {
    if (!scenario.complete) {
      incomplete = true;
      parts.push(`${scenario.label}: incomplete run`);
      continue;
    }

    const stats = internalTimingStats(scenario.launches);

    if (stats.warmMedianMs === null) {
      incomplete = true;
      parts.push(`${scenario.label}: no internal tray timing`);
      continue;
    }

    measured = true;
    const passed = stats.warmMedianMs < TARGET_TRAY_READY_MS;

    failed ||= !passed;
    parts.push(
      `${scenario.label}: warm median ${formatMs(stats.warmMedianMs)} ` +
        `(range ${formatRangeMs(stats)}, cold ${formatMs(stats.coldMs)})`,
    );
  }

  return {
    target: `Host main-entry -> tray-created < ${TARGET_TRAY_READY_MS} ms (release, warm median)`,
    status: failed ? 'miss' : incomplete || !measured ? 'unverified' : 'met',
    detail: parts.join('; '),
  };
}

function idleCpuCheck(scenarios: ScenarioResult[]): TargetCheck {
  const parts: string[] = [];
  let failed = false;
  let measured = false;
  let incomplete = false;

  for (const scenario of scenarios) {
    if (!scenario.complete) {
      incomplete = true;
      parts.push(`${scenario.label}: incomplete run`);
      continue;
    }

    if (scenario.idle === null) {
      incomplete = true;
      parts.push(`${scenario.label}: no idle sample`);
      continue;
    }

    measured = true;
    const nearZero = scenario.idle.treeCpuPercent < NEAR_ZERO_CPU_PERCENT;

    failed ||= !nearZero;
    parts.push(
      `${scenario.label}: tree average ${formatPercent(scenario.idle.treeCpuPercent)}, ` +
        `host ${formatPercent(scenario.idle.host.cpuPercent)}`,
    );
  }

  return {
    target: `~0% average idle CPU (noise threshold < ${NEAR_ZERO_CPU_PERCENT}%)`,
    status: failed ? 'miss' : incomplete || !measured ? 'unverified' : 'met',
    detail: parts.join('; '),
  };
}

function webviewCheck(scenarios: ScenarioResult[]): TargetCheck {
  const parts: string[] = [];
  let failed = false;
  let measured = false;
  let incomplete = false;

  for (const scenario of scenarios) {
    if (!scenario.complete) {
      incomplete = true;
      parts.push(`${scenario.label}: incomplete run`);
      continue;
    }

    const counts = scenario.launches
      .map((launch) => launch.instrumentedWebviews)
      .filter((count): count is number => count !== null);
    const observed = scenario.idle?.processes.filter((process) =>
      /WebKit\.(WebContent|Networking|GPU)/.test(process.executable),
    ).length;

    if (counts.length === 0) {
      incomplete = true;
      parts.push(`${scenario.label}: no instrumented webview count`);
      continue;
    }

    measured = true;
    const maximum = Math.max(...counts);

    failed ||= maximum !== 0 || (observed ?? 0) > 0;
    parts.push(
      `${scenario.label}: instrumented ${maximum} webview(s)` +
        (observed === undefined ? '' : `, ${observed} WebKit descendant(s) observed`),
    );
  }

  return {
    target: 'Zero webviews with pilot enabled and disabled',
    status: failed ? 'miss' : incomplete || !measured ? 'unverified' : 'met',
    detail: parts.join('; '),
  };
}

function treeRssCheck(
  nextScenarios: ScenarioResult[],
  legacyScenario: ScenarioResult | undefined,
): TargetCheck {
  if (nextScenarios.length === 0) {
    return {
      target: 'Lower idle tree RSS than legacy',
      status: 'unverified',
      detail: 'no Stache Next scenario produced an idle sample',
    };
  }

  if (legacyScenario?.idle === undefined || legacyScenario?.idle === null) {
    return {
      target: 'Lower idle tree RSS than legacy',
      status: 'not measured',
      detail: 'legacy baseline was not requested or produced no idle sample',
    };
  }

  const legacyRss = legacyScenario.idle.treeRssFinalKiB;
  const parts: string[] = [];
  let failed = false;
  let measured = false;
  let incomplete = false;

  for (const scenario of nextScenarios) {
    if (!scenario.complete) {
      incomplete = true;
      parts.push(`${scenario.label}: incomplete run`);
      continue;
    }

    if (scenario.idle === null) {
      parts.push(`${scenario.label}: no idle sample`);
      continue;
    }

    measured = true;
    const passed = scenario.idle.treeRssFinalKiB < legacyRss;

    failed ||= !passed;
    parts.push(
      `${scenario.label}: ${formatKiB(scenario.idle.treeRssFinalKiB)} vs ` +
        `legacy ${formatKiB(legacyRss)}`,
    );
  }

  return {
    target: 'Lower idle tree RSS than legacy',
    status: !measured ? 'unverified' : failed ? 'miss' : incomplete ? 'unverified' : 'met',
    detail: parts.join('; '),
  };
}

/** Writes report.md and samples.json under the spec's measurements directory. */
export function writeMeasurementOutputs(
  run: MeasurementRun,
  report: string,
  repositoryRoot: string,
): OutputPaths {
  const timestamp = run.startedAt.replace(/[:.]/g, '-');
  let directory = path.join(repositoryRoot, MEASUREMENTS_DIRECTORY, timestamp);
  let suffix = 2;

  while (existsSync(directory)) {
    directory = path.join(repositoryRoot, MEASUREMENTS_DIRECTORY, `${timestamp}-${suffix}`);
    suffix += 1;
  }

  mkdirSync(directory, { recursive: true });
  const reportPath = path.join(directory, 'report.md');
  const samplesPath = path.join(directory, 'samples.json');

  writeFileSync(reportPath, report, 'utf8');
  writeFileSync(samplesPath, `${JSON.stringify(run, null, 2)}\n`, 'utf8');
  // Format with the repository formatter so the committed output passes the hooks.
  runCommand(path.join(repositoryRoot, 'node_modules/.bin/oxfmt'), [
    '--disable-nested-config',
    directory,
  ]);

  return { directory, reportPath, samplesPath };
}

/** Renders the Markdown report from one completed run. */
export function renderReport(run: MeasurementRun, checks: TargetCheck[]): string {
  const lines: string[] = [];

  lines.push(`# Foundation measurement — ${run.startedAt}`);
  lines.push('');
  lines.push(
    'Read when comparing Stache Next release startup, idle cost and process trees ' +
      'against the legacy application.',
  );
  lines.push('');
  lines.push(
    `- Commit: \`${run.environment.repository.commit}\` on \`${run.environment.repository.branch}\``,
  );
  lines.push(`- Uncommitted changes: ${run.environment.repository.dirty ? 'yes' : 'no'}`);
  lines.push(`- Measured at: ${run.startedAt} (duration ${formatMs(run.durationMs)})`);
  lines.push(`- Command: \`${run.commandLine.join(' ')}\``);
  lines.push(`- Status: ${run.status.toUpperCase()}${statusDetail(run.failure)}`);
  lines.push('');

  if (run.failure !== null) {
    lines.push('## Failure');
    lines.push('');
    lines.push(
      'The run stopped before completing. Scenarios marked incomplete below hold partial ' +
        'measurements; cleanup issues, if any, are listed in the safety record.',
    );
    lines.push('');
    lines.push('```text');

    for (const line of run.failure.split('\n')) {
      lines.push(line);
    }

    lines.push('```');
    lines.push('');
  }

  lines.push('## Environment');
  lines.push('');
  lines.push('| Item | Value |');
  lines.push('| --- | --- |');
  lines.push(`| Machine | ${run.environment.machine.model} |`);
  lines.push(`| Chip | ${run.environment.machine.chip} |`);
  lines.push(`| Memory | ${formatBytes(run.environment.machine.memoryBytes)} |`);
  lines.push(
    `| macOS | ${run.environment.os.productVersion} (${run.environment.os.buildVersion}) |`,
  );
  lines.push(`| Rust | ${run.environment.toolchain.rustc} |`);
  lines.push(`| Cargo | ${run.environment.toolchain.cargo} |`);
  lines.push(`| Stache Next build | ${describeBuild(run.targets.next.build)} |`);

  if (run.targets.legacy !== null) {
    lines.push(`| Legacy build | ${describeBuild(run.targets.legacy.build)} |`);
  }

  lines.push(
    `| Power | ${run.environment.power.source}` +
      (run.environment.power.batteryPercent === null
        ? ''
        : `, ${run.environment.power.batteryPercent}%`) +
      ' |',
  );
  lines.push(`| Displays | ${formatDisplays(run)} |`);
  lines.push(`| Default input | ${run.environment.audio.defaultInput ?? '(none)'} |`);
  lines.push(`| Default output | ${run.environment.audio.defaultOutput ?? '(none)'} |`);
  lines.push('');

  lines.push('### Audio devices');
  lines.push('');
  lines.push('| Device | Transport | Input | Output | Default input | Default output | Class |');
  lines.push('| --- | --- | --- | --- | --- | --- | --- |');

  for (const device of run.environment.audio.devices) {
    const classes = [
      device.builtIn ? 'built-in' : '',
      device.hdmi ? 'HDMI' : '',
      device.airPlay ? 'AirPlay' : '',
    ]
      .filter((label) => label.length > 0)
      .join(', ');

    lines.push(
      markdownRow([
        device.name,
        device.transport ?? '',
        device.inputChannels === null ? '' : String(device.inputChannels),
        device.outputChannels === null ? '' : String(device.outputChannels),
        device.defaultInput ? 'yes' : '',
        device.defaultOutput ? 'yes' : '',
        classes,
      ]),
    );
  }

  lines.push('');

  lines.push('## Method');
  lines.push('');
  lines.push(
    '- Tray-ready (`external`): new host until the control socket answers `core.status`; ' +
      'legacy until its own `setup complete (background tasks spawned)` stderr line.',
  );
  lines.push('- External readiness is polled every 25 ms; the value includes process creation.');
  lines.push(
    '- Tray-created (`internal`): one host log line `startup: tray ready after N ms, ' +
      'webviews=N`, timed from process main entry with `Instant`.',
  );
  lines.push(
    `- Idle sampling: ${run.scenarios[0]?.idle?.sampleIntervalMs ?? 5_000} ms ` +
      'process-tree snapshots of the host and every transitive child.',
  );
  lines.push(
    '- CPU: average from `ps` cumulative CPU time deltas; instantaneous `%cpu` is not used.',
  );
  lines.push('- The sampler runs `ps` outside the measured tree; its own CPU is not attributed.');
  const idleWindowSeconds = run.scenarios[0]?.idle?.windowSeconds ?? 60;

  lines.push(
    `- CPU precision: \`ps\` time resolution is ${PS_CPU_TIME_RESOLUTION_SECONDS * 1_000} ms, ` +
      `which is about ${((PS_CPU_TIME_RESOLUTION_SECONDS / idleWindowSeconds) * 100).toFixed(3)} ` +
      `percentage points over the ${idleWindowSeconds.toFixed(0)}-second window for one process.`,
  );
  lines.push(
    '- RSS: sum of resident sizes. Shared pages are counted once per process, so tree RSS ' +
      'is an upper bound for distinct physical memory.',
  );
  lines.push(
    '- `cold` is the first launch in a scenario with an empty isolated cache; `warm` is a later launch.',
  );
  lines.push(
    '- External launch-to-ready timing includes process creation and is reported separately; ' +
      'the <300 ms target applies only to the internal host measurement.',
  );
  lines.push('');

  for (const scenario of run.scenarios) {
    renderScenario(lines, scenario);
  }

  lines.push('## Provisional targets');
  lines.push('');
  lines.push('| Target | Status | Evidence |');
  lines.push('| --- | --- | --- |');

  for (const check of checks) {
    lines.push(markdownRow([check.target, check.status.toUpperCase(), check.detail]));
  }

  lines.push('');
  lines.push(
    'The "≈0% idle CPU" target has no numeric budget in the spec. The tool reports the raw ' +
      `average and uses < ${NEAR_ZERO_CPU_PERCENT}% only as a noise-level label; it does not ` +
      'relax any target.',
  );
  lines.push('');

  const misses = checks.filter((check) => check.status === 'miss');

  lines.push('## Misses');
  lines.push('');

  if (misses.length === 0) {
    lines.push('None.');
  } else {
    for (const miss of misses) {
      lines.push(`- **${miss.target}**: ${miss.detail}`);
    }
  }

  lines.push('');

  lines.push('## Safety record');
  lines.push('');
  lines.push(
    `- Guard checks: ${
      run.safety.guardChecks.length === 0 ? 'none recorded' : run.safety.guardChecks.join('; ')
    }`,
  );
  lines.push(
    `- Temporary roots: ${
      run.safety.tempRoots.length === 0 ? 'none created' : run.safety.tempRoots.join(', ')
    }`,
  );
  lines.push(`- Temporary roots removed: ${run.safety.tempRootsRemoved ? 'yes' : 'no'}`);
  lines.push(
    `- Default audio devices: start ${describeAudioDefaults(run.safety.audioDefaultsAtStart)}, ` +
      `end ${
        run.safety.audioDefaultsAtEnd === null
          ? '(not observed)'
          : describeAudioDefaults(run.safety.audioDefaultsAtEnd)
      }`,
  );
  lines.push(
    `- Audio restore: ${
      run.safety.audioRestore === null
        ? 'not needed'
        : `${run.safety.audioRestore.succeeded ? 'succeeded' : 'FAILED'} — ${run.safety.audioRestore.detail}`
    }`,
  );
  lines.push(
    `- Signals forwarded to measured processes: ${
      run.safety.signals.length === 0
        ? 'none'
        : run.safety.signals
            .map((signal) => `${signal.signal} ${signal.pid} (${signal.reason})`)
            .join('; ')
    }`,
  );

  for (const note of run.safety.notes) {
    lines.push(`- ${note}`);
  }

  lines.push('');

  lines.push('## Limitations');
  lines.push('');

  for (const limitation of run.limitations) {
    lines.push(`- ${limitation}`);
  }

  lines.push('');

  return `${lines.join('\n')}\n`;
}

function renderScenario(lines: string[], scenario: ScenarioResult): void {
  lines.push(`## ${scenario.label}`);
  lines.push('');
  lines.push(`- Target: \`${scenario.target.executablePath}\``);
  lines.push(`- Bundle identifier: ${scenario.target.bundleIdentifier ?? '(bare executable)'}`);
  lines.push(
    `- Build: ${formatBytes(scenario.target.build.sizeBytes)} executable, modified ` +
      `${scenario.target.build.modifiedAt}; /release/ in path: ` +
      `${scenario.target.build.pathIncludesReleaseDirectory ? 'yes' : 'no'}`,
  );

  for (const annotation of scenario.annotations) {
    lines.push(`- ${annotation}`);
  }

  if (!scenario.complete) {
    lines.push(
      '- Incomplete: the run stopped during this scenario; the rows below are partial ' +
        'measurements, not a finished sample.',
    );
  }

  lines.push('');

  lines.push('### Configuration used');
  lines.push('');

  for (const file of scenario.configFiles) {
    lines.push(`\`${file.path}\``);
    lines.push('');
    lines.push('```json');
    lines.push(file.content.trimEnd());
    lines.push('```');
    lines.push('');
  }

  lines.push('### Launch timings');
  lines.push('');
  lines.push(
    '| # | Context | External launch -> ready | Internal main -> tray | Webviews | Ready source |',
  );
  lines.push('| --- | --- | --- | --- | --- | --- |');

  for (const launch of scenario.launches) {
    lines.push(
      markdownRow([
        String(launch.index + 1),
        launch.cold ? 'cold' : 'warm',
        formatMs(launch.externalReadyMs),
        formatMs(launch.internalTrayReadyMs),
        launch.instrumentedWebviews === null ? '' : String(launch.instrumentedWebviews),
        launch.readySource,
      ]),
    );
  }

  lines.push('');
  lines.push(
    renderTimingSummary('External launch -> ready', externalTimingStats(scenario.launches)),
  );
  lines.push(renderTimingSummary('Internal main -> tray', internalTimingStats(scenario.launches)));
  lines.push('');

  if (scenario.idle === null) {
    lines.push('### Idle sample');
    lines.push('');
    lines.push('Not collected.');
    lines.push('');

    return;
  }

  renderIdle(lines, scenario.idle);
}

function renderIdle(lines: string[], idle: IdleSample): void {
  lines.push('### Idle sample');
  lines.push('');
  lines.push(
    `- Window: ${idle.windowSeconds.toFixed(1)} s (${idle.treeSnapshots.length} snapshots)`,
  );
  lines.push(`- Host CPU average: ${formatPercent(idle.host.cpuPercent)}`);
  lines.push(`- Tree CPU average: ${formatPercent(idle.treeCpuPercent)}`);
  lines.push(
    `- Host RSS: final ${formatKiB(idle.host.rssFinalKiB)}, peak ${formatKiB(idle.host.rssPeakKiB)}`,
  );
  lines.push(
    `- Tree RSS: final ${formatKiB(idle.treeRssFinalKiB)}, peak ${formatKiB(idle.treeRssPeakKiB)} ` +
      `(${idle.processes.length} process(es))`,
  );

  for (const note of idle.notes) {
    lines.push(`- ${note}`);
  }

  lines.push('');

  lines.push('| PID | PPID | Role | CPU | RSS final | RSS peak | Coverage | Executable |');
  lines.push('| --- | --- | --- | --- | --- | --- | --- | --- |');

  for (const process of idle.processes) {
    const coverage = [
      process.presentInAllSnapshots ? 'all snapshots' : 'partial',
      process.exitedDuringSampling ? 'exited during sampling' : '',
    ]
      .filter((label) => label.length > 0)
      .join(', ');

    lines.push(
      markdownRow([
        String(process.pid),
        String(process.ppid),
        process.role,
        formatPercent(process.cpuPercent),
        formatKiB(process.rssFinalKiB),
        formatKiB(process.rssPeakKiB),
        coverage,
        process.executable,
      ]),
    );
  }

  lines.push('');

  lines.push('#### WebKit processes outside the measured tree');
  lines.push('');

  if (idle.unattributableWebKit.length === 0) {
    lines.push('None observed.');
  } else {
    lines.push(
      'Observed in the same user session but not attributable through the process tree; they ' +
        'may be shared with other applications and are excluded from tree totals.',
    );
    lines.push('');

    for (const process of idle.unattributableWebKit) {
      lines.push(
        `- pid ${process.pid} (ppid ${process.ppid}), RSS ${formatKiB(process.rssKiB)}: ` +
          `\`${process.executable}\``,
      );
    }
  }

  lines.push('');
}

function renderTimingSummary(label: string, stats: TimingStats): string {
  if (stats.warmMedianMs === null) {
    return `- ${label}: warm median unavailable; cold ${formatMs(stats.coldMs)}.`;
  }

  return (
    `- ${label}: warm median ${formatMs(stats.warmMedianMs)}, ` +
    `range ${formatRangeMs(stats)}, cold ${formatMs(stats.coldMs)}.`
  );
}

function externalTimingStats(launches: LaunchRecord[]): TimingStats {
  const coldMs = launches[0]?.externalReadyMs ?? null;
  const warm = launches.slice(1).map((launch) => launch.externalReadyMs);

  return summarizeTiming(coldMs, warm);
}

function internalTimingStats(launches: LaunchRecord[]): TimingStats {
  const coldMs = launches[0]?.internalTrayReadyMs ?? null;
  const warm = launches
    .slice(1)
    .map((launch) => launch.internalTrayReadyMs)
    .filter((value): value is number => value !== null);

  return summarizeTiming(coldMs, warm);
}

function summarizeTiming(coldMs: number | null, warm: number[]): TimingStats {
  if (warm.length === 0) {
    return { coldMs, warmMedianMs: null, warmMinMs: null, warmMaxMs: null };
  }

  return {
    coldMs,
    warmMedianMs: median(warm),
    warmMinMs: Math.min(...warm),
    warmMaxMs: Math.max(...warm),
  };
}

function median(values: number[]): number {
  const ordered = [...values].sort((left, right) => left - right);
  const middle = Math.floor(ordered.length / 2);

  if (ordered.length % 2 === 1) {
    return ordered[middle];
  }

  return (ordered[middle - 1] + ordered[middle]) / 2;
}

function formatRangeMs(stats: TimingStats): string {
  if (stats.warmMinMs === null || stats.warmMaxMs === null) {
    return '—';
  }

  return `${formatMs(stats.warmMinMs)}–${formatMs(stats.warmMaxMs)}`;
}

function formatMs(value: number | null): string {
  return value === null ? '—' : `${Math.round(value)} ms`;
}

function formatPercent(value: number): string {
  return `${value.toFixed(2)} %`;
}

function formatKiB(value: number): string {
  return `${(value / 1_024).toFixed(1)} MiB`;
}

function formatBytes(value: number): string {
  if (value >= 1_024 ** 3) {
    return `${(value / 1_024 ** 3).toFixed(0)} GiB`;
  }

  if (value >= 1_024 ** 2) {
    return `${(value / 1_024 ** 2).toFixed(1)} MiB`;
  }

  if (value >= 1_024) {
    return `${(value / 1_024).toFixed(0)} KiB`;
  }

  return `${value} B`;
}

function formatDisplays(run: MeasurementRun): string {
  if (run.environment.displays.length === 0) {
    return 'none reported';
  }

  return run.environment.displays
    .map((display) => {
      const name = display.name ?? 'unnamed display';
      const resolution = display.resolution ?? 'unknown resolution';
      const flags = [
        display.main ? 'main' : '',
        display.mirroring ? 'mirroring' : '',
        display.online ? 'online' : 'offline',
      ]
        .filter((flag) => flag.length > 0)
        .join(', ');

      return `${name} (${resolution}; ${flags})`;
    })
    .join('; ');
}

function describeAudioDefaults(defaults: { input: string | null; output: string | null }): string {
  return `input ${defaults.input ?? '(none)'}, output ${defaults.output ?? '(none)'}`;
}

function describeBuild(build: BuildInfo): string {
  return (
    `\`${build.executablePath}\`, ${formatBytes(build.sizeBytes)}, modified ${build.modifiedAt}; ` +
    `/release/ in path: ${build.pathIncludesReleaseDirectory ? 'yes' : 'no'}`
  );
}

function markdownRow(cells: string[]): string {
  return `| ${cells.map(escapeCell).join(' | ')} |`;
}

function statusDetail(failure: string | null): string {
  if (failure === null) {
    return '';
  }

  return ` — ${escapeCell(failure.split('\n')[0] ?? failure)}`;
}

function escapeCell(value: string): string {
  return value.replaceAll('|', '\\|').replaceAll('\n', ' ');
}
