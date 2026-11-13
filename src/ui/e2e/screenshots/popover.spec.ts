import type { Page } from '@playwright/test';

import type {
  ConfigFileView_Serialize,
  HostStatus_Serialize,
  ModuleStatusView_Serialize,
} from '../../src/ipc/bindings.gen';
import { TILING } from '../configFixtures';
import { FakeConfig } from '../fakeConfig';
import { expect, test } from '../fixtures';

/*
 * Baselines of the tray popover, dark and light, from production
 * `index.html` with the fake host: the shell (the sidebar with each badge
 * kind over an empty Overview), the Overview with the user's modules, and
 * the settings editor: a simple tab (`keepAwake`), `tiling` and an invalid
 * file. The page is transparent, as it is over the native material, so the
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

/** The system colours that follow user settings, pinned to macOS defaults. */
const PINNED: Readonly<Record<string, string>> = {
  '-apple-system-control-accent': 'rgb(0 122 255)',
  '-apple-system-selected-content-background': 'rgb(0 99 225)',
  '-webkit-focus-ring-color': 'rgb(0 103 244)',
};

/** Neutral stand-ins for the native `.popover` material behind the page. */
const MATERIAL = { dark: 'rgb(44 44 46)', light: 'rgb(236 236 236)' } as const;

/**
 * The harness's stable fallbacks: overrides each StyleX variable whose value
 * is a user-dependent system colour keyword with its default, on the root
 * element (inline styles win over the `:root` declarations), and paints a
 * neutral stand-in for the native material behind the transparent page.
 */
async function pinSystemColours(page: Page, scheme: keyof typeof MATERIAL) {
  const count = await page.evaluate(
    ([pinned, material]) => {
      let pinnedCount = 0;

      document.documentElement.style.background = material;

      // StyleX nests its rules in `@layer` blocks.
      const walk = (rules: CSSRuleList) => {
        for (const rule of rules) {
          if (rule instanceof CSSGroupingRule) {
            walk(rule.cssRules);
          } else if (rule instanceof CSSStyleRule) {
            for (const name of rule.style) {
              const value = pinned[rule.style.getPropertyValue(name).trim()];

              if (name.startsWith('--') && value !== undefined) {
                document.documentElement.style.setProperty(name, value);
                pinnedCount += 1;
              }
            }
          }
        }
      };

      for (const sheet of document.styleSheets) {
        walk(sheet.cssRules);
      }

      return pinnedCount;
    },
    [PINNED, MATERIAL[scheme]] as const,
  );

  expect(count, 'the popover tokens were found and pinned').toBeGreaterThan(0);
}

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
  });
}
