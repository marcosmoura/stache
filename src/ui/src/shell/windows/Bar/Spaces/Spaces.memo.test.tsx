import { createElement, type ComponentProps } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { beforeEach, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { availableOutcome, tilingStrip } from '../../../../../tests/barFixtures';
import type { BarChanged, BarSnapshotOutcome } from '../../../../sdk/bindings.gen';
import { BAR_SNAPSHOT_QUERY_KEY } from '../../../../sdk/client';
import { Bar } from '../Bar';
import { barDataFromOutcome } from '../Bar.data';

const renders = vi.hoisted(() => ({ app: 0, workspace: 0 }));

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));

vi.mock('./components/App/App', async (importOriginal) => {
  const original = await importOriginal<typeof import('./components/App/App')>();

  return {
    App: (props: ComponentProps<typeof original.App>) => {
      renders.app += 1;

      return createElement(original.App, props);
    },
  };
});

vi.mock('./components/Workspace/Workspace', async (importOriginal) => {
  const original = await importOriginal<typeof import('./components/Workspace/Workspace')>();

  return {
    Workspace: (props: ComponentProps<typeof original.Workspace>) => {
      renders.workspace += 1;

      return createElement(original.Workspace, props);
    },
  };
});

type Handler = (event: { payload: BarChanged }) => void;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  vi.mocked(listen).mockResolvedValue(vi.fn());
  renders.app = 0;
  renders.workspace = 0;
});

test('a BarChanged that changes only a non-Tiling field keeps the strip and its components', async () => {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'bar_snapshot') {
      return availableOutcome({ sequence: '1', tiling: tilingStrip() });
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);

  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  const screen = await render(
    <QueryClientProvider client={client}>
      <Bar />
    </QueryClientProvider>,
  );
  const held = () => client.getQueryData<BarSnapshotOutcome>(BAR_SNAPSHOT_QUERY_KEY);

  await expect.poll(() => screen.container.textContent).toContain('Ghostty');
  await vi.waitFor(() => expect(vi.mocked(listen)).toHaveBeenCalled());

  const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as Handler;
  const emit = (sequence: string, usage: number) => {
    handler({
      payload: {
        generation: '1',
        sequence,
        resnapshot: false,
        outcome: availableOutcome({
          sequence,
          cpu: { usage, temperature: null },
          tiling: tilingStrip(),
        }),
      },
    });
  };

  emit('2', 11);
  await expect.poll(() => screen.container.textContent).toContain('11%');

  const before = barDataFromOutcome(held()).spaces;
  const appRenders = renders.app;
  const workspaceRenders = renders.workspace;

  expect(appRenders).toBeGreaterThan(0);
  expect(workspaceRenders).toBeGreaterThan(0);

  emit('3', 42);
  await expect.poll(() => screen.container.textContent).toContain('42%');

  expect(barDataFromOutcome(held()).spaces).toBe(before);
  expect(renders.app).toBe(appRenders);
  expect(renders.workspace).toBe(workspaceRenders);
});
