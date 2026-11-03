import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { beforeEach, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { appInfo, hostStatus } from '../../../../tests/hostFixtures';

import { resolveSurface } from './surface';
import { WindowRouter } from './WindowRouter';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));

function client() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        staleTime: Infinity,
      },
    },
  });
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') return appInfo;
    if (command === 'host_status') return hostStatus();
    throw new Error(`Unexpected command: ${command}`);
  }) as never);
  vi.mocked(listen).mockResolvedValue(() => {});
});

test('resolves only declared surfaces', () => {
  expect(resolveSurface('')).toBe('shell');
  expect(resolveSurface('?other=test')).toBe('shell');
  expect(resolveSurface('?surface=test')).toBe('test');
  expect(resolveSurface('?surface=shell')).toBe('shell');
  expect(resolveSurface('?surface=unknown')).toBeNull();
  expect(resolveSurface('?surface=')).toBeNull();
  expect(resolveSurface('?surface=TEST')).toBeNull();
});

test('the default route renders the plain shell without calling native commands', async () => {
  const screen = await render(<WindowRouter search="" />);

  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
  expect(invoke).not.toHaveBeenCalled();
});

test('an unknown surface renders the fallback without calling native commands', async () => {
  const screen = await render(<WindowRouter search="?surface=unknown" />);

  await expect.element(screen.getByRole('heading', { name: 'Unknown surface' })).toBeVisible();
  expect(invoke).not.toHaveBeenCalled();
  expect(document.body.textContent).not.toContain('Reading host status');
});

test('the test surface renders the test window and reads host status', async () => {
  const screen = await render(
    <QueryClientProvider client={client()}>
      <WindowRouter search="?surface=test" />
    </QueryClientProvider>,
  );

  await expect.element(screen.getByText('Host test surface')).toBeVisible();
  await expect.element(screen.getByRole('heading', { name: 'Host' })).toBeVisible();
  expect(invoke).toHaveBeenCalledWith('host_status');
});
