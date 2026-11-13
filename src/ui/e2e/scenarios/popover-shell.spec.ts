import type { ConfigFileView_Serialize } from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/** The Overview footer names the app from `app_info`. */
const APP = { name: 'Stache', version: '0.1.0', identifier: 'com.marcosmoura.stache' };

/*
 * The tray popover shell (`?surface=popover`) through the fake host: the
 * sidebar with badges from `config_files`, keyboard navigation, the tab kept
 * across a `popover-shown`, a `config-changed` refetch and Escape calling
 * `hide_popover`. It may call only `app_info` and `host_status` (the
 * Overview), `config_files`, `config_document` (a file tab), `hide_popover` and the event listen.
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

const POPOVER_FILES: ConfigFileView_Serialize[] = [
  file('global.toml'),
  file('bar.toml'),
  file('commandQuit.toml', { state: 'missing', active: false }),
  file('keepAwake.toml'),
  file('keybindings.toml', { warnings: ['unknown key `foo`'] }),
  file('menuAnywhere.toml'),
  file('notunes.toml'),
  file('proxyAudio.toml'),
  file('tiling.toml', { state: 'invalid', active: false, diagnostic: 'expected a table' }),
  file('wallpapers.toml', { diagnostic: 'held: unknown mode' }),
];

const ALLOWED = new Set([
  'app_info',
  'config_files',
  'host_status',
  // A selected file tab reads its document (the settings editor).
  'config_document',
  'hide_popover',
  'plugin:event|listen',
  'plugin:event|unlisten',
]);

test.describe('Popover shell', () => {
  test('sidebar, keyboard, remembered tab, refetch and Escape', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host
      .answer('app_info', APP)
      .answer('config_files', POPOVER_FILES)
      .answer('config_document', (args: unknown) => ({
        status: 'unknownFile',
        file: (args as { file: string }).file,
      }))
      .answer('hide_popover', null)
      .answerHostStatus({
        ready: true,
        error: null,
        access: { state: 'open', revoked: false, appPath: null },
        modules: [],
        config: [],
      });
    await page.goto('/?surface=popover');

    await expect(page.getByRole('heading', { level: 1, name: 'Stache' })).toBeVisible();
    const tabs = page.getByRole('tab');

    await expect(tabs).toHaveCount(11);
    await expect(tabs.first()).toHaveAccessibleName('Overview');
    await expect(page.getByRole('tab', { name: 'tiling.toml, invalid' })).toBeVisible();
    await expect(
      page.getByRole('tab', { name: 'wallpapers.toml, has a diagnostic' }),
    ).toBeVisible();
    await expect(page.getByRole('tab', { name: 'keybindings.toml, has warnings' })).toBeVisible();
    await expect(page.getByRole('heading', { level: 2, name: 'Overview' })).toBeVisible();

    // Keyboard: ↓ twice from Overview selects bar.toml, ↑ wraps from the top.
    await tabs.first().focus();
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    await expect(page.getByRole('tab', { name: 'bar.toml' })).toBeFocused();
    await expect(page.getByRole('heading', { level: 2, name: 'bar.toml' })).toBeVisible();
    await page.keyboard.press('Home');
    await page.keyboard.press('ArrowUp');
    await expect(
      page.getByRole('tab', { name: 'wallpapers.toml, has a diagnostic' }),
    ).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('tabpanel')).toBeFocused();

    // Shown again: the tab is kept and the files are refetched.
    await page.getByRole('tab', { name: 'proxyAudio.toml' }).click();
    const before = host.commands().filter((command) => command === 'config_files').length;

    await host.emit('popover-shown', {});
    await expect
      .poll(() => host.commands().filter((command) => command === 'config_files').length)
      .toBeGreaterThan(before);
    await expect(page.getByRole('tab', { name: 'proxyAudio.toml' })).toBeFocused();
    await expect(page.getByRole('heading', { level: 2, name: 'proxyAudio.toml' })).toBeVisible();

    // A save fixes tiling.toml: the badge goes away.
    host.answer(
      'config_files',
      POPOVER_FILES.map((entry) => (entry.file === 'tiling.toml' ? file('tiling.toml') : entry)),
    );
    await host.emit('config-changed', { files: ['tiling.toml'] });
    await expect(page.getByRole('tab', { name: 'tiling.toml', exact: true })).toBeVisible();

    await page.keyboard.press('Escape');
    await expect.poll(() => host.commands().filter((c) => c === 'hide_popover').length).toBe(1);

    expect(host.commands().filter((command) => !ALLOWED.has(command))).toEqual([]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });
});
