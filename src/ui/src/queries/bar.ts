import { queryOptions, skipToken } from '@tanstack/react-query';

import { commands, events, type BarSnapshotOutcome, type BarView } from '@/ipc';

import { createFeed } from './feed';

/**
 * The whole typed `bar_snapshot` outcome, written only by {@link barFeed}.
 * Read slices with `useQuery({ ...barSnapshotOptions(), select })`.
 */
export function barSnapshotOptions() {
  return queryOptions<BarSnapshotOutcome>({
    queryKey: ['bar', 'snapshot'],
    queryFn: skipToken,
  });
}

/** The sequence an outcome carries; `unauthorized` has none. */
export function barOutcomeSequence(outcome: BarSnapshotOutcome): string | null {
  switch (outcome.kind) {
    case 'available':
      return outcome.view.sequence;
    case 'unavailable':
      return outcome.sequence;
    default:
      return null;
  }
}

/**
 * `bar_snapshot` + `bar-changed`, ordered by `sequence` and honoring
 * `resnapshot`. `unauthorized` (no sequence) is kept only while nothing
 * sequenced is held.
 */
export const barFeed = createFeed({
  options: barSnapshotOptions(),
  snapshot: (): Promise<BarSnapshotOutcome> => commands.barSnapshot(),
  event: events.barChanged,
  read: (payload) => ({
    value: payload.outcome ?? null,
    sequence: payload.sequence,
    resnapshot: payload.resnapshot,
  }),
  sequenceOf: barOutcomeSequence,
});

/** The outcome kind. */
export function selectBarKind(outcome: BarSnapshotOutcome) {
  return outcome.kind;
}

/** The view while the Bar is available, otherwise `null`. */
export function selectBarView(outcome: BarSnapshotOutcome): BarView | null {
  return outcome.kind === 'available' ? outcome.view : null;
}

/** What the Bar window lays out with, while the Bar is available. */
export type BarFrameLayout = Pick<BarView, 'layout' | 'height'>;

/**
 * The layout and height while the Bar is available, otherwise `null`. `padding`
 * is not selected: the host already insets the panel frame by it.
 */
export function selectBarLayout(outcome: BarSnapshotOutcome): BarFrameLayout | null {
  if (outcome.kind !== 'available') {
    return null;
  }

  const { layout, height } = outcome.view;

  return { layout, height };
}

/**
 * Whether the Bar slides away: only an `available` view whose system menu bar
 * is `visible`. Every other outcome and menu state keeps the Bar shown.
 */
export function selectBarAway(outcome: BarSnapshotOutcome): boolean {
  return outcome.kind === 'available' && outcome.view.menu.state === 'visible';
}
