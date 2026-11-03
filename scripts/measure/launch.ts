import { type ChildProcess, spawn } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as sleep } from 'node:timers/promises';

import type { ScenarioConfig } from './config';
import { throwIfInterrupted } from './interrupt';
import { READY_TIMEOUT_MS } from './paths';
import type { Target } from './types';

const STDERR_TAIL_LINES = 80;
const READY_POLL_INTERVAL_MS = 25;
const SOCKET_PROBE_TIMEOUT_MS = 250;

export interface StartedProcess {
  child: ChildProcess;
  startedAt: number;
  internalTrayReadyMs(): number | null;
  instrumentedWebviews(): number | null;
  legacyReady(): boolean;
  stderrTail(): string[];
}

interface ProcessState {
  internalTrayReadyMs: number | null;
  instrumentedWebviews: number | null;
  legacyReady: boolean;
  stderrLines: string[];
}

/** Starts one isolated target and keeps parsing its readiness evidence. */
export function startTarget(target: Target, config: ScenarioConfig, root: string): StartedProcess {
  const state: ProcessState = {
    internalTrayReadyMs: null,
    instrumentedWebviews: null,
    legacyReady: false,
    stderrLines: [],
  };

  const args = ['--config', target.kind === 'legacy' ? config.files[0].path : config.directory];
  args.push('--desktop');

  const startedAt = performance.now();
  const child = spawn(target.executablePath, args, {
    cwd: root,
    env: childEnvironment(target, root),
    // A separate process group keeps terminal SIGINT away from the measured app;
    // the tool forwards SIGTERM explicitly to the process it owns.
    detached: true,
    stdio: ['ignore', 'ignore', 'pipe'],
  });

  if (child.stderr === null) {
    throw new Error(`could not capture stderr from ${target.executablePath}`);
  }
  const lines = createInterface({ input: child.stderr });
  lines.on('line', (line) => {
    const trimmed = line.trim();
    state.stderrLines.push(trimmed);
    if (state.stderrLines.length > STDERR_TAIL_LINES) {
      state.stderrLines.shift();
    }
    const startup = /startup: tray ready after (\d+) ms, webviews=(\d+)/.exec(trimmed);
    if (startup !== null && state.internalTrayReadyMs === null) {
      state.internalTrayReadyMs = Number.parseInt(startup[1], 10);
      state.instrumentedWebviews = Number.parseInt(startup[2], 10);
    }
    if (trimmed.includes('setup complete (background tasks spawned)')) {
      state.legacyReady = true;
    }
  });

  child.on('error', (error) => {
    state.stderrLines.push(`spawn error: ${error.message}`);
  });

  return {
    child,
    startedAt,
    internalTrayReadyMs: () => state.internalTrayReadyMs,
    instrumentedWebviews: () => state.instrumentedWebviews,
    legacyReady: () => state.legacyReady,
    stderrTail: () => [...state.stderrLines],
  };
}

function childEnvironment(target: Target, root: string): NodeJS.ProcessEnv {
  const environment: NodeJS.ProcessEnv = {
    PATH: process.env.PATH ?? '/usr/bin:/bin:/usr/sbin:/sbin',
    HOME: root,
    TMPDIR: root,
    XDG_CONFIG_HOME: path.join(root, 'config'),
  };
  if (target.kind === 'legacy') {
    environment.RUST_LOG = 'info';
  }
  return environment;
}

export interface ReadyResult {
  externalReadyMs: number;
  readySource: string;
}

/**
 * Waits for the documented readiness acknowledgement.
 *
 * Stache Next: the control socket answers `core.status` (the socket starts after
 * the tray exists). Legacy: its own `setup complete (background tasks spawned)`
 * stderr line, emitted after tray, bar and widget creation.
 *
 * `isInterrupted` ends the wait as soon as a signal arrives, instead of polling
 * a child that the signal handler already killed.
 */
export async function waitForReady(
  started: StartedProcess,
  target: Target,
  socketPath: string,
  isInterrupted: () => boolean = () => false,
): Promise<ReadyResult> {
  if (started.child.pid === undefined) {
    throw new Error(
      `failed to spawn ${target.executablePath}:\n${started.stderrTail().join('\n')}`,
    );
  }
  const deadline = started.startedAt + READY_TIMEOUT_MS;
  while (performance.now() < deadline) {
    throwIfInterrupted(isInterrupted, 'interrupted while waiting for readiness');
    if (started.child.exitCode !== null || started.child.signalCode !== null) {
      throw new Error(
        `${target.kind} target exited before readiness:\n${started.stderrTail().join('\n')}`,
      );
    }
    if (target.kind === 'next') {
      if (await coreStatusAnswers(socketPath)) {
        return {
          externalReadyMs: performance.now() - started.startedAt,
          readySource: 'core.status over the control socket',
        };
      }
    } else if (started.legacyReady()) {
      return {
        externalReadyMs: performance.now() - started.startedAt,
        readySource: 'legacy stderr: setup complete (background tasks spawned)',
      };
    }
    await sleep(READY_POLL_INTERVAL_MS);
  }
  throw new Error(
    `${target.kind} target did not become ready within ${READY_TIMEOUT_MS} ms:\n` +
      started.stderrTail().join('\n'),
  );
}

/** Path of the new host's control socket under one isolated temporary root. */
export function controlSocketPath(root: string): string {
  return path.join(root, 'com.marcosmoura.stache.next', 'ipc');
}

async function coreStatusAnswers(socketPath: string): Promise<boolean> {
  return await new Promise<boolean>((resolve) => {
    const socket = net.createConnection({ path: socketPath });
    let buffer = '';
    let settled = false;

    const finish = (answer: boolean): void => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      resolve(answer);
    };
    const timer = setTimeout(() => finish(false), SOCKET_PROBE_TIMEOUT_MS);

    socket.setEncoding('utf8');
    socket.on('connect', () => {
      socket.write('{"module":"core","method":"status","params":null}\n');
    });
    socket.on('data', (chunk: string) => {
      buffer += chunk;
      const newline = buffer.indexOf('\n');
      if (newline >= 0) {
        finish(isOkResponse(buffer.slice(0, newline)));
      }
    });
    socket.on('error', () => finish(false));
    socket.on('close', () => finish(false));
  });
}

function isOkResponse(line: string): boolean {
  try {
    const parsed: unknown = JSON.parse(line);
    if (typeof parsed !== 'object' || parsed === null) {
      return false;
    }
    return (parsed as { status?: unknown }).status === 'ok';
  } catch {
    return false;
  }
}
