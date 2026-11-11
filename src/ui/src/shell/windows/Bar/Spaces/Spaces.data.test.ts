import { describe, expect, test } from 'vitest';

import { tilingStrip } from '../../../../../tests/barFixtures';

import { getSortedWorkspaces, spacesFromTiling } from './Spaces.data';

describe('Spaces data', () => {
  test('sorts in the legacy order, then unknown names alphabetically', () => {
    const names = [
      'zeta',
      'tasks',
      'files',
      'Alpha',
      'misc',
      'guitar',
      'communication',
      'design',
      'beta',
      'music',
      'browser',
      'coding',
      'terminal',
    ];

    expect(getSortedWorkspaces(names.map((name) => ({ name }))).map(({ name }) => name)).toEqual([
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
      'Alpha',
      'beta',
      'zeta',
    ]);
  });

  test('keeps null, absent and loading as they are', () => {
    expect(spacesFromTiling(null)).toBeNull();
    expect(spacesFromTiling({ kind: 'absent' })).toEqual({ kind: 'absent' });
    expect(spacesFromTiling({ kind: 'loading' })).toEqual({ kind: 'loading' });
  });

  test('maps an available view onto legacy-shaped data keyed by the window key', () => {
    expect(
      spacesFromTiling(
        tilingStrip({
          apps: [
            { key: '9', appName: 'Code', title: 'a.ts' },
            { key: '4', appName: 'Code', title: 'b.ts' },
          ],
          focusedApp: null,
          focusedWorkspace: null,
        }),
      ),
    ).toEqual({
      kind: 'available',
      generation: '3',
      sequence: '7',
      workspaces: [
        { name: 'terminal', displayName: 'Terminal' },
        { name: 'coding', displayName: 'Coding' },
      ],
      focusedWorkspace: null,
      apps: [
        { appName: 'Code', windowId: '9', windowTitle: 'a.ts' },
        { appName: 'Code', windowId: '4', windowTitle: 'b.ts' },
      ],
      focusedApp: null,
    });
  });
});
