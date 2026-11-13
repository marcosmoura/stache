import { type ChildProcess, spawnSync } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';

import {
  DEVELOPMENT_BUNDLE_IDENTIFIER,
  NEXT_BUNDLE_IDENTIFIER,
  SHUTDOWN_TIMEOUT_MS,
} from './paths';
import type { ProcessRow, Target, TargetKind } from './types';

export class CommandExecutionError extends Error {
  constructor(command: string, args: string[], status: number | null, stderr: string) {
    super(
      status === null
        ? `${command} did not start: ${stderr}`
        : `${command} ${args.join(' ')} exited with ${status}: ${stderr.trim()}`,
    );
  }
}

/** Runs a short local command and returns its standard output. */
export function runCommand(command: string, args: string[]): string {
  const result = spawnSync(command, args, {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  });

  if (result.error !== undefined) {
    throw new CommandExecutionError(command, args, null, result.error.message);
  }

  if (result.status !== 0) {
    throw new CommandExecutionError(command, args, result.status, result.stderr);
  }

  return result.stdout;
}

/** Whether an executable name resolves through `/usr/bin/which`. */
export function commandExists(name: string): boolean {
  const result = spawnSync('/usr/bin/which', [name], { encoding: 'utf8' });

  return result.status === 0 && result.stdout.trim().length > 0;
}

/** One process-table snapshot, root included. */
export function listProcesses(): ProcessRow[] {
  const output = runCommand('/bin/ps', ['-axo', 'pid=,ppid=,rss=,time=,comm=']);

  return output
    .split('\n')
    .map(parseProcessLine)
    .filter((row): row is ProcessRow => row !== null);
}

function parseProcessLine(line: string): ProcessRow | null {
  const match = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(.+?)\s*$/.exec(line);

  if (match === null) {
    return null;
  }

  const cpuTimeSeconds = parseCpuTime(match[4]);

  if (cpuTimeSeconds === null) {
    return null;
  }

  return {
    pid: Number.parseInt(match[1], 10),
    ppid: Number.parseInt(match[2], 10),
    rssKiB: Number.parseInt(match[3], 10),
    cpuTimeSeconds,
    executable: match[5],
  };
}

/** Parses `ps -o time` values such as `0:01.23`, `1:02:03` or `1-02:03:04`. */
export function parseCpuTime(value: string): number | null {
  const [daysPart, clockPart] = value.includes('-') ? value.split('-', 2) : ['0', value];
  const days = Number.parseInt(daysPart, 10);

  if (!Number.isFinite(days) || days < 0) {
    return null;
  }

  const segments = clockPart.split(':').map((segment) => Number.parseFloat(segment));

  if (segments.length === 0 || segments.some((segment) => !Number.isFinite(segment))) {
    return null;
  }

  let seconds = 0;

  for (const segment of segments) {
    seconds = seconds * 60 + segment;
  }

  return days * 86_400 + seconds;
}

/** The root process and every transitive child present in one snapshot. */
export function processTree(rootPid: number, rows: ProcessRow[]): ProcessRow[] {
  const childrenByParent = new Map<number, ProcessRow[]>();

  for (const row of rows) {
    const siblings = childrenByParent.get(row.ppid) ?? [];

    siblings.push(row);
    childrenByParent.set(row.ppid, siblings);
  }

  const tree: ProcessRow[] = [];
  const seen = new Set<number>();
  const queue = [rootPid];

  while (queue.length > 0) {
    const pid = queue.shift();

    if (pid === undefined || seen.has(pid)) {
      continue;
    }

    seen.add(pid);
    const row = rows.find((candidate) => candidate.pid === pid);

    if (row === undefined) {
      continue;
    }

    tree.push(row);

    for (const child of childrenByParent.get(pid) ?? []) {
      queue.push(child.pid);
    }
  }

  return tree;
}

/** Reads the process tree under one root, or an empty list when it is gone. */
export function readProcessTree(rootPid: number): ProcessRow[] {
  return processTree(rootPid, listProcesses());
}

/** One GUI application reported by `lsappinfo`. */
export interface RunningApplication {
  name: string;
  bundleIdentifier: string | null;
  executablePath: string | null;
  pid: number | null;
}

/** Reads Launch Services' view of running GUI applications. */
export function readRunningApplications(
  command: typeof runCommand = runCommand,
): RunningApplication[] {
  // A failed read must never look like "nothing is running": the exclusivity
  // guard treats an unreadable process list as a refusal, not as a pass.
  return parseRunningApplications(command('/usr/bin/lsappinfo', ['list']));
}

function parseRunningApplications(output: string): RunningApplication[] {
  const applications: RunningApplication[] = [];
  let current: RunningApplication | null = null;

  for (const line of output.split('\n')) {
    const header = /^\s*\d+\)\s+"(.*)"\s+ASN:/.exec(line);

    if (header !== null) {
      if (current !== null) {
        applications.push(current);
      }

      current = {
        name: header[1],
        bundleIdentifier: null,
        executablePath: null,
        pid: null,
      };
      continue;
    }

    if (current === null) {
      continue;
    }

    const bundleIdentifier = /^\s*bundleID="(.*)"\s*$/.exec(line);

    if (bundleIdentifier !== null && bundleIdentifier[1] !== '[ NULL ]') {
      current.bundleIdentifier = bundleIdentifier[1];
      continue;
    }

    const executablePath = /^\s*executable path="(.*)"\s*$/.exec(line);

    if (executablePath !== null) {
      current.executablePath = executablePath[1];
      continue;
    }

    const pid = /^\s*pid = (\d+)\b/.exec(line);

    if (pid !== null) {
      current.pid = Number.parseInt(pid[1], 10);
    }
  }

  if (current !== null) {
    applications.push(current);
  }

  return applications;
}

/** One running process of a Stache identity. */
export interface RunningStacheProcess {
  kind: TargetKind;
  pid: number | null;
  source: 'lsappinfo' | 'ps';
  description: string;
}

/**
 * Finds any process of either Stache identity.
 *
 * Detection uses Launch Services bundle identifiers plus exact executable and
 * bundle prefixes from the process table. It never signals or terminates a
 * process; the caller decides which identities block the run and refuses to
 * start when one of them is present.
 *
 * Throws when the process table cannot be read, so an unknown state is never
 * reported as a passed guard.
 */
export function findRunningStacheProcesses(
  next: Target,
  legacy: Target | null,
  excludedPids: number[] = [],
): RunningStacheProcess[] {
  const excluded = new Set(excludedPids);
  const found = new Map<string, RunningStacheProcess>();

  for (const application of readRunningApplications()) {
    if (application.pid !== null && excluded.has(application.pid)) {
      continue;
    }

    const kind = classifyApplication(application, legacy);

    if (kind === null) {
      continue;
    }

    const pid = application.pid;
    const key = `${kind}:${pid ?? application.bundleIdentifier}`;

    found.set(key, {
      kind,
      pid,
      source: 'lsappinfo',
      description: `${application.name} (${application.executablePath ?? 'unknown path'})`,
    });
  }

  for (const row of listProcesses()) {
    if (excluded.has(row.pid)) {
      continue;
    }

    const kind = classifyExecutable(row.executable, next, legacy);

    if (kind === null) {
      continue;
    }

    const key = `${kind}:${row.pid}`;

    if (!found.has(key)) {
      found.set(key, {
        kind,
        pid: row.pid,
        source: 'ps',
        description: row.executable,
      });
    }
  }

  return [...found.values()];
}

/**
 * Classifies one Launch Services entry. The release and legacy identifiers are
 * equal, so an entry with that identifier is `legacy` only when its executable
 * is the explicit `--legacy-app` target; otherwise it is the measured identity.
 * A bundled development app ({@link DEVELOPMENT_BUNDLE_IDENTIFIER}) is classified with
 * it so it blocks too; nothing here signals it.
 */
export function classifyApplication(
  application: RunningApplication,
  legacy: Target | null,
): TargetKind | null {
  if (application.bundleIdentifier === DEVELOPMENT_BUNDLE_IDENTIFIER) {
    return 'next';
  }

  if (application.bundleIdentifier !== NEXT_BUNDLE_IDENTIFIER) {
    return null;
  }

  if (
    legacy !== null &&
    application.executablePath !== null &&
    matchesTargetExecutable(application.executablePath, legacy)
  ) {
    return 'legacy';
  }

  return 'next';
}

/**
 * Classifies one process-table executable. Legacy comes only from the explicit
 * `--legacy-app` target; any `Stache.app` bundle is the measured identity.
 */
export function classifyExecutable(
  executablePath: string,
  next: Target,
  legacy: Target | null,
): TargetKind | null {
  if (matchesTargetExecutable(executablePath, next)) {
    return 'next';
  }

  if (legacy !== null && matchesTargetExecutable(executablePath, legacy)) {
    return 'legacy';
  }

  if (/\/Stache( Next)?\.app\/Contents\/MacOS\//.test(executablePath)) {
    return 'next';
  }

  return null;
}

/** Why an instance query did not prove exclusive ownership. */
export type InstanceRefusal = 'foreign-instance' | 'unknown-identity' | 'query-failed';

/** A refusal to treat the owned PID as the only running next instance. */
export class InstanceCollisionError extends Error {
  readonly reason: InstanceRefusal;
  readonly evidence: RunningStacheProcess[];

  constructor(reason: InstanceRefusal, message: string, evidence: RunningStacheProcess[] = []) {
    super(message);
    this.name = 'InstanceCollisionError';
    this.reason = reason;
    this.evidence = evidence;
  }
}

/**
 * Requires the only running instance of the measured identity
 * ({@link NEXT_BUNDLE_IDENTIFIER}) to be the owned PID.
 *
 * The query is injected so tests never touch a real process table. Entries
 * classified `legacy` (only from an explicit `--legacy-app` path) never count
 * as a collision, but they also never satisfy ownership. A foreign or unidentified
 * next instance, a missing owned instance, and a failed query all refuse by
 * throwing {@link InstanceCollisionError}; nothing here signals or launches a
 * process.
 */
export function assertNoOtherNextInstance(
  query: () => RunningStacheProcess[],
  ownedPid: number,
): RunningStacheProcess[] {
  let evidence: RunningStacheProcess[];

  try {
    evidence = query();
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);

    throw new InstanceCollisionError('query-failed', `the instance query failed: ${detail}`);
  }

  const nextInstances = evidence.filter((entry) => entry.kind === 'next');
  const unidentified = nextInstances.find((entry) => entry.pid === null);

  if (unidentified !== undefined) {
    throw new InstanceCollisionError(
      'unknown-identity',
      `a next instance has no verifiable PID: ${unidentified.description}`,
      evidence,
    );
  }

  const foreign = nextInstances.find((entry) => entry.pid !== ownedPid);

  if (foreign !== undefined) {
    throw new InstanceCollisionError(
      'foreign-instance',
      `another next instance is running: pid ${foreign.pid} (${foreign.description})`,
      evidence,
    );
  }

  if (!nextInstances.some((entry) => entry.pid === ownedPid)) {
    throw new InstanceCollisionError(
      'unknown-identity',
      `the owned pid ${ownedPid} is not a reported next instance`,
      evidence,
    );
  }

  return evidence;
}

function matchesTargetExecutable(executablePath: string, target: Target): boolean {
  if (executablePath === target.executablePath) {
    return true;
  }

  return (
    target.bundlePath !== null && executablePath.startsWith(`${target.bundlePath}/Contents/MacOS/`)
  );
}

export type RecordSignal = (pid: number, signal: NodeJS.Signals, reason: string) => void;

export interface ShutdownOutcome {
  lingering: ProcessRow[];
}

/**
 * Stops one measured process tree.
 *
 * Only the spawned root is signalled first. If recorded descendants outlive it,
 * they are signalled by PID after re-reading the process table and confirming
 * that the PID still runs the recorded executable; `killall` and name matching
 * are never used.
 */
export async function stopOwnedProcessTree(
  child: ChildProcess,
  recordSignal: RecordSignal,
): Promise<ShutdownOutcome> {
  const rootPid = child.pid;

  if (rootPid === undefined) {
    return { lingering: [] };
  }

  const tracked = readProcessTree(rootPid);

  if (tracked.length === 0) {
    return { lingering: [] };
  }

  if (isProcessAlive(rootPid)) {
    child.kill('SIGTERM');
    recordSignal(rootPid, 'SIGTERM', 'stop measured process tree');
    await waitForExit(child, SHUTDOWN_TIMEOUT_MS);
  }

  if (isProcessAlive(rootPid)) {
    child.kill('SIGKILL');
    recordSignal(rootPid, 'SIGKILL', 'root did not exit after SIGTERM');
    await sleep(1_000);
  }

  const descendants = tracked.filter((row) => row.pid !== rootPid);

  await stopDescendants(descendants, recordSignal);
  const lingering = aliveMatching(descendants);
  const rootRow = tracked.find((row) => row.pid === rootPid);

  if (rootRow !== undefined && isProcessAlive(rootPid)) {
    lingering.push(rootRow);
  }

  return { lingering };
}

async function stopDescendants(tracked: ProcessRow[], recordSignal: RecordSignal): Promise<void> {
  if (tracked.length === 0) {
    return;
  }

  const alive = aliveMatching(tracked);

  for (const row of alive) {
    try {
      process.kill(row.pid, 'SIGTERM');
      recordSignal(row.pid, 'SIGTERM', 'descendant outlived the measured root');
    } catch {
      // The process exited between the check and the signal.
    }
  }

  await sleep(2_000);

  for (const row of aliveMatching(alive)) {
    try {
      process.kill(row.pid, 'SIGKILL');
      recordSignal(row.pid, 'SIGKILL', 'descendant did not exit after SIGTERM');
    } catch {
      // The process exited between the check and the signal.
    }
  }

  const deadline = Date.now() + 5_000;

  while (Date.now() < deadline && aliveMatching(alive).length > 0) {
    await sleep(100);
  }
}

function aliveMatching(rows: ProcessRow[]): ProcessRow[] {
  const current = new Map(listProcesses().map((row) => [row.pid, row]));

  return rows.filter((row) => current.get(row.pid)?.executable === row.executable);
}

/** Waits until a spawned child exits or the timeout elapses. */
export async function waitForExit(child: ChildProcess, timeoutMs: number): Promise<boolean> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return true;
  }

  return await new Promise<boolean>((resolve) => {
    const timer = setTimeout(() => {
      child.off('exit', onExit);
      resolve(false);
    }, timeoutMs);
    const onExit = (): void => {
      clearTimeout(timer);
      resolve(true);
    };

    child.once('exit', onExit);

    if (child.exitCode !== null || child.signalCode !== null) {
      onExit();
    }
  });
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);

    return true;
  } catch {
    return false;
  }
}
