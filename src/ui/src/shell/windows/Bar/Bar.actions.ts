import type { BarTilingTarget } from '../../../sdk/bindings.gen';
import {
  runBarAction,
  runBarTilingAction,
  runSessionAction,
  type BarAction,
} from '../../../sdk/client';

import type { KeepAwakeSession } from './Bar.data';

/**
 * The click handler of a Bar component. The host maps the enum value to its
 * fixed target; the outcome only feeds diagnostics (no toast, badge or other
 * UI) and a click that did not complete is never retried here.
 */
export function barClick(action: BarAction): () => void {
  return () => {
    runBarAction(action)
      .then((outcome) => {
        if (outcome.kind !== 'completed') {
          console.debug(`bar action ${action}: ${outcome.kind}`);
        }
      })
      .catch((error: unknown) => {
        console.debug(`bar action ${action} failed`, error);
      });
  };
}

/**
 * The Keep Awake click: a generic host session Pause/Resume of `keepAwake`.
 * The action follows the observed `paused` flag of `BarView.keepAwake` (never
 * the colour); the host rechecks the current state at execution and answers
 * `stale` rather than inverting a newer decision, `disabled` when Keep Awake is
 * off (inert: it never enables or edits configuration). The outcome only feeds
 * diagnostics: the indicator changes solely through the next `BarChanged`.
 * Unresolved (`undefined`) sends nothing.
 */
export function keepAwakeClick(session: KeepAwakeSession | undefined): () => void {
  return () => {
    if (!session) {
      return;
    }

    const action = session.paused ? 'resume' : 'pause';

    runSessionAction('keepAwake', action)
      .then((outcome) => {
        if (outcome.kind !== 'applied') {
          console.debug(`keep awake ${action}: ${outcome.kind}`);
        }
      })
      .catch((error: unknown) => {
        console.debug(`keep awake ${action} failed`, error);
      });
  };
}

/**
 * A Tiling strip click: focuses a workspace or window through
 * `bar_tiling_action`, echoing the generation and sequence of the strip view that
 * was rendered so Tiling can refuse a stale target. The outcome only feeds
 * diagnostics; the strip changes solely through the next `BarChanged`. A strip
 * that is not `available` (`undefined`) sends nothing.
 */
export function tilingClick(
  rendered: { generation: string; sequence: string } | undefined,
  action: BarTilingTarget,
): void {
  if (!rendered) {
    return;
  }

  runBarTilingAction({ generation: rendered.generation, sequence: rendered.sequence, action })
    .then((outcome) => {
      if (outcome.kind !== 'applied') {
        console.debug(`tiling ${action.kind}: ${outcome.kind}`);
      }
    })
    .catch((error: unknown) => {
      console.debug(`tiling ${action.kind} failed`, error);
    });
}
