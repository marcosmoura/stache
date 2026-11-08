import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { beforeEach, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { appInfo, hostStatus } from '../../tests/hostFixtures';

import { Shell } from './Shell';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));

vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

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
  vi.mocked(getCurrentWindow).mockReset();
  vi.mocked(getCurrentWindow).mockReturnValue({
    close: vi.fn().mockResolvedValue(undefined),
  } as never);
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return hostStatus();
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);
  vi.mocked(listen).mockResolvedValue(() => {});
});

test.each(['', '?other=test', '?surface=shell'])(
  'the query %j renders the plain shell',
  async (search) => {
    const screen = await render(<Shell search={search} />);

    await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
    expect(invoke).not.toHaveBeenCalled();
  },
);

test.each([
  '?surface=',
  '?surface=TEST',
  '?surface=Access',
  '?surface=toString',
  '?surface=__proto__',
])('the undeclared surface %j renders the fallback', async (search) => {
  const screen = await render(<Shell search={search} />);

  await expect.element(screen.getByRole('heading', { name: 'Unknown surface' })).toBeVisible();
  expect(invoke).not.toHaveBeenCalled();
});

test('the default route renders the plain shell without calling native commands', async () => {
  const screen = await render(<Shell search="" />);

  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
  expect(invoke).not.toHaveBeenCalled();
});

test('an unknown surface renders the fallback without calling native commands', async () => {
  const screen = await render(<Shell search="?surface=unknown" />);

  await expect.element(screen.getByRole('heading', { name: 'Unknown surface' })).toBeVisible();
  expect(invoke).not.toHaveBeenCalled();
  expect(document.body.textContent).not.toContain('Reading host status');
});

test('the test surface renders the test window and reads host status', async () => {
  const screen = await render(
    <QueryClientProvider client={client()}>
      <Shell search="?surface=test" />
    </QueryClientProvider>,
  );

  await expect.element(screen.getByText('Host test surface')).toBeVisible();
  await expect.element(screen.getByRole('heading', { name: 'Host' })).toBeVisible();
  expect(invoke).toHaveBeenCalledWith('host_status');
});

test('the access surface renders the guidance and reads host status', async () => {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return hostStatus({ access: { state: 'blocked', revoked: false, appPath: null } });
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);

  const screen = await render(
    <QueryClientProvider client={client()}>
      <Shell search="?surface=access" />
    </QueryClientProvider>,
  );

  await expect
    .element(screen.getByRole('heading', { name: 'Stache Next needs Accessibility access' }))
    .toBeVisible();
  expect(invoke).toHaveBeenCalledWith('host_status');
  expect(document.body.textContent).not.toContain('Host test surface');
});
