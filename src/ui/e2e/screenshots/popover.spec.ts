import type {
  ConfigFileView_Serialize,
  HostStatus_Serialize,
  ModuleStatusView_Serialize,
} from '../../src/ipc/bindings.gen';
import { BAR, TILING } from '../configFixtures';
import { FakeConfig } from '../fakeConfig';
import { expect, test } from '../fixtures';

import { pinSystemColours } from './systemColours';

/*
 * Baselines of the tray popover, dark and light, from production
 * `index.html` with the fake host: the shell (the sidebar with each badge
 * kind over an empty Overview), the Overview with the user's modules, and
 * the settings editor: a simple tab (`keepAwake`), `tiling`, `bar` (its
 * layout zones and a widget expanded, then Weather) and an invalid file. The page is transparent, as it is over the native material, so the
 * harness paints a neutral stand-in for the material. The popover uses the system font and WebKit's
 * system colours; the user-dependent ones (accent and focus ring) are pinned
 * by `pinSystemColours` so the baselines do not follow System Settings, and
 * the system font needs no skip. Update with
 * `bunx playwright test src/ui/e2e/screenshots/popover.spec.ts --update-snapshots`.
 */

const file = (
  name: string,
  overrides: Partial<ConfigFileView_Serialize> = {},
): ConfigFileView_Serialize => ({
  file: name,
  path: `/Users/me/.config/stache/${name}`,
  state: 'valid',
  active: true,
  diagnostic: null,
  warnings: [],
  ...overrides,
});

const FILES: ConfigFileView_Serialize[] = [
  file('global.toml'),
  file('bar.toml'),
  file('commandQuit.toml'),
  file('keepAwake.toml'),
  file('keybindings.toml', { warnings: ['unknown key `foo`'] }),
  file('menuAnywhere.toml'),
  file('notunes.toml'),
  file('proxyAudio.toml'),
  file('tiling.toml', { state: 'invalid', active: false, diagnostic: 'expected a table' }),
  file('wallpapers.toml', { diagnostic: 'held: unknown mode' }),
];

const module = (
  id: string,
  overrides: Partial<ModuleStatusView_Serialize> = {},
): ModuleStatusView_Serialize => ({
  id,
  state: 'running',
  enabled: true,
  paused: false,
  revision: '1',
  detail: null,
  diagnostic: null,
  ...overrides,
});

const status = (modules: ModuleStatusView_Serialize[]): HostStatus_Serialize => ({
  ready: true,
  error: null,
  access: { state: 'open', revoked: false, appPath: null },
  modules,
  config: [],
});

const MODULES: ModuleStatusView_Serialize[] = [
  module('bar'),
  module('commandQuit'),
  module('keepAwake', { state: 'disabled', enabled: false }),
  module('keybindings', { state: 'paused', paused: true }),
  module('menuAnywhere'),
  module('notunes', { diagnostic: 'unknown field `foo`, expected `enabled`' }),
  module('proxyAudio', {
    state: 'failed',
    detail: 'start failed (coreAudio): the output device disappeared',
  }),
  module('tiling'),
  module('wallpapers', { state: 'disabled', enabled: false }),
];

for (const scheme of ['dark', 'light'] as const) {
  test.describe(`popover (${scheme})`, () => {
    test.use({ deviceScaleFactor: 2 });

    test('popover shell', async ({ page, host }) => {
      await page.setViewportSize({ width: 480, height: 640 });
      host.answer('config_files', FILES).answerHostStatus(status([]));
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.goto('/index.html?surface=popover');
      await pinSystemColours(page, scheme);

      await expect(page.getByRole('tab', { name: 'tiling.toml, invalid' })).toBeVisible();
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(page).toHaveScreenshot(`popover-${scheme}.png`, { omitBackground: true });
    });

    test('popover overview', async ({ page, host }) => {
      await page.setViewportSize({ width: 480, height: 640 });
      host.answer('config_files', FILES).answerHostStatus(status(MODULES));
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.goto('/index.html?surface=popover');
      await pinSystemColours(page, scheme);

      await expect(page.locator('[data-module="wallpapers"]')).toBeVisible();
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(page).toHaveScreenshot(`popover-overview-${scheme}.png`, {
        omitBackground: true,
      });
    });

    const tabs = [
      { name: 'keepAwake', tab: 'keepAwake.toml', texts: { 'keepAwake.toml': 'enabled = true\n' } },
      { name: 'tiling', tab: 'tiling.toml', texts: { 'tiling.toml': TILING } },
      {
        name: 'invalid',
        tab: 'tiling.toml',
        texts: { 'tiling.toml': TILING.replace('ratio = 60', 'ratio 60') },
      },
    ] as const;

    for (const { name, tab, texts } of tabs) {
      test(`popover ${name} tab`, async ({ page, host }) => {
        await page.setViewportSize({ width: 480, height: 640 });
        new FakeConfig(host, texts);
        host.answerHostStatus(status(MODULES));
        await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
        await page.goto('/index.html?surface=popover');
        await pinSystemColours(page, scheme);

        await page.getByRole('tab', { name: tab }).click();
        await expect(page.getByTestId('settings-form')).toBeVisible();
        await page.evaluate(() => document.fonts.ready.then(() => undefined));
        await expect(page).toHaveScreenshot(`popover-${name}-${scheme}.png`, {
          omitBackground: true,
        });
      });
    }

    test('popover bar tab', async ({ page, host }) => {
      await page.setViewportSize({ width: 480, height: 1100 });
      // One widget with its own options, so the expanded card shows them.
      const bar = BAR.replace('"cpu"', '{ widget = "cpu", temperature_alert = 90 }');

      expect(bar).not.toBe(BAR);
      new FakeConfig(host, { 'bar.toml': bar });
      host.answerHostStatus(status(MODULES));
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.goto('/index.html?surface=popover');
      await pinSystemColours(page, scheme);

      await page.getByRole('tab', { name: 'bar.toml' }).click();
      await expect(page.getByTestId('settings-form')).toBeVisible();

      for (const zone of ['Left', 'Right']) {
        await page
          .getByRole('region', { name: zone, exact: true })
          .getByRole('button', { name: zone, exact: true })
          .click();
      }

      await page
        .getByRole('group', { name: 'CPU' })
        .getByRole('button', { name: 'CPU', exact: true })
        .click();
      await expect(page.getByText('Alert temperature')).toBeVisible();
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(page).toHaveScreenshot(`popover-bar-${scheme}.png`, { omitBackground: true });
    });
  });
}
