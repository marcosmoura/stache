import type { AccessState, HostWindow, TrialVerdict } from './types';

/** The access state a `core.status` response line reports, or 'unreported'. */
export function parseCoreStatusAccess(line: string): AccessState | 'unreported' | null {
  let parsed: unknown;

  try {
    parsed = JSON.parse(line);
  } catch {
    return null;
  }

  const response = asRecord(parsed);

  if (response === null || response.status !== 'ok') {
    return null;
  }

  const data = asRecord(response.data);
  const access = data === null ? null : asRecord(data.access);

  if (access === null) {
    return 'unreported';
  }

  const state = access.state;

  return state === 'open' || state === 'blocked' || state === 'checking' ? state : 'unreported';
}

/** Parses the JSON array printed by `list-windows.swift`; malformed input throws. */
export function parseHostWindows(output: string): HostWindow[] {
  const parsed: unknown = JSON.parse(output);

  if (!Array.isArray(parsed)) {
    throw new Error('the window list is not a JSON array');
  }

  return parsed.map((entry): HostWindow => {
    const record = asRecord(entry);

    if (record === null) {
      throw new Error('a window entry is not an object');
    }

    return {
      layer: numberField(record, 'layer'),
      x: numberField(record, 'x'),
      y: numberField(record, 'y'),
      width: numberField(record, 'width'),
      height: numberField(record, 'height'),
      onScreen: record.onScreen === true,
    };
  });
}

/** Whether a window is the Bar panel: the configured height and a real width. */
export function isBarPanel(window: HostWindow, height: number): boolean {
  return window.width > 0 && Math.abs(window.height - height) <= 1;
}

/** The Accessibility gate must be open; anything else means no module (Bar) runs. */
export function judgeAccess(
  access: AccessState | 'unreported' | null,
  appPath: string,
): TrialVerdict {
  if (access === 'blocked' || access === 'checking') {
    return {
      valid: false,
      reason: 'access-blocked',
      message:
        `the Accessibility launch gate is ${access}: every module, Bar included, is held. ` +
        `Grant Accessibility to this exact .app path, then rerun: ${appPath}`,
    };
  }

  if (access !== 'open') {
    return {
      valid: false,
      reason: 'access-unreported',
      message:
        'core.status did not report the Accessibility gate, so a started Bar cannot be ' +
        `proven. Use a current release build and grant Accessibility to: ${appPath}`,
    };
  }

  return { valid: true };
}

export interface TrialEvidence {
  arm: 'off' | 'on';
  appPath: string;
  height: number;
  access: AccessState | 'unreported' | null;
  windows: HostWindow[];
}

/**
 * Decides whether a launch measured what it claims to.
 *
 * Both arms need the Accessibility gate open: a gated host holds every module
 * and shows a guidance window, so neither arm would be the Bar cost. The "on"
 * arm also needs a window of the configured Bar height owned by the host; the
 * "off" arm must own no window at all. An invalid launch is never a sample.
 */
export function judgeTrial(evidence: TrialEvidence): TrialVerdict {
  const { arm, appPath, height, access, windows } = evidence;

  const gate = judgeAccess(access, appPath);

  if (!gate.valid) {
    return gate;
  }

  if (arm === 'on') {
    return windows.some((window) => isBarPanel(window, height))
      ? { valid: true }
      : {
          valid: false,
          reason: 'bar-panel-missing',
          message:
            `the host owns no window of the Bar height (${height}); Bar did not start. ` +
            `Confirm Accessibility is granted to ${appPath} and a primary display exists.`,
        };
  }

  return windows.length === 0
    ? { valid: true }
    : {
        valid: false,
        reason: 'unexpected-window',
        message: `the Bar-off host owns ${windows.length} window(s); it is not a clean baseline`,
      };
}

function numberField(record: Record<string, unknown>, key: string): number {
  const value = record[key];

  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new Error(`window field ${key} is not a finite number`);
  }

  return value;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}
