import type { HostStatus_Serialize } from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

import { pinSystemColours } from './systemColours';

/*
 * Baselines of the Accessibility guide (blocked, revoked),
 * dark and light, from production `index.html` with the fake host. The guide
 * has the popover's native look: `pinSystemColours` pins the user-dependent
 * system colours and paints a stand-in for the native material behind the
 * transparent page, and the system font needs no skip. Update with
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
        await pinSystemColours(page, scheme);

        await expect(page.getByText(APP.identifier)).toBeVisible();
        await page.evaluate(() => document.fonts.ready.then(() => undefined));
        await expect(page).toHaveScreenshot(`access-${name}-${scheme}.png`, {
          omitBackground: true,
        });
      });
    }
  });
}
