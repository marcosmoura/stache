import { StrictMode } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { availableOutcome, barData, unavailableOutcome } from '../../../../tests/barFixtures';
import type { BarChanged, BarSnapshotOutcome } from '../../../sdk/bindings.gen';

import { Bar } from './Bar';
import type { BarData } from './Bar.data';
import { isMenuBarVisible } from './Bar.state';
import { BarDataProvider } from './BarDataProvider';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));

type Handler = (event: { payload: BarChanged }) => void;

const unlisten = vi.fn();

function snapshotIs(outcome: BarSnapshotOutcome | Promise<BarSnapshotOutcome>) {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'bar_snapshot') {
      return outcome;
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);
}

function emit(payload: BarChanged) {
  const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as Handler | undefined;

  if (!handler) {
    throw new Error('bar-changed listener was not registered');
  }

  handler({ payload });
}

async function renderBar(data?: BarData) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  const tree = (
    <QueryClientProvider client={client}>
      {data ? (
        <BarDataProvider data={data}>
          <Bar />
        </BarDataProvider>
      ) : (
        <Bar />
      )}
    </QueryClientProvider>
  );
  const screen = await render(tree);

  screen.container.style.height = '28px';

  return { client, screen };
}

function bar(screen: { container: HTMLElement }) {
  return screen.container.firstElementChild as HTMLElement;
}

beforeEach(() => {
  unlisten.mockReset();
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  vi.mocked(listen).mockResolvedValue(unlisten);
  snapshotIs(availableOutcome({ sequence: '1' }));
});

describe('menu visibility mapping', () => {
  test.each([
    ['visible', true],
    ['hidden', false],
    ['unknown', false],
  ] as const)('menu %s -> hidden Bar %s', (state, hidden) => {
    expect(isMenuBarVisible(availableOutcome({ menu: { state, reason: null } }))).toBe(hidden);
  });

  test('unavailable, unauthorized and unread feeds keep the Bar shown', () => {
    expect(isMenuBarVisible(unavailableOutcome('2'))).toBe(false);
    expect(isMenuBarVisible({ kind: 'unauthorized' })).toBe(false);
    expect(isMenuBarVisible(undefined)).toBe(false);
  });
});

describe('Bar', () => {
  test('renders the grid with the empty Spaces slot, no Media and the Clock', async () => {
    const { screen } = await renderBar();
    const root = bar(screen);
    const style = getComputedStyle(root);

    expect(style.display).toBe('grid');
    expect(style.gridAutoFlow).toBe('column');
    expect(style.justifyContent).toBe('space-between');
    expect(style.justifyItems).toBe('center');
    expect(style.alignItems).toBe('center');
    expect(style.overflow).toBe('hidden');
    expect(style.transitionProperty).toBe('transform, opacity');
    expect(style.transitionDuration).toBe('0.15s');
    expect(style.transitionTimingFunction).toBe('cubic-bezier(0.42, 0, 0.58, 1)');
    expect(root.children[0]?.tagName).toBe('DIV');
    expect(root.children[0]?.childElementCount).toBe(0);
    expect(root.children).toHaveLength(2);
    expect(root.querySelector('[data-testid="status-container"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="media-container"]')).toBeNull();
  });

  test('renders Media between the Spaces slot and Status', async () => {
    const { screen } = await renderBar(
      barData({
        media: { artist: 'A', title: 'T', bundleIdentifier: 'com.spotify.client', playing: true },
      }),
    );
    const root = bar(screen);

    expect(root.children).toHaveLength(3);
    expect(root.children[1]?.getAttribute('data-testid')).toBe('media-container');
    expect(root.children[2]?.getAttribute('data-testid')).toBe('status-container');
  });

  test('slides away while the menu bar is visible and returns when it hides', async () => {
    snapshotIs(availableOutcome({ sequence: '1', menu: { state: 'visible', reason: null } }));

    const { screen } = await renderBar();
    const root = bar(screen);

    await expect.poll(() => getComputedStyle(root).opacity).toBe('0');
    expect(getComputedStyle(root).transform).toBe('matrix(1, 0, 0, 1, 0, 28)');

    emit({
      generation: '1',
      sequence: '2',
      resnapshot: false,
      outcome: availableOutcome({ sequence: '2', menu: { state: 'hidden', reason: null } }),
    });

    await expect.poll(() => getComputedStyle(root).opacity).toBe('1');
    expect(getComputedStyle(root).transform).toBe('none');
  });

  test('stays shown for unavailable and unauthorized answers without crashing', async () => {
    snapshotIs(unavailableOutcome('3', 'waitingForPermission'));

    const first = await renderBar();

    expect(getComputedStyle(bar(first.screen)).opacity).toBe('1');
    await first.screen.unmount();

    snapshotIs({ kind: 'unauthorized' });

    const second = await renderBar();

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(getComputedStyle(bar(second.screen)).opacity).toBe('1');
  });

  test('stays shown when the snapshot command fails', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('ipc failed'));

    const { screen } = await renderBar();

    await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
    expect(getComputedStyle(bar(screen)).transform).toBe('none');
  });

  test('subscribes before it reads the snapshot and disposes under StrictMode', async () => {
    const order: string[] = [];

    vi.mocked(listen).mockImplementation((async () => {
      order.push('listen');

      return unlisten;
    }) as never);
    vi.mocked(invoke).mockImplementation((async () => {
      order.push('invoke');

      return availableOutcome();
    }) as never);

    const client = new QueryClient();
    const screen = await render(
      <StrictMode>
        <QueryClientProvider client={client}>
          <Bar />
        </QueryClientProvider>
      </StrictMode>,
    );

    await vi.waitFor(() =>
      expect(order.filter((step) => step === 'invoke').length).toBeGreaterThan(0),
    );
    expect(order[0]).toBe('listen');
    expect(order.indexOf('listen')).toBeLessThan(order.indexOf('invoke'));

    await screen.unmount();
    // Every listener that was created is released by the time the tree is gone.
    await vi.waitFor(() =>
      expect(unlisten).toHaveBeenCalledTimes(order.filter((s) => s === 'listen').length),
    );
  });

  test('sets the Bar document marker and applies the legacy root rules only while mounted', async () => {
    const before = getComputedStyle(document.documentElement);

    expect(before.overflow).toBe('visible');
    expect(document.documentElement.dataset.surface).toBeUndefined();

    const { screen } = await renderBar();
    const root = getComputedStyle(document.documentElement);

    expect(document.documentElement.dataset.surface).toBe('bar');
    expect(root.overflow).toBe('hidden');
    expect(root.fontFamily).toBe('Maple Mono NF, SF Mono, monospace');
    expect(root.fontSize).toBe('13px');
    expect(root.fontWeight).toBe('500');
    expect(root.fontVariantNumeric).toBe('tabular-nums');
    expect(Number.parseFloat(root.lineHeight)).toBeCloseTo(20.8, 3);
    expect(root.height).not.toBe('0px');
    expect(getComputedStyle(document.body).margin).toBe('0px');

    await screen.unmount();

    expect(document.documentElement.dataset.surface).toBeUndefined();
    expect(getComputedStyle(document.documentElement).overflow).toBe('visible');
  });

  test('keeps legacy media rules: svg display block and img height auto yield to classes', async () => {
    const { screen } = await renderBar();
    const svg = screen.container.querySelector('svg') as SVGElement;

    expect(getComputedStyle(svg).display).toBe('block');
    expect(getComputedStyle(svg).maxWidth).toBe('100%');
  });

  test('shows the legacy error fallback when a child throws and recovers on retry', async () => {
    const throwing = {
      get isConfigured(): boolean {
        throw new Error('child failed');
      },
      isLoading: false,
    };
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    const { screen } = await renderBar(barData({ weather: throwing }));

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();
    expect(screen.container.querySelector('[role="alert"]')).not.toBeNull();
    spy.mockRestore();
  });

  test('renders nothing (Suspense fallback null) while a child is suspended', async () => {
    const suspended = {
      get isConfigured(): boolean {
        throw new Promise(() => {});
      },
      isLoading: false,
    };

    const { screen } = await renderBar(barData({ weather: suspended }));

    expect(screen.container.firstElementChild).toBeNull();
  });
});
