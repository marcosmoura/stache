import type {
  BarChanged_Serialize,
  BarLayoutItem,
  BarLayoutView,
  BarWidgetKind,
  BarSnapshotOutcome_Serialize,
  BarView_Serialize,
  HostStatus_Serialize,
  TilingStripView_Serialize,
} from '@/ipc';
import type { WidgetOptions } from '@/windows/Bar/widgets';

import { USER_WORKSPACES } from './workspaces';

export { USER_WORKSPACES } from './workspaces';

/** A ready, ungated host with no modules. */
export function hostStatusFixture(
  overrides: Partial<HostStatus_Serialize> = {},
): HostStatus_Serialize {
  return {
    ready: true,
    error: null,
    access: { state: 'open', revoked: false, appPath: null },
    modules: [],
    config: [],
    ...overrides,
  };
}

/** The options the host publishes for an entry that sets none. */
export const defaultWidgetOptions: { [K in BarWidgetKind]: WidgetOptions<K> } = {
  spaces: {},
  media: {},
  weather: {},
  cpu: { temperature_alert: 85 },
  battery: {},
  keep_awake: {},
  wifi: {},
  clock: { show_date: true, show_seconds: true, hour_cycle: 'h23' },
};

/** A published layout item with default options, overridden by `options`. */
export function layoutItem<K extends BarWidgetKind>(
  id: string,
  widget: K,
  options: Partial<WidgetOptions<K>> = {},
): BarLayoutItem {
  const resolved: WidgetOptions = { ...defaultWidgetOptions[widget], ...options };

  // A widget's defaults merged with its own partial options: still its options.
  return { id, widget, options: resolved } as BarLayoutItem;
}

/** The default `bar.toml` layout, normalized as the host publishes it. */
export const defaultLayoutFixture: BarLayoutView = {
  start: [layoutItem('spaces', 'spaces')],
  center: [layoutItem('media', 'media')],
  end: [
    layoutItem('weather', 'weather'),
    layoutItem('cpu', 'cpu'),
    layoutItem('battery', 'battery'),
    layoutItem('keep_awake', 'keep_awake'),
    layoutItem('wifi', 'wifi'),
    layoutItem('clock', 'clock'),
  ],
};

/** A running Bar view at `sequence`. */
export function barViewFixture(
  sequence: string,
  overrides: Partial<BarView_Serialize> = {},
): BarView_Serialize {
  return {
    generation: '1',
    sequence,
    configRevision: '1',
    panel: 'loaded',
    menu: { state: 'hidden', reason: null },
    frame: null,
    height: 32,
    padding: 8,
    layout: defaultLayoutFixture,
    detail: null,
    cpu: null,
    battery: null,
    wifi: null,
    media: null,
    weatherEpoch: '0',
    keepAwake: null,
    tiling: null,
    ...overrides,
  };
}

export function barAvailable(
  sequence: string,
  overrides: Partial<BarView_Serialize> = {},
): BarSnapshotOutcome_Serialize {
  return { kind: 'available', view: barViewFixture(sequence, overrides) };
}

export function barUnavailable(sequence: string): BarSnapshotOutcome_Serialize {
  return { kind: 'unavailable', reason: 'stopped', detail: null, generation: '1', sequence };
}

export const barUnauthorized: BarSnapshotOutcome_Serialize = { kind: 'unauthorized' };

/** A `bar-changed` payload carrying `outcome` (or a resnapshot request). */
export function barChanged(
  sequence: string,
  outcome: BarSnapshotOutcome_Serialize | null,
  resnapshot = false,
): BarChanged_Serialize {
  return { generation: '1', sequence, resnapshot, outcome };
}

/** An available Tiling strip: the user's workspaces (terminal focused) and Ghostty focused. */
export function tilingStrip(
  overrides: Partial<Extract<TilingStripView_Serialize, { kind: 'available' }>> = {},
): TilingStripView_Serialize {
  return {
    kind: 'available',
    generation: '3',
    sequence: '7',
    workspaces: USER_WORKSPACES.map((name) => ({
      name,
      focused: name === 'terminal',
      inactive: false,
    })),
    apps: [{ key: '100', appName: 'Ghostty', title: 'zsh' }],
    focusedApp: { key: '100', appName: 'Ghostty', title: 'zsh' },
    focusedWorkspace: 'terminal',
    refusedFocus: null,
    ...overrides,
  };
}
