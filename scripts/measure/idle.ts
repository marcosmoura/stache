import { setTimeout as sleep } from 'node:timers/promises';

import { throwIfInterrupted } from './interrupt';
import { IDLE_SAMPLE_INTERVAL_MS } from './paths';
import { listProcesses, readProcessTree } from './process';
import type { IdleProcess, IdleSample, ObservedProcess, ProcessRow } from './types';

interface InternalSnapshot {
  takenAtMs: number;
  takenAt: string;
  processes: ProcessRow[];
}

interface Appearance {
  snapshotIndex: number;
  takenAtMs: number;
  row: ProcessRow;
}

const WEBKIT_PROCESS = /WebKit\.(WebContent|Networking|GPU)|com\.apple\.WebKit/;

/**
 * Samples cumulative CPU time and RSS for one host and its descendants.
 *
 * CPU percentages are averages over the interval in which each process was
 * observed, derived from `ps` cumulative CPU time rather than its decaying
 * instantaneous `%cpu` column. RSS values are resident sizes and double-count
 * shared pages across processes.
 *
 * `isInterrupted` aborts the remaining window so a signal does not keep sampling
 * a process the signal handler already killed.
 */
export async function sampleIdle(
  rootPid: number,
  durationSeconds: number,
  isInterrupted: () => boolean = () => false,
): Promise<IdleSample> {
  const snapshots = await takeSnapshots(rootPid, durationSeconds, isInterrupted);
  const first = snapshots[0];
  const last = snapshots[snapshots.length - 1];
  if (first === undefined || last === undefined) {
    throw new Error('idle sampling produced no process snapshots');
  }

  const appearances = new Map<number, Appearance[]>();
  for (const [snapshotIndex, snapshot] of snapshots.entries()) {
    for (const row of snapshot.processes) {
      const entries = appearances.get(row.pid) ?? [];
      entries.push({ snapshotIndex, takenAtMs: snapshot.takenAtMs, row });
      appearances.set(row.pid, entries);
    }
  }

  const processes: IdleProcess[] = [];
  for (const [pid, entries] of appearances) {
    processes.push(buildIdleProcess(pid, rootPid, entries, snapshots.length));
  }
  processes.sort((left, right) => left.pid - right.pid);

  const host = processes.find((process) => process.pid === rootPid);
  if (host === undefined) {
    throw new Error(`host pid ${rootPid} disappeared during idle sampling`);
  }

  const windowSeconds = Math.max((last.takenAtMs - first.takenAtMs) / 1_000, 0.001);
  const treeCpuPercent = processes.reduce((total, process) => total + process.cpuPercent, 0);
  const treeRssFinalKiB = last.processes.reduce((total, row) => total + row.rssKiB, 0);
  const treeRssPeakKiB = Math.max(
    ...snapshots.map((snapshot) =>
      snapshot.processes.reduce((total, row) => total + row.rssKiB, 0),
    ),
  );
  const notes: string[] = [];
  const joinedLate = processes.filter(
    (process) => process.role === 'descendant' && !process.presentInAllSnapshots,
  );
  if (joinedLate.length > 0) {
    notes.push(
      `${joinedLate.length} descendant(s) were absent from at least one snapshot; their CPU ` +
        'average uses only the interval in which they were observed.',
    );
  }
  notes.push('RSS is the sum of resident sizes; shared pages are counted once per process.');

  return {
    durationSeconds,
    sampleIntervalMs: IDLE_SAMPLE_INTERVAL_MS,
    windowSeconds,
    host,
    processes,
    treeCpuPercent,
    treeRssFinalKiB,
    treeRssPeakKiB,
    unattributableWebKit: observedWebKitOutsideTree(rootPid),
    treeSnapshots: snapshots.map((snapshot) => ({
      takenAt: snapshot.takenAt,
      processes: snapshot.processes,
    })),
    notes,
  };
}

async function takeSnapshots(
  rootPid: number,
  durationSeconds: number,
  isInterrupted: () => boolean,
): Promise<InternalSnapshot[]> {
  const snapshots: InternalSnapshot[] = [];
  const startedAt = Date.now();
  const durationMs = durationSeconds * 1_000;

  const take = (): void => {
    throwIfInterrupted(isInterrupted, 'interrupted during idle sampling');
    const takenAtMs = Date.now();
    const processes = readProcessTree(rootPid);
    if (processes.length === 0) {
      throw new Error(`host pid ${rootPid} exited during idle sampling`);
    }
    snapshots.push({ takenAtMs, takenAt: new Date(takenAtMs).toISOString(), processes });
  };

  take();
  while (Date.now() - startedAt < durationMs) {
    const remaining = durationMs - (Date.now() - startedAt);
    if (remaining <= 0) {
      break;
    }
    await sleep(Math.min(IDLE_SAMPLE_INTERVAL_MS, remaining));
    take();
  }
  const elapsed = Date.now() - startedAt;
  if (elapsed < durationMs) {
    await sleep(durationMs - elapsed);
    take();
  }
  return snapshots;
}

function buildIdleProcess(
  pid: number,
  rootPid: number,
  entries: Appearance[],
  snapshotCount: number,
): IdleProcess {
  const first = entries[0];
  const last = entries[entries.length - 1];
  const observedSeconds = Math.max((last.takenAtMs - first.takenAtMs) / 1_000, 0.001);
  const cpuDelta = Math.max(last.row.cpuTimeSeconds - first.row.cpuTimeSeconds, 0);
  return {
    pid,
    ppid: last.row.ppid,
    executable: last.row.executable,
    role: pid === rootPid ? 'host' : 'descendant',
    cpuPercent: (cpuDelta / observedSeconds) * 100,
    rssFinalKiB: last.row.rssKiB,
    rssPeakKiB: Math.max(...entries.map((entry) => entry.row.rssKiB)),
    presentInAllSnapshots: entries.length === snapshotCount,
    exitedDuringSampling: last.snapshotIndex < snapshotCount - 1,
  };
}

function observedWebKitOutsideTree(rootPid: number): ObservedProcess[] {
  const treePids = new Set(readProcessTree(rootPid).map((row) => row.pid));
  return listProcesses()
    .filter((row) => !treePids.has(row.pid) && WEBKIT_PROCESS.test(row.executable))
    .map((row) => ({
      pid: row.pid,
      ppid: row.ppid,
      executable: row.executable,
      rssKiB: row.rssKiB,
    }));
}
