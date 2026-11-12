import type { HostStatus_Serialize } from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

import { skipWithoutMapleMono } from './font';

/*
 * Baselines of the Accessibility guide (blocked, revoked) and the test window,
 * dark and light, from production `index.html` with the fake host. Compared
 * only with 'Maple Mono NF' installed. Update with
 * `bunx playwright test src/ui/e2e/screenshots/windows.spec.ts --update-snapshots`.
 */

const APP = { name: 'Stache Next', version: '0.1.0', identifier: 'com.marcosmoura.stache.next' };

const guide = (revoked: boolean): HostStatus_Serialize => ({
  ready: true,
  error: null,
  access: { state: 'blocked', revoked, appPath: '/Applications/Stache Next.app' },
  modules: [],
  config: [],
});

const TEST_STATUS: HostStatus_Serialize = {
  ready: true,
  error: null,
  access: { state: 'open', revoked: false, appPath: null },
  modules: [
    {
      id: 'proxyAudio',
      state: 'disabled',
      enabled: false,
      paused: false,
      revision: '18446744073709551615',
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
      path: '/Users/me/.config/stache/global.toml',
      state: 'valid',
      active: true,
      diagnostic: null,
      warnings: ['unknown key `foo`'],
    },
    {
      file: 'tiling.toml',
      path: '/Users/me/.config/stache/tiling.toml',
      state: 'invalid',
      active: false,
      diagnostic: 'expected a table',
      warnings: [],
    },
  ],
};

for (const scheme of ['dark', 'light'] as const) {
  test.describe(`windows (${scheme})`, () => {
    test.use({ deviceScaleFactor: 2 });

    for (const revoked of [false, true]) {
      const name = revoked ? 'revoked' : 'blocked';

      test(`Accessibility guide ${name}`, async ({ page, host }) => {
        await page.setViewportSize({ width: 480, height: 640 });
        host.answer('app_info', APP).answerHostStatus(guide(revoked));
        await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
        await page.goto('/index.html?surface=access');
        await skipWithoutMapleMono(page);

        await expect(page.getByText(APP.identifier)).toBeVisible();
        await page.evaluate(() => document.fonts.ready.then(() => undefined));
        await expect(page).toHaveScreenshot(`access-${name}-${scheme}.png`, {
          omitBackground: true,
        });
      });
    }

    test('test window', async ({ page, host }) => {
      await page.setViewportSize({ width: 900, height: 1100 });
      host.answer('app_info', APP).answerHostStatus(TEST_STATUS);
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.goto('/index.html?surface=test');
      await skipWithoutMapleMono(page);

      await expect(page.getByText('Host ready')).toBeVisible();
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(page).toHaveScreenshot(`test-window-${scheme}.png`, { fullPage: true });
    });
  });
}
