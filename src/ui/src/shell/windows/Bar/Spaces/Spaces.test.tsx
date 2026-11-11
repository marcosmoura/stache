import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';
import { page } from 'vitest/browser';

import { barData, tilingStrip } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';
import type { TilingStripView_Serialize } from '../../../../sdk/bindings.gen';
import { tilingClick } from '../Bar.actions';
import { BarDataProvider } from '../BarDataProvider';

import { Spaces, SpacesFallback } from './Spaces';
import { spacesFromTiling } from './Spaces.data';
import { useSpaces } from './Spaces.state';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue({ kind: 'applied' });
});

afterEach(async () => {
  await page.viewport(414, 896);
});

function dataFor(tiling: TilingStripView_Serialize | null) {
  return barData({ spaces: spacesFromTiling(tiling) });
}

function SpacesProbe() {
  const { apps, workspaces, focusedApp, focusedWorkspace, isEnabled, isLoading } = useSpaces();

  return (
    <pre>
      {JSON.stringify({ apps, workspaces, focusedApp, focusedWorkspace, isEnabled, isLoading })}
    </pre>
  );
}

async function probe(tiling: TilingStripView_Serialize | null) {
  const { screen } = await renderWithBarData(<SpacesProbe />, dataFor(tiling));

  return JSON.parse(screen.container.textContent ?? '') as ReturnType<typeof useSpaces>;
}

describe('Spaces rendering per state', () => {
  test.each([
    ['unresolved (undefined)', barData()],
    ['null', dataFor(null)],
    ['absent', dataFor({ kind: 'absent' })],
    ['zero workspaces', dataFor(tilingStrip({ workspaces: [] }))],
  ])('%s keeps the empty first-grid-slot div', async (_label, data) => {
    const { screen } = await renderWithBarData(<Spaces />, data);

    expect(screen.container.innerHTML).toBe('<div></div>');
  });

  test('loading renders the legacy Loading... fallback, DOM-identical to SpacesFallback', async () => {
    const { screen } = await renderWithBarData(<Spaces />, dataFor({ kind: 'loading' }));

    await expect.element(screen.getByText('Loading...')).toBeVisible();

    const { screen: fallback } = await renderWithBarData(<SpacesFallback />);

    await vi.waitFor(() => {
      expect(screen.container.innerHTML).toBe(fallback.container.innerHTML);
    });
  });

  test('the Loading... fallback keeps its inline-flex surface', async () => {
    const { screen } = await renderWithBarData(<SpacesFallback />);
    const surface = screen.container.firstElementChild?.firstElementChild;

    await expect.element(screen.getByText('Loading...')).toBeVisible();

    if (!surface) {
      throw new Error('fallback missing');
    }

    const style = getComputedStyle(surface);

    // `inline-flex` is blockified because the Stack grid makes the surface a grid item.
    expect(style.display).toBe('flex');
    expect(style.alignItems).toBe('center');
    expect(style.paddingLeft).toBe('10px');
    expect(style.paddingRight).toBe('10px');
  });

  test('available renders the workspace list and the app list in the sorted order', async () => {
    const { screen } = await renderWithBarData(<Spaces />, dataFor(tilingStrip()));
    const container = screen.getByTestId('spaces-container');

    await expect.element(container).toBeVisible();

    const [workspaces, ...apps] = Array.from(container.element().children);

    expect(workspaces?.querySelectorAll('button')).toHaveLength(2);
    expect(apps).toHaveLength(1);
    expect(screen.container.textContent).toBe('Ghostty');
    // terminal is sorted before coding and carries the indicator.
    expect(workspaces?.querySelector('button')?.querySelector('div')).not.toBeNull();
    expect(workspaces?.querySelectorAll('button')[1]?.querySelector('div')).toBeNull();
  });

  test('no focused workspace and no focused app draw no indicator and no label', async () => {
    const { screen } = await renderWithBarData(
      <Spaces />,
      dataFor(tilingStrip({ focusedWorkspace: null, focusedApp: null })),
    );

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();
    expect(screen.container.querySelectorAll('button div')).toHaveLength(0);
    expect(screen.container.textContent).toBe('');
  });
});

describe('useSpaces display names', () => {
  const longTitle = 'An extremely long window title that keeps on going forever.ts';
  const duplicates = tilingStrip({
    apps: [
      { key: '1', appName: 'Code', title: longTitle },
      { key: '2', appName: 'Code', title: 'ünïcødé — 文件.ts' },
      { key: '3', appName: 'Code', title: '' },
      { key: '4', appName: 'Safari', title: 'Google' },
    ],
    focusedApp: null,
  });

  test.each([
    [2035, true],
    [2036, true],
    [2037, false],
  ])('viewport %i: laptop=%s', async (width, laptop) => {
    await page.viewport(width, 900);

    const { apps } = await probe(duplicates);

    expect(apps.map(({ displayName }) => displayName)).toEqual([
      laptop ? `${longTitle.slice(0, 25)}...` : `${longTitle.slice(0, 40)}...`,
      'ünïcødé — 文件.ts',
      // An empty title falls back to the app name.
      'Code',
      'Safari',
    ]);
    expect(apps.map(({ windowId }) => windowId)).toEqual(['1', '2', '3', '4']);
  });

  test('a title of exactly 40 characters is not truncated on a wide screen', async () => {
    await page.viewport(2560, 900);
    const title = 'x'.repeat(40);
    const { apps } = await probe(
      tilingStrip({
        apps: [
          { key: '1', appName: 'Code', title },
          { key: '2', appName: 'Code', title: `${title}y` },
        ],
      }),
    );

    expect(apps.map(({ displayName }) => displayName)).toEqual([title, `${title}...`]);
  });

  test.each([
    [2036, ['2']],
    [2037, ['1', '2', '3']],
  ])('viewport %i reduces the list to the focused app only on laptop', async (width, keys) => {
    await page.viewport(width, 900);

    const { apps } = await probe(
      tilingStrip({
        apps: [
          { key: '1', appName: 'Code', title: 'a.ts' },
          { key: '2', appName: 'Code', title: 'b.ts' },
          { key: '3', appName: 'Zed', title: 'z' },
        ],
        focusedApp: { key: '2', appName: 'Code', title: 'b.ts' },
      }),
    );

    expect(apps.map(({ windowId }) => windowId)).toEqual(keys);
    // On laptop the single focused app is no longer a duplicate: its app name shows.
    expect(apps.find(({ windowId }) => windowId === '2')?.displayName).toBe(
      width <= 2036 ? 'Code' : 'b.ts',
    );
  });

  test('flags: available, loading and the focus fields', async () => {
    expect(await probe(tilingStrip())).toMatchObject({
      isEnabled: true,
      isLoading: false,
      focusedWorkspace: 'terminal',
      focusedApp: { appName: 'Ghostty', windowId: '100', windowTitle: 'Ghostty' },
      workspaces: [
        { name: 'terminal', displayName: 'Terminal' },
        { name: 'coding', displayName: 'Coding' },
      ],
    });
    expect(await probe({ kind: 'loading' })).toMatchObject({ isEnabled: false, isLoading: true });
    expect(await probe({ kind: 'absent' })).toMatchObject({ isEnabled: false, isLoading: false });
  });
});

describe('Spaces clicks', () => {
  test('a workspace click sends the rendered generation, sequence and name', async () => {
    const { screen } = await renderWithBarData(
      <Spaces />,
      dataFor(tilingStrip({ generation: '12', sequence: '18446744073709551615' })),
    );

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();
    screen.container.querySelectorAll<HTMLButtonElement>('button')[1]?.click();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('bar_tiling_action', {
      request: {
        generation: '12',
        sequence: '18446744073709551615',
        action: { kind: 'focusWorkspace', name: 'coding' },
      },
    });
  });

  test('an app click sends the rendered window key', async () => {
    await page.viewport(2560, 900);
    const { screen } = await renderWithBarData(
      <Spaces />,
      dataFor(
        tilingStrip({
          apps: [
            { key: '100', appName: 'Ghostty', title: 'Ghostty' },
            { key: '205', appName: 'Code', title: 'x' },
          ],
        }),
      ),
    );

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();
    screen.container.querySelectorAll<HTMLButtonElement>('button')[3]?.click();

    expect(invoke).toHaveBeenCalledWith('bar_tiling_action', {
      request: { generation: '3', sequence: '7', action: { kind: 'focusWindow', window: '205' } },
    });
  });

  test('after a newer strip is rendered, the next click carries its generation and sequence', async () => {
    const screen = await render(
      <BarDataProvider data={dataFor(tilingStrip())}>
        <Spaces />
      </BarDataProvider>,
    );

    await expect.element(screen.getByTestId('spaces-container')).toBeVisible();
    await screen.rerender(
      <BarDataProvider data={dataFor(tilingStrip({ generation: '4', sequence: '1' }))}>
        <Spaces />
      </BarDataProvider>,
    );
    screen.container.querySelector<HTMLButtonElement>('button')?.click();

    expect(invoke).toHaveBeenCalledWith('bar_tiling_action', {
      request: {
        generation: '4',
        sequence: '1',
        action: { kind: 'focusWorkspace', name: 'terminal' },
      },
    });
  });

  test.each([
    ['loading', { kind: 'loading' } as const],
    ['absent', { kind: 'absent' } as const],
    ['null', null],
  ])('while %s the handlers send nothing', async (_label, tiling) => {
    function Clicker() {
      const { onSpaceClick, onAppClick } = useSpaces();

      return (
        <button
          type="button"
          onClick={() => {
            onSpaceClick('terminal');
            onAppClick('100');
          }}
        >
          click
        </button>
      );
    }

    const { screen } = await renderWithBarData(<Clicker />, dataFor(tiling));

    await screen.getByRole('button').click();
    expect(invoke).not.toHaveBeenCalled();
  });

  test('tilingClick without a rendered strip sends nothing', () => {
    tilingClick(undefined, { kind: 'focusWorkspace', name: 'terminal' });
    expect(invoke).not.toHaveBeenCalled();
  });

  test('every outcome other than applied, and a rejection, only reach console.debug', async () => {
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});
    const rendered = { generation: '1', sequence: '2' };
    const target = { kind: 'focusWorkspace', name: 'coding' } as const;

    vi.mocked(invoke).mockResolvedValueOnce({ kind: 'applied' });
    tilingClick(rendered, target);
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    await Promise.resolve();
    expect(debug).not.toHaveBeenCalled();

    const outcomes = [
      { kind: 'noOp' },
      { kind: 'stale' },
      { kind: 'unavailable', reason: 'disabled' },
      { kind: 'busy' },
      { kind: 'uncertain' },
      { kind: 'refused' },
      { kind: 'failed' },
      { kind: 'invalid' },
      { kind: 'unauthorized' },
    ];

    for (const outcome of outcomes) {
      vi.mocked(invoke).mockResolvedValueOnce(outcome);
      tilingClick(rendered, target);
    }

    vi.mocked(invoke).mockRejectedValueOnce(new Error('ipc down'));
    tilingClick(rendered, target);

    await vi.waitFor(() => expect(debug).toHaveBeenCalledTimes(outcomes.length + 1));
    expect(debug.mock.calls.map(([message]) => message)).toEqual([
      ...outcomes.map(({ kind }) => `tiling focusWorkspace: ${kind}`),
      'tiling focusWorkspace failed',
    ]);
    debug.mockRestore();
  });
});
