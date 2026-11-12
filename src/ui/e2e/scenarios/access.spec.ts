import type { AccessView_Serialize, HostStatus_Serialize } from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/*
 * The Accessibility guide (`?surface=access`) through the fake host: blocked,
 * a request, revoked, then open and the window closing itself. It may call
 * only `app_info`, `host_status`, its three commands, the event listen and the
 * window close.
 */

const APP = { name: 'Stache Next', version: '0.1.0', identifier: 'com.marcosmoura.stache.next' };
const APP_PATH = '/Applications/Stache Next.app';

const status = (
  access: Partial<AccessView_Serialize>,
  error: string | null = null,
): HostStatus_Serialize => ({
  ready: true,
  error,
  access: { state: 'blocked', revoked: false, appPath: APP_PATH, ...access },
  modules: [],
  config: [],
});

const ALLOWED = new Set([
  'app_info',
  'host_status',
  'request_accessibility',
  'open_accessibility_settings',
  'quit_app',
  'plugin:event|listen',
  'plugin:event|unlisten',
  'plugin:window|close',
]);

test.describe('Accessibility guide', () => {
  test('blocked, request, revoked, then open and self-close', async ({
    page,
    host,
    consoleErrors,
  }) => {
    let releaseRequest: () => void = () => {};

    host
      .answer('app_info', APP)
      .answerHostStatus(status({}))
      .answer('plugin:window|close', null)
      .answer(
        'request_accessibility',
        () =>
          new Promise<null>((resolve) => {
            releaseRequest = () => resolve(null);
          }),
      )
      .answer('open_accessibility_settings', null);
    await page.goto('/?surface=access');

    // Blocked: steps, identity and collapsed troubleshooting.
    await expect(
      page.getByRole('heading', { name: 'Stache Next needs Accessibility access' }),
    ).toBeVisible();
    await expect(page.getByRole('listitem')).toHaveCount(3);
    await expect(page.getByText(APP.identifier)).toBeVisible();
    await expect(page.getByText(APP_PATH)).toBeVisible();
    await expect(page.getByRole('button', { name: 'Troubleshooting' })).toHaveAttribute(
      'aria-expanded',
      'false',
    );
    // The page is transparent around the rounded panel.
    expect(
      await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor),
    ).toBe('rgba(0, 0, 0, 0)');
    await expect(page.getByRole('main')).toHaveCSS('border-top-left-radius', '16px');

    // Request: sent once, disabled while pending.
    const request = page.getByRole('button', { name: 'Request Access' });

    await request.click();
    await expect(request).toBeDisabled();
    expect(host.commands().filter((command) => command === 'request_accessibility')).toHaveLength(
      1,
    );
    releaseRequest();
    await expect(request).toBeEnabled();

    await page.getByRole('button', { name: 'Open System Settings' }).click();
    await expect
      .poll(() => host.commands().filter((command) => command === 'open_accessibility_settings'))
      .toHaveLength(1);

    // A host error is shown.
    await host.emitStatusChanged(status({}, 'Accessibility pane did not open'));
    await expect(page.getByRole('alert')).toHaveText('Accessibility pane did not open');

    // Revoked.
    await host.emitStatusChanged(status({ revoked: true }));
    await expect(
      page.getByRole('heading', { name: 'Accessibility access was turned off' }),
    ).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);

    // Open: the success line, then the window closes itself.
    expect(host.commands()).not.toContain('plugin:window|close');
    await host.emitStatusChanged(status({ state: 'open' }));
    await expect(page.getByRole('status')).toHaveText('Access granted. Starting Stache Next…');
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'plugin:window|close'))
      .toEqual([{ command: 'plugin:window|close', args: { label: 'access' } }]);
    await expect(page.locator('footer')).toHaveCount(0);

    expect(host.calls.filter((call) => !ALLOWED.has(call.command))).toEqual([]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });

  test('the unbundled variant, Escape and Close', async ({ page, host, consoleErrors }) => {
    host
      .answer('app_info', APP)
      .answerHostStatus(status({ appPath: null }))
      .answer('plugin:window|close', null);
    await page.goto('/?surface=access');

    await expect(page.getByText('not an app bundle')).toBeVisible();
    await page.getByRole('button', { name: 'Troubleshooting' }).click();
    await expect(page.getByText(/This copy is not an app bundle\./)).toBeVisible();

    const closes = () => host.commands().filter((command) => command === 'plugin:window|close');

    await page.keyboard.press('Escape');
    await expect.poll(closes).toHaveLength(1);
    await page.getByRole('button', { name: 'Close' }).click();
    await expect.poll(closes).toHaveLength(2);
    // The Close button is outside the drag region.
    await expect(page.locator('[data-tauri-drag-region]')).toHaveText('Stache Next');

    expect(host.calls.filter((call) => !ALLOWED.has(call.command))).toEqual([]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });

  test('Quit sends quit_app once', async ({ page, host, consoleErrors }) => {
    host.answer('app_info', APP).answerHostStatus(status({})).answer('quit_app', null);
    await page.goto('/?surface=access');

    await page.getByRole('button', { name: 'Quit Stache Next' }).click();
    await expect
      .poll(() => host.commands().filter((command) => command === 'quit_app'))
      .toHaveLength(1);
    expect(consoleErrors).toEqual([]);
  });
});
