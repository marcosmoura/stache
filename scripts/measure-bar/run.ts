import { type ChildProcess, spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

import { createTempRoot, removeTempRoot } from '../measure/config';
import { sampleIdle } from '../measure/idle';
import { InterruptedError, sleepUnlessInterrupted, throwIfInterrupted } from '../measure/interrupt';
import { controlSocketPath, waitForReady, type StartedProcess } from '../measure/launch';
import { REPOSITORY_ROOT } from '../measure/paths';
import {
  findRunningStacheProcesses,
  runCommand,
  stopOwnedProcessTree,
  type RecordSignal,
} from '../measure/process';
import { collectEnvironment } from '../measure/system';
import { resolveTarget } from '../measure/targets';
import type { Target } from '../measure/types';

import { assessBusyness, describeBusy } from './busy';
import {
  armOrder,
  pairOf,
  renderBarToml,
  renderGlobalToml,
  seriesEnvironment,
  seriesLabel,
} from './plan';
import { renderBarReport, writeBarOutputs, type BarOutputPaths } from './report';
import { buildMetrics, hostCpuSeries, summarizeTrials } from './stats';
import type {
  AccessState,
  Arm,
  BarOptions,
  BarRun,
  HostWindow,
  TrialRecord,
  TrialVerdict,
} from './types';
import {
  isBarPanel,
  judgeAccess,
  judgeTrial,
  parseCoreStatusAccess,
  parseHostWindows,
} from './validity';
import { parseTopWakeups, treeWakeupsPerSecond } from './wakeups';

export { InterruptedError };

const WINDOW_HELPER = fileURLToPath(new URL('./list-windows.swift', import.meta.url));
const ACCESS_WAIT_MS = 15_000;
const PANEL_WAIT_MS = 20_000;
const POLL_MS = 1_000;
const MAXIMUM_INVALID_TRIALS = 3;

export interface BarRunOutcome {
  outputs: BarOutputPaths;
  clean: boolean;
  failure: Error | null;
  interrupted: boolean;
}

/**
 * Runs the Bar-off versus Bar-on schedule and writes report.md and samples.json.
 *
 * Only children this tool spawned are ever signalled, through the foundation
 * tool's owned-tree shutdown; nothing is terminated by name. The raw samples are
 * written even when the run fails or is interrupted.
 */
export async function runBarMeasurement(
  options: BarOptions,
  outputRoot: string = REPOSITORY_ROOT,
): Promise<BarRunOutcome> {
  const startedAt = new Date().toISOString();
  const target = resolveTarget('next', options.appPath);

  if (target.bundlePath === null) {
    throw new Error(
      'the Bar measurement needs a release .app bundle (Accessibility trust is per .app path), ' +
        `not a bare executable: ${options.appPath}`,
    );
  }

  const order = armOrder(options.launchesPerArm);
  const run: BarRun = {
    startedAt,
    finishedAt: startedAt,
    status: 'failed',
    failure: null,
    commandLine: [...process.argv],
    options,
    appPath: target.bundlePath,
    executablePath: target.executablePath,
    series: seriesLabel(options.ssid),
    environment: null,
    preflight: null,
    armOrder: order,
    trials: [],
    summary: null,
    safety: { guardChecks: [], tempRoots: [], tempRootsRemoved: false, signals: [], notes: [] },
    limitations: limitations(),
  };
  const recordSignal: RecordSignal = (pid, signal, reason) => {
    run.safety.signals.push({ at: new Date().toISOString(), pid, signal, reason });
  };
  const state = {
    active: null as ChildProcess | null,
    interrupted: false,
    lingeringRoot: null as string | null,
  };
  const onSignal = (signal: 'SIGINT' | 'SIGTERM'): void => {
    if (state.interrupted) {
      return;
    }

    state.interrupted = true;
    run.safety.notes.push(`received ${signal}; stopping the active measurement`);

    const pid = state.active?.pid;

    if (state.active?.kill('SIGTERM') && pid !== undefined) {
      recordSignal(pid, 'SIGTERM', `stop active measurement after ${signal}`);
    }
  };
  const onSigint = (): void => onSignal('SIGINT');
  const onSigterm = (): void => onSignal('SIGTERM');
  const isInterrupted = (): boolean => state.interrupted;

  process.on('SIGINT', onSigint);
  process.on('SIGTERM', onSigterm);

  let failure: Error | null = null;

  try {
    assertNoStache(target, run);
    run.environment = collectEnvironment();
    console.log(`measure:bar pre-flight idle check (${options.preflightSeconds} s)`);
    const preflightSample = await sampleIdle(1, options.preflightSeconds, isInterrupted);

    run.preflight = assessBusyness(preflightSample, options.maxBusyCpuPercent);

    if (run.preflight.busy) {
      throw new Error(describeBusy(run.preflight));
    }

    for (const [index, arm] of order.entries()) {
      throwIfInterrupted(isInterrupted, `interrupted before trial ${index + 1}`);
      assertNoStache(target, run);
      const trial = await runTrial(target, options, arm, index, run, state, recordSignal);

      run.trials.push(trial);
      console.log(
        `measure:bar trial ${index + 1}/${order.length} ${arm}: ` +
          `${trial.valid ? 'valid' : `INVALID (${trial.invalidReason})`} - ${trial.message}`,
      );

      if (trial.invalidReason === 'access-blocked' || trial.invalidReason === 'access-unreported') {
        throw new Error(trial.message);
      }

      if (run.trials.filter((entry) => !entry.valid).length >= MAXIMUM_INVALID_TRIALS) {
        throw new Error(
          `${MAXIMUM_INVALID_TRIALS} invalid trials; stopping. Last: ${trial.message}`,
        );
      }
    }
  } catch (error) {
    failure = error instanceof Error ? error : new Error(String(error));
  } finally {
    process.off('SIGINT', onSigint);
    process.off('SIGTERM', onSigterm);
  }

  removeRoots(run, state.lingeringRoot);

  if (state.interrupted && failure === null) {
    failure = new InterruptedError('interrupted during cleanup');
  }

  const interrupted = failure instanceof InterruptedError;

  run.finishedAt = new Date().toISOString();
  run.status = failure === null ? 'completed' : interrupted ? 'interrupted' : 'failed';
  run.failure = failure?.message ?? null;
  run.summary =
    run.trials.length === 0 ? null : summarizeTrials(run.trials, options.launchesPerArm);

  if (failure === null && run.summary !== null && !run.summary.sufficient) {
    run.status = 'failed';
    run.failure = 'fewer valid launches than requested in at least one arm';
  }

  const outputs = writeBarOutputs(run, renderBarReport(run), outputRoot);

  return { outputs, clean: run.safety.tempRootsRemoved, failure, interrupted };
}

async function runTrial(
  target: Target,
  options: BarOptions,
  arm: Arm,
  index: number,
  run: BarRun,
  state: { active: ChildProcess | null; interrupted: boolean; lingeringRoot: string | null },
  recordSignal: RecordSignal,
): Promise<TrialRecord> {
  const isInterrupted = (): boolean => state.interrupted;
  const trial: TrialRecord = {
    index,
    pair: pairOf(index),
    arm,
    valid: false,
    invalidReason: null,
    message: '',
    readyMs: null,
    accessState: null,
    windowsAfterWarmup: null,
    windowsAfterSample: null,
    metrics: null,
    hostCpuSeries: [],
    idle: null,
    topOutput: null,
    stderrTail: [],
  };
  const root = createTempRoot();

  run.safety.tempRoots.push(root);
  const configDirectory = path.join(root, 'config', 'stache');

  mkdirSync(configDirectory, { recursive: true });
  writeFileSync(path.join(configDirectory, 'global.toml'), renderGlobalToml(), 'utf8');
  writeFileSync(
    path.join(configDirectory, 'bar.toml'),
    renderBarToml(arm, options.height, options.padding),
    'utf8',
  );

  const started = startHost(target, configDirectory, root, options);

  state.active = started.child;
  let stopFailed = false;

  try {
    const ready = await waitForReady(started, target, controlSocketPath(root), isInterrupted);

    trial.readyMs = ready.externalReadyMs;
    const hostPid = requirePid(started.child);

    trial.accessState = await waitForAccess(controlSocketPath(root), isInterrupted);
    const early = judgeAccess(trial.accessState, target.bundlePath ?? target.executablePath);

    if (!early.valid) {
      return finishInvalid(trial, early);
    }

    await sleepUnlessInterrupted(options.warmupSeconds * 1_000, isInterrupted);
    trial.windowsAfterWarmup = await waitForWindows(hostPid, arm, options.height, isInterrupted);
    const verdict = judgeTrial({
      arm,
      appPath: target.bundlePath ?? target.executablePath,
      height: options.height,
      access: trial.accessState,
      windows: trial.windowsAfterWarmup,
    });

    if (!verdict.valid) {
      return finishInvalid(trial, verdict);
    }

    const [idle, topOutput] = await Promise.all([
      sampleIdle(hostPid, options.sampleSeconds, isInterrupted),
      readTopOutput(options.sampleSeconds, isInterrupted),
    ]);

    trial.idle = idle;
    trial.topOutput = topOutput;
    trial.hostCpuSeries = hostCpuSeries(idle.treeSnapshots, hostPid);
    const pids = idle.treeSnapshots.flatMap((snapshot) => snapshot.processes.map((row) => row.pid));
    const wakeups =
      topOutput === null
        ? null
        : treeWakeupsPerSecond(parseTopWakeups(topOutput), pids, idle.windowSeconds);

    trial.metrics = buildMetrics(idle.host, idle.treeCpuPercent, trial.hostCpuSeries, wakeups);
    trial.windowsAfterSample = await listWindows(hostPid);
    const after = judgeTrial({
      arm,
      appPath: target.bundlePath ?? target.executablePath,
      height: options.height,
      access: trial.accessState,
      windows: trial.windowsAfterSample,
    });

    if (!after.valid) {
      trial.metrics = null;

      return finishInvalid(trial, {
        valid: false,
        reason: arm === 'on' ? 'bar-panel-lost' : 'unexpected-window',
        message: `after sampling: ${after.message}`,
      });
    }

    trial.valid = true;
    trial.message = wakeups === null ? 'ok (idle wakeups unavailable)' : 'ok';

    return trial;
  } catch (error) {
    if (error instanceof InterruptedError) {
      throw error;
    }

    trial.invalidReason = started.child.exitCode !== null ? 'host-exited' : 'measurement-error';
    trial.message = error instanceof Error ? error.message.split('\n')[0] : String(error);
    trial.metrics = null;

    return trial;
  } finally {
    trial.stderrTail = started.stderrTail();

    try {
      const outcome = await stopOwnedProcessTree(started.child, recordSignal);

      stopFailed = outcome.lingering.length > 0;
    } catch (error) {
      stopFailed = true;
      run.safety.notes.push(
        `could not confirm the host stopped: ${error instanceof Error ? error.message : String(error)}`,
      );
    }

    if (stopFailed) {
      state.lingeringRoot = root;
    }

    state.active = null;
  }
}

function finishInvalid(trial: TrialRecord, verdict: TrialVerdict): TrialRecord {
  if (!verdict.valid) {
    trial.invalidReason = verdict.reason;
    trial.message = verdict.message;
  }

  trial.valid = false;
  trial.metrics = null;

  return trial;
}

function startHost(
  target: Target,
  configDirectory: string,
  root: string,
  options: BarOptions,
): StartedProcess {
  const stderrLines: string[] = [];
  const startedAt = performance.now();
  const child = spawn(target.executablePath, ['--config', configDirectory, '--desktop'], {
    cwd: root,
    env: {
      PATH: process.env.PATH ?? '/usr/bin:/bin:/usr/sbin:/sbin',
      HOME: root,
      TMPDIR: root,
      XDG_CONFIG_HOME: path.join(root, 'config'),
      ...seriesEnvironment(options.ssid),
    },
    // Own process group: a terminal SIGINT must not reach the measured app.
    detached: true,
    stdio: ['ignore', 'ignore', 'pipe'],
  });

  if (child.stderr === null) {
    throw new Error(`could not capture stderr from ${target.executablePath}`);
  }

  createInterface({ input: child.stderr }).on('line', (line) => {
    stderrLines.push(line.trim());

    if (stderrLines.length > 80) {
      stderrLines.shift();
    }
  });
  child.on('error', (error) => stderrLines.push(`spawn error: ${error.message}`));

  return {
    child,
    startedAt,
    internalTrayReadyMs: () => null,
    instrumentedWebviews: () => null,
    legacyReady: () => false,
    stderrTail: () => [...stderrLines],
  };
}

function requirePid(child: ChildProcess): number {
  if (child.pid === undefined) {
    throw new Error('spawned process has no pid');
  }

  return child.pid;
}

/** Any Stache Next or legacy instance blocks the run; nothing is ever terminated. */
function assertNoStache(target: Target, run: BarRun): void {
  let running: ReturnType<typeof findRunningStacheProcesses>;

  try {
    running = findRunningStacheProcesses(target, null);
  } catch (error) {
    throw new Error(
      'Refusing to start: could not verify that no Stache instance is running: ' +
        (error instanceof Error ? error.message : String(error)),
    );
  }

  if (running.length > 0) {
    throw new Error(
      [
        'Refusing to start: another Stache Next or legacy Stache process is running.',
        ...running.map((entry) => `  ${entry.kind} pid ${entry.pid ?? '?'}: ${entry.description}`),
        'Quit it from its own tray or menu. This tool never signals another instance.',
      ].join('\n'),
    );
  }

  run.safety.guardChecks.push(
    `no Stache Next or legacy Stache process at ${new Date().toISOString()}`,
  );
}

async function waitForAccess(
  socketPath: string,
  isInterrupted: () => boolean,
): Promise<AccessState | 'unreported' | null> {
  const deadline = Date.now() + ACCESS_WAIT_MS;
  let last: AccessState | 'unreported' | null = null;

  while (Date.now() < deadline) {
    throwIfInterrupted(isInterrupted, 'interrupted while waiting for the Accessibility gate');
    last = await readAccess(socketPath);

    // `checking` is transient; `blocked`, `open` and an old host are final.
    if (last !== null && last !== 'checking') {
      return last;
    }

    await sleep(250);
  }

  return last;
}

async function readAccess(socketPath: string): Promise<AccessState | 'unreported' | null> {
  return await new Promise((resolve) => {
    const socket = net.createConnection({ path: socketPath });
    let buffer = '';
    let settled = false;
    const finish = (value: AccessState | 'unreported' | null): void => {
      if (!settled) {
        settled = true;
        clearTimeout(timer);
        socket.destroy();
        resolve(value);
      }
    };
    const timer = setTimeout(() => finish(null), 1_000);

    socket.setEncoding('utf8');
    socket.on('connect', () => socket.write('{"module":"core","method":"status","params":null}\n'));
    socket.on('data', (chunk: string) => {
      buffer += chunk;
      const newline = buffer.indexOf('\n');

      if (newline >= 0) {
        finish(parseCoreStatusAccess(buffer.slice(0, newline)));
      }
    });
    socket.on('error', () => finish(null));
    socket.on('close', () => finish(null));
  });
}

async function listWindows(pid: number): Promise<HostWindow[]> {
  return parseHostWindows(runCommand('/usr/bin/swift', [WINDOW_HELPER, String(pid)]));
}

/** Polls the host's windows: the on arm until the Bar panel shows, the off arm once. */
async function waitForWindows(
  pid: number,
  arm: Arm,
  height: number,
  isInterrupted: () => boolean,
): Promise<HostWindow[]> {
  const deadline = Date.now() + PANEL_WAIT_MS;
  let windows = await listWindows(pid);

  while (arm === 'on' && !windows.some((window) => isBarPanel(window, height))) {
    if (Date.now() >= deadline) {
      break;
    }

    throwIfInterrupted(isInterrupted, 'interrupted while waiting for the Bar panel');
    await sleep(POLL_MS);
    windows = await listWindows(pid);
  }

  return windows;
}

/**
 * Runs `top -l 2` for the sample window; its second sample holds the idle
 * wakeups of that interval. Null when `top` fails or is interrupted.
 */
async function readTopOutput(
  intervalSeconds: number,
  isInterrupted: () => boolean,
): Promise<string | null> {
  return await new Promise((resolve) => {
    const child = spawn(
      '/usr/bin/top',
      ['-l', '2', '-s', String(intervalSeconds), '-stats', 'pid,idlew'],
      {
        stdio: ['ignore', 'pipe', 'ignore'],
      },
    );
    let output = '';
    const watchdog = setInterval(() => {
      if (isInterrupted()) {
        child.kill('SIGTERM');
      }
    }, 200);
    const timeout = setTimeout(() => child.kill('SIGTERM'), (intervalSeconds + 30) * 1_000);

    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => {
      output += chunk;
    });
    child.on('error', () => settle(null));
    child.on('close', (code) => settle(code === 0 ? output : null));

    function settle(value: string | null): void {
      clearInterval(watchdog);
      clearTimeout(timeout);
      resolve(value);
    }
  });
}

function removeRoots(run: BarRun, keepRoot: string | null): void {
  let removed = 0;

  for (const root of run.safety.tempRoots) {
    if (root === keepRoot) {
      run.safety.notes.push(
        `kept temporary root ${root}: its process tree was not confirmed stopped`,
      );
      continue;
    }

    try {
      removeTempRoot(root);
      removed += 1;
    } catch (error) {
      run.safety.notes.push(
        `could not remove ${root}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  run.safety.tempRootsRemoved = keepRoot === null && removed === run.safety.tempRoots.length;
}

function limitations(): string[] {
  return [
    'The control socket exposes only the Accessibility gate (`core.status`), not the Bar module ' +
      'state or panel phase. Bar "on" is proven by an open gate plus a host-owned window of the ' +
      'configured Bar height (CGWindowList: owner pid, bounds; no Screen Recording needed). A ' +
      'loaded page is not separately observed.',
    'Idle wakeups come from the second sample of `top -l 2 -stats pid,idlew` over the sampling ' +
      'window, summed over every pid seen in the host tree; only compare values from this tool.',
    'Host CPU peak is the highest average over one 5 s snapshot interval, not an instantaneous maximum.',
    '`ps` cumulative CPU time has 10 ms resolution.',
    'The pre-flight sums per-process CPU over the whole process table; short-lived processes are not captured.',
    'Dock, Spaces and display state are not controlled; the machine is shared with unrelated activity.',
    'Bar-on runs after a Bar-off launch on a cold app cache for neither arm; page cache is not flushed.',
  ];
}
