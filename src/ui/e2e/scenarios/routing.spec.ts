import type { HostStatus_Serialize } from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/* `bar` renders the Bar window (bar-layout.spec.ts); `access` the Accessibility guide. */
const READY: HostStatus_Serialize = {
  ready: true,
  error: null,
  access: { state: 'blocked', revoked: false, appPath: null },
  modules: [],
  config: [],
};
const APP = { name: 'Stache Next', version: '0.1.0', identifier: 'com.marcosmoura.stache.next' };

test.describe('surface routing', () => {
  for (const [surface, heading] of [
    ['access', 'Stache Next needs Accessibility access'],
  ] as const) {
    test(`?surface=${surface} renders its window with only app_info, host_status and the event listen`, async ({
      page,
      host,
      consoleErrors,
    }) => {
      host.answer('app_info', APP).answerHostStatus(READY);
      await page.goto(`/?surface=${surface}`);

      await expect(page.getByRole('main')).toHaveAttribute('data-surface', surface);
      await expect(page.getByText(heading)).toBeVisible();
      const allowed = ['plugin:event|listen', 'plugin:event|unlisten', 'host_status', 'app_info'];

      expect(host.calls.filter((call) => !allowed.includes(call.command))).toEqual([]);
      expect(new Set(host.commands())).toEqual(new Set(['app_info', 'host_status']));
      expect(host.unanswered).toEqual([]);
      expect(consoleErrors).toEqual([]);
    });
  }

  for (const search of [
    '',
    '?surface=settings',
    '?surface=',
    '?surface=Bar',
    '?surface=shell',
    '?surface=test',
  ]) {
    test(`${search} renders the fallback with zero commands`, async ({
      page,
      host,
      consoleErrors,
    }) => {
      await page.goto(`/${search}`);

      await expect(page.getByRole('heading', { name: 'Unknown surface' })).toBeVisible();
      expect(host.calls).toEqual([]);
      expect(consoleErrors).toEqual([]);
    });
  }
});

test.describe('fake host', () => {
  test('answers scripted commands, records calls and delivers events', async ({ page, host }) => {
    host.answer('app_info', { name: 'Stache' }).fail('host_status', 'offline');
    await page.goto('/?surface=nope');
    await expect(page.getByRole('heading', { name: 'Unknown surface' })).toBeVisible();

    const result = await page.evaluate(async () => {
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (command: string, args?: unknown) => Promise<unknown>;
            transformCallback: (callback: (data: unknown) => void) => number;
          };
        }
      ).__TAURI_INTERNALS__;
      const received: unknown[] = [];
      const handler = internals.transformCallback((data) => received.push(data));

      await internals.invoke('plugin:event|listen', {
        event: 'status-changed',
        target: { kind: 'Any' },
        handler,
      });

      const info = await internals.invoke('app_info');
      const status = await internals.invoke('host_status').catch((error: unknown) => error);
      const missing = await internals.invoke('nope').catch((error: unknown) => String(error));

      (window as unknown as { received: unknown[] }).received = received;

      return { info, status, missing };
    });

    expect(result).toEqual({
      info: { name: 'Stache' },
      status: 'offline',
      missing: 'fake host: no scripted answer for "nope"',
    });
    expect(host.commands()).toEqual(['app_info', 'host_status', 'nope']);
    expect(host.unanswered).toEqual(['nope']);
    expect(await host.emit('status-changed', { ready: true })).toBe(1);
    expect(
      await page.evaluate(() => (window as unknown as { received: unknown[] }).received),
    ).toEqual([{ event: 'status-changed', id: expect.any(Number), payload: { ready: true } }]);
  });
});
