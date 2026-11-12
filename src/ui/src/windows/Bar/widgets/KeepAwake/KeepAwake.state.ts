import type { BarSnapshotOutcome, KeepAwakeIndicator, SessionAction } from '@/ipc';

/** The Keep Awake indicator while the Bar is available, otherwise `null` (before the first observation too). */
export function selectKeepAwake(outcome: BarSnapshotOutcome): KeepAwakeIndicator | null {
  return outcome.kind === 'available' ? (outcome.view.keepAwake ?? null) : null;
}

/** The session action a click sends: implied by the rendered `paused` flag, never the color. */
export function keepAwakeAction({ paused }: KeepAwakeIndicator): SessionAction {
  return paused ? 'resume' : 'pause';
}

/** The accessible name, stating the current state. */
export function keepAwakeLabel({ active }: KeepAwakeIndicator): string {
  return active ? 'Keep Awake: on' : 'Keep Awake: off';
}
