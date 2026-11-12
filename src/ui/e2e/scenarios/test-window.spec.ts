import type { HostStatus_Serialize } from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/*
 * The test window (`?surface=test`) through the fake host: loading, the host
 * status with modules and files, a live `status-changed`, and only
 * `app_info`, `host_status` and the event listen called.
 */

const APP = { name: 'Stache Next', version: '0.1.0', identifier: 'com.marcosmoura.stache.next' };
const HUGE_REVISION = '18446744073709551615';

const READY: HostStatus_Serialize = {
  ready: true,
  error: null,
  access: { state: 'open', revoked: false, appPath: null },
  modules: [
    {
      id: 'proxyAudio',
      state: 'disabled',
      enabled: false,
      paused: false,
      revision: '1',
      detail: null,
      diagnostic: null,
    },
    {
      id: 'tiling',
      state: 'running',
      enabled: true,
      paused: false,
      revision: '4',
      detail: 'waiting for displays',
      diagnostic: 'unknown layout "spiral"',
    },
  ],
  config: [
    {
      file: 'global.toml',
      path: '/cfg/global.toml',
      state: 'valid',
      active: true,
      diagnostic: null,
      warnings: ['unknown key `foo`'],
    },
    {
      file: 'proxyAudio.toml',
      path: '/cfg/proxyAudio.toml',
      state: 'invalid',
      active: false,
      diagnostic: 'expected a table',
      warnings: [],
    },
  ],
};

test.describe('test window', () => {
  test('shows the host status and follows status-changed', async ({
    page,
    host,
    consoleErrors,
  }) => {
    let answer: (status: HostStatus_Serialize) => void = () => {};

    host
      .answer('app_info', APP)
      .answer('host_status', () => new Promise((resolve) => (answer = resolve)));
    await page.goto('/?surface=test');

    await expect(page.getByRole('status')).toHaveText('Reading host status…');
    await expect(page.getByText('com.marcosmoura.stache.next (v0.1.0)')).toBeVisible();
    answer(READY);

    await expect(page.getByText('Host ready')).toBeVisible();
    const tiling = page
      .getByRole('listitem')
      .filter({ has: page.locator('h3', { hasText: 'tiling' }) });

    await expect(tiling.locator('[data-state]')).toHaveText('Running');
    await expect(tiling.getByRole('heading', { name: 'Runtime detail' })).toBeVisible();
    await expect(tiling.getByRole('heading', { name: 'Configuration diagnostic' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'File diagnostic' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Warnings' })).toBeVisible();

    await host.emitStatusChanged({
      ...READY,
      modules: [{ ...READY.modules[0]!, revision: HUGE_REVISION }],
    });
    await expect(page.getByText(HUGE_REVISION, { exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'tiling' })).toHaveCount(0);

    expect(new Set(host.commands())).toEqual(new Set(['app_info', 'host_status']));
    expect(
      host.calls.filter(
        (call) => call.command.startsWith('plugin:') && !call.command.startsWith('plugin:event|'),
      ),
    ).toEqual([]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });

  test('reports an unavailable host status', async ({ page, host }) => {
    host.answer('app_info', APP).fail('host_status', 'offline');
    await page.goto('/?surface=test');

    await expect(page.getByRole('alert')).toContainText('Host status unavailable');
    await expect(page.getByRole('alert')).toContainText('offline');
  });
});
