import type {
  AccessView,
  HostStatus,
  ModuleEnableOutcome,
  ModuleState,
  ModuleStatusView,
  SessionAction,
  SessionActionOutcome,
  SessionUnavailable,
} from '@/ipc';

/** Readable module names; an unlisted id is shown capitalised. */
const MODULE_NAMES: Readonly<Record<string, string>> = {
  commandQuit: 'Hold to Quit',
  keepAwake: 'Keep Awake',
  menuAnywhere: 'Menu Anywhere',
  notunes: 'noTunes',
  proxyAudio: 'Proxy Audio',
};

export function moduleName(id: string): string {
  return MODULE_NAMES[id] ?? `${id.charAt(0).toUpperCase()}${id.slice(1)}`;
}

/** The former tray's state labels, unchanged. */
export const STATE_LABELS: Readonly<Record<ModuleState, string>> = {
  stopped: 'stopped',
  disabled: 'disabled',
  running: 'running',
  paused: 'paused',
  waitingForPermission: 'waiting for permission',
  unavailable: 'unavailable',
  failed: 'failed',
};

/** The tone of a module's status dot: green running, orange paused or waiting, red failed. */
export type StateTone = 'green' | 'orange' | 'red' | 'gray';

export const STATE_TONES: Readonly<Record<ModuleState, StateTone>> = {
  stopped: 'gray',
  disabled: 'gray',
  running: 'green',
  paused: 'orange',
  waitingForPermission: 'orange',
  unavailable: 'red',
  failed: 'red',
};

/** What a module row offers, by the former tray's `rows` rules. */
export type ModuleActions = {
  /** Pause or Resume, present only for an enabled module. */
  session: SessionAction | null;
  /** Retry: every failed module, and an enabled, unpaused unavailable one. */
  retry: boolean;
};

export function moduleActions(module: ModuleStatusView): ModuleActions {
  return {
    session: module.enabled ? (module.paused ? 'resume' : 'pause') : null,
    retry:
      module.state === 'failed' ||
      (module.state === 'unavailable' && module.enabled && !module.paused),
  };
}

/** The header line: `starting…` before readiness, `checking Accessibility…` while checking. */
export function headerLine(status: HostStatus): string | null {
  if (!status.ready) {
    return 'starting…';
  }

  return status.access.state === 'checking' ? 'checking Accessibility…' : null;
}

/** The gate banner's title while blocked, or `null` when no banner shows. */
export function bannerTitle(access: AccessView): string | null {
  if (access.state !== 'blocked') {
    return null;
  }

  return access.revoked ? 'Accessibility was turned off' : 'Accessibility required';
}

const UNAVAILABLE_TEXT: Readonly<Record<SessionUnavailable, string>> = {
  failed: 'the module failed',
  unavailable: 'a prerequisite is missing',
  waitingForPermission: 'waiting for permission',
  releasePending: 'a release is pending',
  shuttingDown: 'Stache is quitting',
  notRunning: 'the host is not running',
};

/**
 * The inline text for a session outcome, or `null` when nothing is shown:
 * `applied` and `stale` (the host moved on; the next status redraws the row).
 */
export function sessionOutcomeText(
  action: SessionAction,
  outcome: SessionActionOutcome,
): string | null {
  const verb = action === 'pause' ? 'pause' : 'resume';

  switch (outcome.kind) {
    case 'applied':
    case 'stale':
      return null;
    case 'notApplied':
      return `Could not ${verb}: ${outcome.reason === 'refused' ? 'the module refused' : 'not reflected'}`;
    case 'disabled':
      return `Could not ${verb}: the module is disabled`;
    case 'unavailable':
      return `Could not ${verb}: ${UNAVAILABLE_TEXT[outcome.reason]}`;
    case 'busy':
      return `Could not ${verb}: another action is in progress`;
    case 'uncertain':
      return `The host did not confirm the ${verb}`;
    case 'unauthorized':
      return `Could not ${verb}: not allowed`;
  }
}

/**
 * The inline text for an enable outcome, or `null` when nothing is shown
 * (`applied`, `unchanged`): the host's own message for a refusal, so two
 * identical refusals each settle and show it.
 */
export function enableOutcomeText(outcome: ModuleEnableOutcome): string | null {
  switch (outcome.kind) {
    case 'applied':
    case 'unchanged':
      return null;
    case 'refused':
    case 'unavailable':
      return outcome.message;
  }
}

/** The module's configuration file (and settings tab) name. */
export const moduleFile = (id: string) => `${id}.toml`;

/**
 * A control waiting for the host. Retry returns at once, so it waits for the
 * next status; Enable and Pause/Resume wait for their typed outcome.
 * `baseline` is the status the click was made on.
 */
export type Pending =
  | { kind: 'enable'; target: boolean }
  | { kind: 'retry'; baseline: HostStatus }
  | { kind: 'session'; action: SessionAction };

export type PendingMap = Readonly<Record<string, Pending>>;
export type ErrorMap = Readonly<Record<string, string>>;

/** A host error that appeared or changed since `baseline`, if any. */
function newError(status: HostStatus, baseline: HostStatus): string | null {
  return status.error != null && status.error !== baseline.error ? status.error : null;
}

/**
 * Settles pending Retry controls against a new status: on the first newer
 * status, with the new host error inline if there is one. Enable and session
 * actions are settled only by their outcome.
 */
export function reconcile(
  pending: PendingMap,
  errors: ErrorMap,
  status: HostStatus,
): { pending: PendingMap; errors: ErrorMap } {
  const nextPending: Record<string, Pending> = {};
  const nextErrors: Record<string, string> = { ...errors };
  let changed = false;

  for (const [id, entry] of Object.entries(pending)) {
    if (entry.kind !== 'retry' || status === entry.baseline) {
      nextPending[id] = entry;
      continue;
    }

    const error = newError(status, entry.baseline);

    changed = true;

    if (error !== null) {
      nextErrors[id] = error;
    }
  }

  return changed ? { pending: nextPending, errors: nextErrors } : { pending, errors };
}
