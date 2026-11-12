import type { BarSnapshotOutcome, StripApp, TilingStripView } from '@/ipc';

/** The Tiling strip while the Bar is available, otherwise `null` (nothing drawn). */
export function selectTiling(outcome: BarSnapshotOutcome): TilingStripView | null {
  return outcome.kind === 'available' ? (outcome.view.tiling ?? null) : null;
}

/** The longest window title shown, wide and compact. */
export const TITLE_MAX = 40;
export const TITLE_MAX_COMPACT = 25;

/** `text`, cut to `max` characters plus `...` when longer. */
export function truncate(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, max)}...`;
}

/** One listed app: the window key, the app name and its label. */
export type ListedApp = { key: string; appName: string; label: string };

/**
 * The apps the strip lists. In compact mode only the focused app (when there is
 * one); otherwise every app in Tiling's order. The label is the app name; an
 * app with more than one listed window shows its non-empty title, truncated to
 * 40 characters (25 in compact mode) plus `...`.
 */
export function listApps(
  apps: readonly StripApp[],
  focusedApp: StripApp | null | undefined,
  compact: boolean,
): ListedApp[] {
  const listed = compact && focusedApp ? [focusedApp] : apps;
  const counts = new Map<string, number>();

  for (const { appName } of listed) {
    counts.set(appName, (counts.get(appName) ?? 0) + 1);
  }

  return listed.map(({ key, appName, title }) => ({
    key,
    appName,
    label:
      (counts.get(appName) ?? 0) > 1 && title
        ? truncate(title, compact ? TITLE_MAX_COMPACT : TITLE_MAX)
        : appName,
  }));
}

/** The label reveal's duration in seconds: 0.15 s, longer for labels over 15 characters. */
export function revealDuration(label: string, base: number): number {
  return Math.max(base, (label.length * base) / 15);
}

/**
 * What the strip draws: nothing, the loading item, or the workspace names (in
 * Tiling's published order, which is its configuration's order) and the listed
 * app keys.
 */
export type StripShape =
  | { kind: 'loading' }
  | { kind: 'available'; workspaces: string[]; apps: string[] };

/**
 * The strip's shape: `null` for no Tiling, `absent` and zero workspaces.
 * Only names and keys, so a focus or label change does not change it.
 */
export function stripShape(outcome: BarSnapshotOutcome, compact: boolean): StripShape | null {
  const tiling = selectTiling(outcome);

  if (tiling?.kind === 'loading') {
    return { kind: 'loading' };
  }

  if (tiling?.kind !== 'available' || tiling.workspaces.length === 0) {
    return null;
  }

  return {
    kind: 'available',
    workspaces: tiling.workspaces.map(({ name }) => name),
    apps: listApps(tiling.apps, tiling.focusedApp, compact).map(({ key }) => key),
  };
}

/** Whether `name` is the focused workspace of an available strip. */
export function isFocusedWorkspace(outcome: BarSnapshotOutcome, name: string): boolean {
  const tiling = selectTiling(outcome);

  return tiling?.kind === 'available' && tiling.focusedWorkspace === name;
}

/** One app item: its listing and whether it is the focused window. */
export type AppView = ListedApp & { focused: boolean };

/** The app item for window `key`, or `null` when the strip no longer lists it. */
export function appView(
  outcome: BarSnapshotOutcome,
  key: string,
  compact: boolean,
): AppView | null {
  const tiling = selectTiling(outcome);

  if (tiling?.kind !== 'available') {
    return null;
  }

  const listed = listApps(tiling.apps, tiling.focusedApp, compact).find((app) => app.key === key);

  return listed ? { ...listed, focused: tiling.focusedApp?.key === key } : null;
}

/** The generation and sequence of an available strip, which a click echoes back. */
export function renderedStrip(
  outcome: BarSnapshotOutcome | undefined,
): { generation: string; sequence: string } | null {
  const tiling = outcome ? selectTiling(outcome) : null;

  return tiling?.kind === 'available'
    ? { generation: tiling.generation, sequence: tiling.sequence }
    : null;
}
