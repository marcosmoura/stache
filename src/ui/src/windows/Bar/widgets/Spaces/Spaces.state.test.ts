import { describe, expect, it } from 'vitest';

import type { StripApp, TilingStripView_Serialize } from '@/ipc';

import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';

import {
  appView,
  isFocusedWorkspace,
  listApps,
  renderedStrip,
  revealDuration,
  selectTiling,
  stripShape,
  truncate,
} from './Spaces.state';

const app = (key: string, appName: string, title = ''): StripApp => ({ key, appName, title });

const strip = (
  overrides: Partial<Extract<TilingStripView_Serialize, { kind: 'available' }>> = {},
): TilingStripView_Serialize => ({
  kind: 'available',
  generation: '3',
  sequence: '7',
  workspaces: [
    { name: 'coding', focused: false },
    { name: 'terminal', focused: true },
  ],
  apps: [app('1', 'Ghostty', 'zsh'), app('2', 'Code', 'a.ts')],
  focusedApp: app('1', 'Ghostty', 'zsh'),
  focusedWorkspace: 'terminal',
  ...overrides,
});

describe('Spaces state', () => {
  it('selects the strip only while the Bar is available', () => {
    expect(selectTiling(barUnavailable('1'))).toBeNull();
    expect(selectTiling(barAvailable('1'))).toBeNull();
    expect(selectTiling(barAvailable('1', { tiling: { kind: 'absent' } }))).toEqual({
      kind: 'absent',
    });
  });

  it('keeps the workspaces in the published order, unknown names included', () => {
    const names = ['zeta', 'tasks', 'files', 'Alpha', 'terminal', 'media', 'coding'];
    const tiling = strip({ workspaces: names.map((name) => ({ name, focused: false })) });

    expect(stripShape(barAvailable('1', { tiling }), false)).toMatchObject({ workspaces: names });
  });

  it('truncates past the limit only', () => {
    expect(truncate('x'.repeat(40), 40)).toBe('x'.repeat(40));
    expect(truncate('x'.repeat(41), 40)).toBe(`${'x'.repeat(40)}...`);
  });

  it('labels single windows by app name and multiple windows by title', () => {
    const long = 'An extremely long window title that keeps on going forever.ts';
    const apps = [
      app('1', 'Code', long),
      app('2', 'Code', 'ünïcødé — 文件.ts'),
      app('3', 'Code', ''),
      app('4', 'Safari', 'Google'),
    ];

    expect(listApps(apps, null, false).map(({ label }) => label)).toEqual([
      `${long.slice(0, 40)}...`,
      'ünïcødé — 文件.ts',
      'Code',
      'Safari',
    ]);
    expect(listApps(apps, null, true).map(({ label }) => label)).toEqual([
      `${long.slice(0, 25)}...`,
      'ünïcødé — 文件.ts',
      'Code',
      'Safari',
    ]);
  });

  it('lists only the focused app in compact mode, labelled by its app name', () => {
    const apps = [app('1', 'Code', 'a.ts'), app('2', 'Code', 'b.ts'), app('3', 'Zed', 'z')];

    expect(listApps(apps, apps[1], true)).toEqual([{ key: '2', appName: 'Code', label: 'Code' }]);
    expect(listApps(apps, apps[1], false).map(({ key }) => key)).toEqual(['1', '2', '3']);
    // Without a focused app compact mode keeps the list.
    expect(listApps(apps, null, true).map(({ key }) => key)).toEqual(['1', '2', '3']);
  });

  it('shapes each state', () => {
    const of = (tiling: TilingStripView_Serialize | null) =>
      stripShape(barAvailable('1', { tiling }), false);

    expect(of(null)).toBeNull();
    expect(of({ kind: 'absent' })).toBeNull();
    expect(of(strip({ workspaces: [] }))).toBeNull();
    expect(of({ kind: 'loading' })).toEqual({ kind: 'loading' });
    expect(of(strip())).toEqual({
      kind: 'available',
      workspaces: ['coding', 'terminal'],
      apps: ['1', '2'],
    });
    expect(stripShape(barUnavailable('1'), false)).toBeNull();
  });

  it('reads one item: focus, label and the rendered counters', () => {
    const outcome = barAvailable('1', { tiling: strip() });

    expect(isFocusedWorkspace(outcome, 'terminal')).toBe(true);
    expect(isFocusedWorkspace(outcome, 'coding')).toBe(false);
    expect(appView(outcome, '1', false)).toEqual({
      key: '1',
      appName: 'Ghostty',
      label: 'Ghostty',
      focused: true,
    });
    expect(appView(outcome, '2', true)).toBeNull();
    expect(appView(barAvailable('1', { tiling: { kind: 'loading' } }), '1', false)).toBeNull();
    expect(renderedStrip(outcome)).toEqual({ generation: '3', sequence: '7' });
    expect(renderedStrip(barAvailable('1', { tiling: { kind: 'loading' } }))).toBeNull();
    expect(renderedStrip(undefined)).toBeNull();
  });

  it('scales the reveal with the label length', () => {
    expect(revealDuration('Code', 0.15)).toBe(0.15);
    expect(revealDuration('x'.repeat(30), 0.15)).toBeCloseTo(0.3);
  });
});
