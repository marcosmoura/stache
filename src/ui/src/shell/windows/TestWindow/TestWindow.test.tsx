import { StrictMode } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { beforeEach, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { appInfo, configFile, hostStatus, moduleStatus } from '../../../../tests/hostFixtures';
import { renderWithClient } from '../../../../tests/renderWithClient';
import { HOST_STATUS_QUERY_KEY } from '../../../sdk/client';
import type { HostStatus } from '../../../sdk/client';

import { TestWindow } from './TestWindow';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));

type StatusHandler = (event: { payload: { status: HostStatus } }) => void;

function mockHostCommands(status: Promise<HostStatus> | HostStatus) {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return status;
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);
}

function emitStatus(status: HostStatus) {
  const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as StatusHandler | undefined;

  if (!handler) {
    throw new Error('status listener was not registered');
  }

  handler({ payload: { status } });
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
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

test('renders status, modules and configuration with exact revision text', async () => {
  const revision = '18446744073709551615';

  mockHostCommands(
    hostStatus({
      modules: [
        moduleStatus({
          revision,
          detail: 'Input priority applied.',
          diagnostic: 'Unknown module key "tempo".',
        }),
      ],
      config: [
        configFile({
          state: 'invalid',
          active: false,
          diagnostic: 'Unknown top-level key "theme".',
          warnings: ['input[0].dependsOn is ignored.'],
        }),
      ],
    }),
  );

  const { screen } = await renderWithClient(<TestWindow />);

  await expect.element(screen.getByRole('heading', { name: 'Host' })).toBeVisible();
  await expect.element(screen.getByText('Host ready')).toBeVisible();
  await expect.element(screen.getByRole('heading', { name: 'Modules' })).toBeVisible();
  await expect.element(screen.getByRole('heading', { name: 'proxy-audio' })).toBeVisible();
  await expect.element(screen.getByText('Input priority applied.')).toBeVisible();
  await expect.element(screen.getByText('Unknown module key "tempo".')).toBeVisible();
  await expect
    .element(screen.getByRole('heading', { name: 'Configuration', exact: true }))
    .toBeVisible();
  await expect.element(screen.getByText('Unknown top-level key "theme".')).toBeVisible();
  await expect.element(screen.getByText('input[0].dependsOn is ignored.')).toBeVisible();

  expect(screen.getByText(revision).element().textContent).toBe(revision);
});

test('separates a host error from configuration diagnostics', async () => {
  mockHostCommands(
    hostStatus({
      ready: false,
      error: 'Module registry could not start.',
      config: [configFile({ diagnostic: 'File is not valid JSONC.' })],
    }),
  );

  const { screen } = await renderWithClient(<TestWindow />);

  await expect.element(screen.getByRole('alert')).toBeVisible();
  await expect.element(screen.getByText('Host error')).toBeVisible();
  await expect.element(screen.getByText('Module registry could not start.')).toBeVisible();
  await expect.element(screen.getByText('File diagnostic')).toBeVisible();
  await expect.element(screen.getByText('File is not valid JSONC.')).toBeVisible();
});

test('shows loading, then an empty host without modules or configuration', async () => {
  let resolveStatus: ((status: HostStatus) => void) | undefined;

  mockHostCommands(
    new Promise<HostStatus>((resolve) => {
      resolveStatus = resolve;
    }),
  );

  const { screen } = await renderWithClient(<TestWindow />);

  await expect.element(screen.getByText('Reading host status…')).toBeVisible();
  expect(screen.getByRole('heading', { name: 'Modules' }).query()).toBeNull();

  resolveStatus?.(hostStatus({ modules: [], config: [] }));

  await expect.element(screen.getByText('No modules registered.')).toBeVisible();
  await expect.element(screen.getByText('No configuration files.')).toBeVisible();
});

test('shows a host status failure instead of stale data', async () => {
  mockHostCommands(Promise.reject(new Error('host is not reachable')));

  const { screen } = await renderWithClient(<TestWindow />);

  await expect.element(screen.getByRole('alert')).toBeVisible();
  await expect.element(screen.getByText('Host status unavailable')).toBeVisible();
  await expect.element(screen.getByText('host is not reachable')).toBeVisible();
});

test('reports an application identity failure without hiding host status', async () => {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      throw new Error('identity unavailable');
    }

    if (command === 'host_status') {
      return hostStatus();
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);

  const { screen } = await renderWithClient(<TestWindow />);

  await expect
    .element(screen.getByText('Application identity unavailable: identity unavailable'))
    .toBeVisible();
  await expect.element(screen.getByText('Host ready')).toBeVisible();
});

test('a status event updates the view and a stale snapshot does not replace it', async () => {
  let resolveStatus: ((status: HostStatus) => void) | undefined;

  mockHostCommands(
    new Promise<HostStatus>((resolve) => {
      resolveStatus = resolve;
    }),
  );

  const { screen } = await renderWithClient(<TestWindow />);

  await vi.waitFor(() => expect(vi.mocked(listen)).toHaveBeenCalled());
  await vi.waitFor(() => expect(resolveStatus).toBeTypeOf('function'));

  emitStatus(hostStatus({ ready: true, modules: [moduleStatus({ id: 'from-event' })] }));

  await expect.element(screen.getByRole('heading', { name: 'from-event' })).toBeVisible();

  resolveStatus?.(hostStatus({ ready: false, modules: [moduleStatus({ id: 'from-snapshot' })] }));

  await vi.waitFor(() =>
    expect(screen.getByRole('heading', { name: 'from-event' }).query()).not.toBeNull(),
  );
  expect(screen.getByRole('heading', { name: 'from-snapshot' }).query()).toBeNull();
});

test('StrictMode removes listeners that resolve after unmount', async () => {
  const stops = [vi.fn(), vi.fn()];
  const resolvers: Array<(stop: () => void) => void> = [];

  vi.mocked(listen).mockImplementation((() => {
    return new Promise<() => void>((resolve) => {
      resolvers.push(resolve);
    });
  }) as never);

  const client = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        staleTime: Infinity,
      },
    },
  });
  const screen = await render(
    <StrictMode>
      <QueryClientProvider client={client}>
        <TestWindow />
      </QueryClientProvider>
    </StrictMode>,
  );

  await vi.waitFor(() => expect(resolvers.length).toBeGreaterThanOrEqual(2));
  await screen.unmount();

  resolvers.slice(0, 2).forEach((resolve, index) => resolve(stops[index]));

  await vi.waitFor(() => {
    expect(stops[0]).toHaveBeenCalledTimes(1);
    expect(stops[1]).toHaveBeenCalledTimes(1);
  });
  expect(client.getQueryData(HOST_STATUS_QUERY_KEY)).toBeUndefined();
});

test('uses the Catppuccin Mocha token background and text colors', async () => {
  const { screen } = await renderWithClient(<TestWindow />);

  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();

  const main = document.querySelector('main');

  if (!main) {
    throw new Error('Test window did not mount');
  }

  const computed = getComputedStyle(main);

  expect(computed.backgroundColor).toBe('rgb(30, 30, 46)');
  expect(computed.color).toBe('rgb(205, 214, 244)');
  expect(computed.fontFamily).toContain('system-ui');
});

test('AG-05-09 the test window leaves the document and its page opaque', async () => {
  const { screen } = await renderWithClient(<TestWindow />);

  await expect.element(screen.getByText('Host test surface')).toBeVisible();

  expect(document.documentElement.dataset.surface).toBeUndefined();
  const page = document.querySelector('main');

  expect(page && getComputedStyle(page).backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
});
