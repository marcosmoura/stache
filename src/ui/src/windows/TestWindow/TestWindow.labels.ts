import type { ConfigFileState, ModuleState } from '@/ipc';

export const MODULE_STATE_LABELS: Readonly<Record<ModuleState, string>> = {
  stopped: 'Stopped',
  disabled: 'Disabled',
  running: 'Running',
  paused: 'Paused',
  waitingForPermission: 'Waiting for permission',
  unavailable: 'Unavailable',
  failed: 'Failed',
};

export const CONFIG_STATE_LABELS: Readonly<Record<ConfigFileState, string>> = {
  missing: 'Missing',
  valid: 'Valid',
  invalid: 'Invalid',
};

/** Every state a badge shows. */
export type BadgeState = ModuleState | ConfigFileState;

/** The tone of a state badge. */
export type BadgeTone = 'neutral' | 'success' | 'danger' | 'warning' | 'attention';

export function badgeTone(state: BadgeState): BadgeTone {
  switch (state) {
    case 'running':
      return 'success';
    case 'failed':
    case 'invalid':
      return 'danger';
    case 'waitingForPermission':
    case 'missing':
      return 'warning';
    case 'paused':
    case 'unavailable':
      return 'attention';
    default:
      return 'neutral';
  }
}

/** A yes/no flag as text. */
export const yesNo = (value: boolean) => (value ? 'Yes' : 'No');
