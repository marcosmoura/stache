import type { TilingStripView } from '../../../../sdk/bindings.gen';

import type { SpacesStrip, Workspaces } from './Spaces.types';

const workspaceOrder = [
  'terminal',
  'coding',
  'browser',
  'music',
  'design',
  'communication',
  'guitar',
  'misc',
  'files',
  'tasks',
];

/** Legacy's order: the known names first, then unknown names alphabetically. */
export function getSortedWorkspaces<T extends { name: string }>(workspaces: T[]): T[] {
  return [...workspaces].sort((a, b) => {
    const indexA = workspaceOrder.indexOf(a.name);
    const indexB = workspaceOrder.indexOf(b.name);

    if (indexA === -1 && indexB === -1) {
      return a.name.localeCompare(b.name);
    }

    if (indexA === -1) {
      return 1;
    }

    if (indexB === -1) {
      return -1;
    }

    return indexA - indexB;
  });
}

/**
 * Projections by view identity. The Bar feed writes through React Query's
 * structural sharing, so an unchanged `BarView.tiling` keeps its identity across
 * unrelated `BarChanged` events (CPU, battery, clock) and the strip below it
 * receives the same objects and does not re-render.
 */
const projections = new WeakMap<TilingStripView, SpacesStrip>();

/** Projects `BarView.tiling` onto the legacy-shaped strip data (`null` stays unresolved). */
export function spacesFromTiling(tiling: TilingStripView | null | undefined): SpacesStrip | null {
  if (!tiling) {
    return null;
  }

  const cached = projections.get(tiling);

  if (cached) {
    return cached;
  }

  const projected = projectTiling(tiling);

  projections.set(tiling, projected);

  return projected;
}

function projectTiling(tiling: TilingStripView): SpacesStrip {
  if (tiling.kind !== 'available') {
    return { kind: tiling.kind };
  }

  const workspaces: Workspaces = getSortedWorkspaces(tiling.workspaces).map(({ name }) => ({
    name,
    displayName: name.charAt(0).toUpperCase() + name.slice(1),
  }));

  return {
    kind: 'available',
    generation: tiling.generation,
    sequence: tiling.sequence,
    workspaces,
    focusedWorkspace: tiling.focusedWorkspace ?? null,
    apps: tiling.apps.map(({ appName, key, title }) => ({
      appName,
      windowId: key,
      windowTitle: title,
    })),
    focusedApp: tiling.focusedApp
      ? {
          appName: tiling.focusedApp.appName,
          windowId: tiling.focusedApp.key,
          windowTitle: tiling.focusedApp.title,
        }
      : null,
  };
}
