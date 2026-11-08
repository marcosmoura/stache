import { invoke } from '@tauri-apps/api/core';
import * as eventApi from '@tauri-apps/api/event';
import { beforeEach, expect, test, vi } from 'vitest';

import { appInfo, hostStatus } from '../../tests/hostFixtures';

import { events } from './bindings.gen';
import { readAppInfo, readHostStatus, subscribeToStatus } from './client';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(eventApi.listen).mockReset();
  vi.mocked(eventApi.once).mockReset();
  vi.mocked(eventApi.emit).mockReset();
});

test('reads app info through the generated command wrapper', async () => {
  vi.mocked(invoke).mockResolvedValue(appInfo);

  await expect(readAppInfo()).resolves.toEqual(appInfo);
  expect(invoke).toHaveBeenCalledWith('app_info');
});

test('reads host status through the generated command wrapper', async () => {
  const status = hostStatus();

  vi.mocked(invoke).mockResolvedValue(status);

  await expect(readHostStatus()).resolves.toEqual(status);
  expect(invoke).toHaveBeenCalledWith('host_status');
});

test('subscribes to the generated status event and unwraps its payload', async () => {
  const unlisten = vi.fn();

  vi.mocked(eventApi.listen).mockResolvedValue(unlisten);
  const onStatus = vi.fn();

  const stop = await subscribeToStatus(onStatus);

  expect(eventApi.listen).toHaveBeenCalledWith('status-changed', expect.any(Function));
  const handler = vi.mocked(eventApi.listen).mock.calls[0]?.[1];

  if (!handler) {
    throw new Error('status listener was not registered');
  }

  const status = hostStatus({ ready: false });

  handler({ event: 'status-changed', id: 1, payload: { status } });
  expect(onStatus).toHaveBeenCalledWith(status);

  stop();
  expect(unlisten).toHaveBeenCalledTimes(1);
});

test('generated event wrapper exposes once, emit and webview-scoped variants', async () => {
  const onceUnlisten = vi.fn();

  vi.mocked(eventApi.once).mockResolvedValue(onceUnlisten);
  const statusChanged = { status: hostStatus() };
  const callback = vi.fn();

  await events.statusChanged.once(callback);
  expect(eventApi.once).toHaveBeenCalledWith('status-changed', expect.any(Function));

  await events.statusChanged.emit(statusChanged);
  expect(eventApi.emit).toHaveBeenCalledWith('status-changed', statusChanged);

  const target = {
    listen: vi.fn().mockResolvedValue(vi.fn()),
    once: vi.fn().mockResolvedValue(vi.fn()),
    emit: vi.fn().mockResolvedValue(undefined),
  };
  const scoped = events.statusChanged(target as never);

  await scoped.listen(callback);
  await scoped.once(callback);
  await scoped.emit(statusChanged);

  expect(target.listen).toHaveBeenCalledWith('status-changed', expect.any(Function));
  expect(target.once).toHaveBeenCalledWith('status-changed', expect.any(Function));
  expect(target.emit).toHaveBeenCalledWith('status-changed', statusChanged);
});
