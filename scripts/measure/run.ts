import {
  assertPilotIsInert,
  createTempRoot,
  removeTempRoot,
  writeLegacyConfig,
  writeNextConfigs,
} from './config';
import { type ScenarioConfig } from './config';
import { sampleIdle } from './idle';
import { InterruptedError, sleepUnlessInterrupted, throwIfInterrupted } from './interrupt';
import { controlSocketPath, startTarget, waitForReady, type StartedProcess } from './launch';
import { IDLE_WARMUP_SECONDS, REPOSITORY_ROOT } from './paths';
import {
  findRunningStacheProcesses,
  stopOwnedProcessTree,
  type RecordSignal,
  type RunningStacheProcess,
  type ShutdownOutcome,
} from './process';
import { evaluateTargets, renderReport, writeMeasurementOutputs, type OutputPaths } from './report';
import { collectEnvironment, readAudioState, restoreDefaultDevice } from './system';
import { resolveTarget } from './targets';
import type {
  AudioState,
  EnvironmentSnapshot,
  MeasurementRun,
  MeasureOptions,
  SafetyRecord,
  ScenarioResult,
  Target,
} from './types';

export { InterruptedError };

export interface RunOutcome {
  outputs: OutputPaths;
  /** True when every temporary root was removed and the audio end state was verified. */
  clean: boolean;
  /** First error that stopped the run; null when every scenario completed. */
  failure: Error | null;
  interrupted: boolean;
}

interface Session {
  /** A started child whose process tree has not been confirmed stopped. */
  active: StartedProcess | null;
  /** Temporary root that child runs under. */
  activeRoot: string | null;
  /** Root kept because its measured process tree outlived the stop attempts. */
  lingeringRoot: string | null;
}

interface ScenarioDefinition {
  id: string;
  label: string;
  target: Target;
  config: ScenarioConfig;
  root: string;
  startAudio: AudioState;
}

interface ScenarioPlan {
  id: string;
  label: string;
  target: Target;
  enabled: boolean | null;
}

interface GuardTargets {
  next: Target;
  legacy: Target | null;
}

interface ScenarioContext {
  options: MeasureOptions;
  safety: SafetyRecord;
  session: Session;
  recordSignal: RecordSignal;
  guardTargets: GuardTargets;
  isInterrupted: () => boolean;
}

/**
 * Runs every requested scenario sequentially and writes the report.
 *
 * A guard runs before the first launch and again before every later launch.
 * Success, a measurement failure and SIGINT all follow the same cleanup path:
 * every owned tree is stopped, temporary roots are removed unless a tree
 * survived, and default audio devices are checked and restored. The report is
 * written even when the run stops early, and records the failure plus the
 * partial measurements and cleanup evidence gathered so far.
 *
 * `outputRoot` receives the timestamped measurement directory; it exists so an
 * offline check can write results outside the repository.
 */
export async function runMeasurement(
  options: MeasureOptions,
  outputRoot: string = REPOSITORY_ROOT,
): Promise<RunOutcome> {
  const startedWall = Date.now();
  const startedAt = new Date(startedWall).toISOString();
  const nextTarget = resolveTarget('next', options.newAppPath);
  const legacyTarget =
    options.legacyAppPath === null ? null : resolveTarget('legacy', options.legacyAppPath);
  if (legacyTarget !== null && legacyTarget.executablePath === nextTarget.executablePath) {
    throw new Error('--legacy-app must not point at the Stache Next executable');
  }

  const environment = collectEnvironment();
  const safety: SafetyRecord = {
    guardChecks: [],
    tempRoots: [],
    tempRootsRemoved: false,
    audioDefaultsAtStart: {
      input: environment.audio.defaultInput,
      output: environment.audio.defaultOutput,
    },
    audioDefaultsAtEnd: null,
    audioRestore: null,
    signals: [],
    notes: [],
  };
  const recordSignal: RecordSignal = (pid, signal, reason) => {
    safety.signals.push({ at: new Date().toISOString(), pid, signal, reason });
  };

  const session: Session = { active: null, activeRoot: null, lingeringRoot: null };
  let interrupted = false;
  let repeatedSignalNoted = false;
  const onSignal = (signal: 'SIGINT' | 'SIGTERM'): void => {
    if (interrupted) {
      // Cleanup must not be abandoned by a second signal.
      if (!repeatedSignalNoted) {
        repeatedSignalNoted = true;
        safety.notes.push(`received ${signal} again; cleanup is already in progress`);
      }
      return;
    }
    interrupted = true;
    safety.notes.push(`received ${signal}; stopping the active measurement`);
    if (session.active?.child.kill('SIGTERM')) {
      const pid = session.active.child.pid;
      if (pid !== undefined) {
        recordSignal(pid, 'SIGTERM', `stop active measurement after ${signal}`);
      }
    }
  };
  const onSigint = (): void => onSignal('SIGINT');
  const onSigterm = (): void => onSignal('SIGTERM');
  process.on('SIGINT', onSigint);
  process.on('SIGTERM', onSigterm);

  const scenarios: ScenarioResult[] = [];
  let audioAtEnd: AudioState | null = environment.audio;
  let audioChanged = false;
  let audioObserved = true;
  let failure: Error | null = null;

  try {
    assertNoOtherStache(nextTarget, legacyTarget, safety);
    for (const plan of buildScenarioPlans(nextTarget, legacyTarget)) {
      throwIfInterrupted(() => interrupted, 'interrupted before the next scenario');
      const definition = prepareScenario(plan, environment, safety);
      console.log(`measure: scenario ${definition.id} (${definition.root})`);
      const result = createScenarioResult(definition, options);
      scenarios.push(result);
      await measureScenario(definition, result, {
        options,
        safety,
        session,
        recordSignal,
        guardTargets: { next: nextTarget, legacy: legacyTarget },
        isInterrupted: () => interrupted,
      });
    }
  } catch (error) {
    failure = error instanceof Error ? error : new Error(String(error));
  } finally {
    try {
      let lingering = false;
      try {
        lingering = await settleActiveProcess(session, recordSignal, safety);
      } catch (error) {
        lingering = true;
        safety.notes.push(
          'could not stop the active process tree: ' +
            (error instanceof Error ? error.message : String(error)),
        );
      }
      if (lingering && session.lingeringRoot === null) {
        session.lingeringRoot = session.activeRoot;
      }
      cleanupTemporaryRoots(safety, session.lingeringRoot);
      try {
        ({ audioAtEnd, audioChanged, audioObserved } = reconcileAudio(environment.audio, safety));
      } catch (error) {
        audioAtEnd = null;
        audioChanged = false;
        audioObserved = false;
        safety.audioDefaultsAtEnd = null;
        safety.notes.push(
          'audio reconciliation failed: ' +
            (error instanceof Error ? error.message : String(error)),
        );
      }
    } finally {
      process.off('SIGINT', onSigint);
      process.off('SIGTERM', onSigterm);
    }
  }

  if (interrupted && failure === null) {
    failure = new InterruptedError('interrupted during cleanup');
  }
  const interruptedBySignal = failure instanceof InterruptedError;
  const run: MeasurementRun = {
    startedAt,
    finishedAt: new Date().toISOString(),
    durationMs: Date.now() - startedWall,
    status: failure === null ? 'completed' : interruptedBySignal ? 'interrupted' : 'failed',
    failure: failure?.message ?? null,
    commandLine: [...process.argv],
    targets: { next: nextTarget, legacy: legacyTarget },
    environment,
    audioAtEnd,
    scenarios,
    safety,
    limitations: describeLimitations(legacyTarget),
  };
  const checks = evaluateTargets(run);
  const report = renderReport(run, checks);
  const outputs = writeMeasurementOutputs(run, report, outputRoot);
  const clean = safety.tempRootsRemoved && audioObserved && !audioChanged;
  return { outputs, clean, failure, interrupted: interruptedBySignal };
}

function buildScenarioPlans(nextTarget: Target, legacyTarget: Target | null): ScenarioPlan[] {
  const plans: ScenarioPlan[] = [
    {
      id: 'next-disabled',
      label: 'Stache Next - proxyAudio disabled',
      target: nextTarget,
      enabled: false,
    },
    {
      id: 'next-enabled',
      label: 'Stache Next - proxyAudio enabled with inert priorities',
      target: nextTarget,
      enabled: true,
    },
  ];
  if (legacyTarget !== null) {
    plans.push({
      id: 'legacy',
      label: 'Legacy Stache baseline',
      target: legacyTarget,
      enabled: null,
    });
  }
  return plans;
}

function prepareScenario(
  plan: ScenarioPlan,
  environment: EnvironmentSnapshot,
  safety: SafetyRecord,
): ScenarioDefinition {
  const root = createTempRoot();
  safety.tempRoots.push(root);
  const startAudio = readAudioState();
  if (plan.enabled === null) {
    return {
      ...plan,
      config: writeLegacyConfig(root),
      root,
      startAudio,
    };
  }
  if (plan.enabled) {
    assertPilotIsInert(startAudio, environment.displays);
  }
  return {
    ...plan,
    config: writeNextConfigs(root, plan.enabled, startAudio),
    root,
    startAudio,
  };
}

function createScenarioResult(
  definition: ScenarioDefinition,
  options: MeasureOptions,
): ScenarioResult {
  return {
    id: definition.id,
    label: definition.label,
    target: definition.target,
    configFiles: definition.config.files,
    launches: [],
    idle: null,
    complete: false,
    annotations: [
      `${options.launches} timed launches; idle sample uses the last launch after ` +
        `${IDLE_WARMUP_SECONDS} s warmup.`,
      ...(definition.target.kind === 'legacy'
        ? [
            'Legacy config disables every configurable tool; its tray, bar and widget services ' +
              'remain baseline overhead.',
            'Legacy webviews are observed from the process tree; its own count is not exposed.',
          ]
        : ['No window is requested; the host aborts setup if any webview exists.']),
    ],
  };
}

async function measureScenario(
  definition: ScenarioDefinition,
  result: ScenarioResult,
  context: ScenarioContext,
): Promise<void> {
  const { options, safety, session, recordSignal, guardTargets, isInterrupted } = context;

  for (let index = 0; index < options.launches; index += 1) {
    throwIfInterrupted(isInterrupted, `${definition.id}: interrupted before launch ${index + 1}`);
    assertNoOtherStache(guardTargets.next, guardTargets.legacy, safety);
    const started = startTarget(definition.target, definition.config, definition.root);
    session.active = started;
    session.activeRoot = definition.root;

    let failure: Error | null = null;
    let cleanupProblem: Error | null = null;
    try {
      const ready = await waitForReady(
        started,
        definition.target,
        controlSocketPath(definition.root),
        isInterrupted,
      );
      console.log(
        `measure: ${definition.id} launch ${index + 1}/${options.launches} ready in ` +
          `${Math.round(ready.externalReadyMs)} ms external, ` +
          `${started.internalTrayReadyMs() ?? '?'} ms internal`,
      );
      result.launches.push({
        index,
        cold: index === 0,
        externalReadyMs: ready.externalReadyMs,
        internalTrayReadyMs: started.internalTrayReadyMs(),
        instrumentedWebviews: started.instrumentedWebviews(),
        readySource: ready.readySource,
        stderrTail: started.stderrTail(),
      });

      if (index === options.launches - 1) {
        await sleepUnlessInterrupted(IDLE_WARMUP_SECONDS * 1_000, isInterrupted);
        assertDefaultAudioUnchanged(definition.startAudio, definition.id);
        console.log(
          `measure: ${definition.id} sampling idle for ${options.idleSeconds} s after ` +
            `${IDLE_WARMUP_SECONDS} s warmup`,
        );
        result.idle = await sampleIdle(requirePid(started), options.idleSeconds, isInterrupted);
      }
    } catch (error) {
      failure = error instanceof Error ? error : new Error(String(error));
    } finally {
      cleanupProblem = await stopLaunch(definition, started, session, recordSignal);
    }
    if (cleanupProblem !== null) {
      if (failure === null) {
        throw cleanupProblem;
      }
      // The measurement error stays primary; the cleanup issue is evidence too.
      safety.notes.push(
        `${definition.id}: cleanup after the failure also reported: ${cleanupProblem.message}`,
      );
    }
    if (failure !== null) {
      throw failure;
    }
  }

  throwIfInterrupted(isInterrupted, `${definition.id}: interrupted after the last launch`);
  result.complete = true;
}

/**
 * Stops one owned tree and records whether its temporary root must be kept.
 *
 * The session keeps ownership until the tree is confirmed gone. A root is only
 * released for removal when the whole tree exited; otherwise the outer cleanup
 * keeps it, because deleting a live process's HOME is worse than a failed run.
 */
async function stopLaunch(
  definition: ScenarioDefinition,
  started: StartedProcess,
  session: Session,
  recordSignal: RecordSignal,
): Promise<Error | null> {
  let problem: Error | null = null;
  let outcome: ShutdownOutcome | null = null;
  try {
    outcome = await stopOwnedProcessTree(started.child, recordSignal);
  } catch (error) {
    problem = error instanceof Error ? error : new Error(String(error));
  }

  const lingering = outcome?.lingering ?? [];
  if (problem !== null) {
    // The stop outcome is unknown: keep the child tracked for one retry at the
    // outer cleanup, and keep the root because its tree may still be alive.
    session.lingeringRoot = definition.root;
    return problem;
  }
  if (lingering.length > 0) {
    // The root is gone but descendants survived. Never retry: a second
    // derivation from a dead root would miss them and free their HOME.
    session.active = null;
    session.activeRoot = null;
    session.lingeringRoot = definition.root;
    return new Error(
      `${definition.id} process tree did not exit: ` +
        lingering.map((row) => `${row.pid} ${row.executable}`).join(', '),
    );
  }

  session.active = null;
  session.activeRoot = null;
  try {
    assertDefaultAudioUnchanged(definition.startAudio, definition.id);
  } catch (error) {
    return error instanceof Error ? error : new Error(String(error));
  }
  return null;
}

function requirePid(started: StartedProcess): number {
  if (started.child.pid === undefined) {
    throw new Error('spawned process has no pid');
  }
  return started.child.pid;
}

function assertNoOtherStache(next: Target, legacy: Target | null, safety: SafetyRecord): void {
  let running: RunningStacheProcess[];
  try {
    running = findRunningStacheProcesses(next, legacy);
  } catch (error) {
    throw new Error(
      'Refusing to start: could not verify that no other Stache instance is running: ' +
        (error instanceof Error ? error.message : String(error)),
    );
  }

  const otherNext = running.filter((process) => process.kind === 'next');
  const otherLegacy = running.filter((process) => process.kind === 'legacy');
  const blocking = legacy === null ? otherNext : [...otherNext, ...otherLegacy];
  if (blocking.length > 0) {
    throw new Error(describeRunningProcesses(blocking));
  }

  const scope =
    legacy === null
      ? 'Stache Next process (legacy Stache is not guarded without --legacy-app)'
      : 'Stache Next or legacy Stache process';
  safety.guardChecks.push(`no other ${scope} at ${new Date().toISOString()}`);

  if (legacy === null && otherLegacy.length > 0 && safety.guardChecks.length === 1) {
    const observed = otherLegacy
      .map((process) => `pid ${process.pid ?? '?'} ${process.description}`)
      .join(', ');
    safety.notes.push(
      `A legacy Stache was running and was not measured (${observed}); it does not block a ` +
        'run without --legacy-app, and its proxyAudio must stay disabled so only one ' +
        'device-changing watcher runs.',
    );
  }
}

function describeRunningProcesses(running: RunningStacheProcess[]): string {
  const details = running
    .map((process) => `  ${process.kind} pid ${process.pid ?? '?'}: ${process.description}`)
    .join('\n');
  return [
    'Refusing to start: another Stache process is running.',
    details,
    'Quit it from its own tray or menu before measuring. This tool never signals',
    'another Stache instance, including /Applications/Stache.app.',
  ].join('\n');
}

function assertDefaultAudioUnchanged(expected: AudioState, scenarioId: string): void {
  const current = readAudioState();
  if (current.defaultInput !== expected.defaultInput) {
    throw new Error(
      `${scenarioId} changed the default input from "${expected.defaultInput ?? '(none)'}" ` +
        `to "${current.defaultInput ?? '(none)'}"; restoring before continuing`,
    );
  }
  if (current.defaultOutput !== expected.defaultOutput) {
    throw new Error(
      `${scenarioId} changed the default output from "${expected.defaultOutput ?? '(none)'}" ` +
        `to "${current.defaultOutput ?? '(none)'}"; restoring before continuing`,
    );
  }
}

/**
 * One last stop attempt for a child that the per-launch cleanup could not
 * confirm stopped.
 *
 * Returns true when a tree may still be alive, in which case its root is kept.
 */
async function settleActiveProcess(
  session: Session,
  recordSignal: RecordSignal,
  safety: SafetyRecord,
): Promise<boolean> {
  if (session.active === null) {
    return session.lingeringRoot !== null;
  }
  const started = session.active;
  session.active = null;
  if (started.child.exitCode !== null || started.child.signalCode !== null) {
    // The root already exited, so a fresh derivation cannot see descendants
    // that outlived it. Keep the root rather than assume the tree is gone.
    safety.notes.push(
      'the measured root exited without a confirmed process-tree stop; its temporary root was kept',
    );
    return true;
  }
  let outcome: ShutdownOutcome;
  try {
    outcome = await stopOwnedProcessTree(started.child, recordSignal);
  } catch (error) {
    safety.notes.push(
      'could not confirm the measured process tree stopped: ' +
        (error instanceof Error ? error.message : String(error)),
    );
    return true;
  }
  if (outcome.lingering.length > 0) {
    safety.notes.push(
      'A measured process tree did not exit; its temporary root was kept for inspection: ' +
        outcome.lingering.map((row) => `${row.pid} ${row.executable}`).join(', '),
    );
    return true;
  }
  session.lingeringRoot = null;
  return false;
}

/**
 * Removes every recorded temporary root except `keepRoot`, whose process tree
 * was not confirmed stopped.
 */
export function cleanupTemporaryRoots(safety: SafetyRecord, keepRoot: string | null): void {
  let removed = 0;
  for (const root of safety.tempRoots) {
    if (root === keepRoot) {
      safety.notes.push(
        `kept temporary root ${root} because its measured process tree was not confirmed stopped`,
      );
      continue;
    }
    try {
      removeTempRoot(root);
      removed += 1;
    } catch (error) {
      safety.notes.push(
        `could not remove temporary root ${root}: ` +
          (error instanceof Error ? error.message : String(error)),
      );
    }
  }
  safety.tempRootsRemoved = keepRoot === null && removed === safety.tempRoots.length;
}

function reconcileAudio(
  start: AudioState,
  safety: SafetyRecord,
): { audioAtEnd: AudioState | null; audioChanged: boolean; audioObserved: boolean } {
  let audioAtEnd: AudioState;
  try {
    audioAtEnd = readAudioState();
  } catch (error) {
    safety.notes.push(
      'could not re-read the default audio devices after the run; the end state is unverified: ' +
        (error instanceof Error ? error.message : String(error)),
    );
    safety.audioRestore = {
      attempted: false,
      succeeded: false,
      detail: 'the default devices could not be re-read, so no restore was attempted',
    };
    return { audioAtEnd: null, audioChanged: false, audioObserved: false };
  }
  safety.audioDefaultsAtEnd = {
    input: audioAtEnd.defaultInput,
    output: audioAtEnd.defaultOutput,
  };

  const outputChanged = audioAtEnd.defaultOutput !== start.defaultOutput;
  const inputChanged = audioAtEnd.defaultInput !== start.defaultInput;
  if (!inputChanged && !outputChanged) {
    return { audioAtEnd, audioChanged: false, audioObserved: true };
  }

  const details: string[] = [];
  let succeeded = true;
  if (outputChanged && start.defaultOutput !== null) {
    const result = restoreDefaultDevice('output', start.defaultOutput);
    succeeded &&= result.succeeded;
    details.push(result.detail);
  }
  if (inputChanged && start.defaultInput !== null) {
    const result = restoreDefaultDevice('input', start.defaultInput);
    succeeded &&= result.succeeded;
    details.push(result.detail);
  }
  const attempted = details.length > 0;
  safety.audioRestore = {
    attempted,
    succeeded: attempted && succeeded,
    detail: attempted
      ? details.join('; ')
      : 'no previous default device was recorded for the changed direction',
  };
  safety.notes.push('Default audio devices changed during the run; see the restore result.');

  try {
    audioAtEnd = readAudioState();
    safety.audioDefaultsAtEnd = {
      input: audioAtEnd.defaultInput,
      output: audioAtEnd.defaultOutput,
    };
    const stillChanged =
      audioAtEnd.defaultOutput !== start.defaultOutput ||
      audioAtEnd.defaultInput !== start.defaultInput;
    if (safety.audioRestore !== null) {
      safety.audioRestore.succeeded &&= !stillChanged;
      if (stillChanged) {
        safety.audioRestore.detail += '; devices still differ after the restore attempt';
      }
    }
  } catch {
    // The earlier end state stays in the report.
  }
  return { audioAtEnd, audioChanged: true, audioObserved: true };
}

function describeLimitations(legacyTarget: Target | null): string[] {
  const limitations = [
    'OS page cache is not flushed: "cold" means the first launch in a scenario with an ' +
      'empty isolated app cache, not necessarily the first launch after the build.',
    'Process-tree attribution follows parent PIDs. Helpers reparented to launchd are listed ' +
      'as observed WebKit processes outside the tree and excluded from tree totals.',
    'Legacy has no tray-created acknowledgement; its readiness is the setup-complete log line ' +
      'emitted after tray, bar and widget creation.',
    'RSS sums resident sizes per process, so shared pages are counted more than once.',
    '`ps` cumulative CPU time has 10 ms resolution; helper processes that start and exit ' +
      'inside one sampling interval may not be captured.',
    'The measured machine is shared: unrelated system activity can add CPU noise.',
    'Two measurement tool instances are not mutually excluded; run one at a time.',
    'Process identity is matched by Launch Services bundle identifier, the measured executable ' +
      'path, and the Stache Next/Stache .app paths. A bare release binary at a different path is ' +
      'not detected by the guard.',
  ];
  if (legacyTarget === null) {
    limitations.push(
      'A running legacy Stache is recorded in the safety notes but does not block a run ' +
        'without --legacy-app; keep its proxyAudio disabled while measuring.',
    );
  } else {
    limitations.push(
      'The legacy cache socket path under the isolated HOME exceeds the 103-byte sun_path ' +
        'limit, so the legacy IPC server cannot bind during measurement. Readiness uses its ' +
        'own setup log line and the baseline lacks that idle accept thread.',
    );
  }
  return limitations;
}
