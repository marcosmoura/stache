import type { Page } from '@playwright/test';

import { BAR, KEYBINDINGS, TILING } from '../configFixtures';
import { FakeConfig } from '../fakeConfig';
import type { FakeHost } from '../fakeHost';
import { expect, test } from '../fixtures';

/*
 * The settings editor end to end against an in-memory store of real TOML
 * text (`fakeConfig.ts`): realistic `tiling.toml`, `bar.toml` and
 * `keybindings.toml` with comments. Each step edits through the form, then
 * reads the fake file text back: the value changed and every comment that
 * belonged to untouched lines is still there.
 */

const TILING_COMMENTS = [
  '#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/tiling.schema.json',
  '# Tiling for the laptop and the studio display.',
  '# most workspaces tile',
  '# Pixels between windows.',
  '# Coding first.',
  '# Browsing.',
  '# Chat and mail.',
];

function keepsComments(text: string, comments: readonly string[]) {
  for (const comment of comments) {
    expect(text, `comment kept: ${comment}`).toContain(comment);
  }
}

const STATUS = {
  ready: true,
  error: null,
  access: { state: 'open', revoked: false, appPath: null },
  modules: [],
  config: [],
};

async function open(page: Page, host: FakeHost, tab: string) {
  const store = new FakeConfig(host, {
    'global.toml': null,
    'bar.toml': BAR,
    'keybindings.toml': KEYBINDINGS,
    'tiling.toml': TILING,
  });

  host.answerHostStatus(STATUS as never).answer('hide_popover', null);
  await page.setViewportSize({ width: 480, height: 640 });
  await page.goto('/index.html?surface=popover');
  await page.getByRole('tab', { name: tab }).click();
  await expect(page.getByTestId('file-tab')).toBeVisible();

  return store;
}

const field = (page: Page, path: string) => page.locator(`[data-path="${path}"]`).first();

test.describe('Popover settings editor', () => {
  test('tiling: gap, layout enum, workspaces, reset and an out-of-range number', async ({
    page,
    host,
    consoleErrors,
  }) => {
    const store = await open(page, host, 'tiling.toml');

    // The gap: a number written on Enter.
    const inner = field(page, 'gaps.inner').locator('input');

    await expect(inner).toHaveValue('8');
    await inner.fill('10');
    await inner.press('Enter');
    await expect.poll(() => store.text('tiling.toml')).toContain('inner = 10\n');
    keepsComments(store.text('tiling.toml'), TILING_COMMENTS);

    // The layout enum: a floating menu, written at once; the trailing comment stays.
    await page.getByRole('button', { name: 'Default layout', exact: true }).click();
    await page.getByRole('option', { name: /^Master and stack/ }).click();
    await expect
      .poll(() => store.text('tiling.toml'))
      .toContain('default_layout = "master" # most workspaces tile');

    // Add a workspace, move it up, then remove it.
    const workspaces = page.getByRole('region', { name: 'Workspaces' });

    await workspaces.getByRole('button', { name: 'Add workspace' }).click();
    await expect.poll(() => store.text('tiling.toml')).toMatch(/\[\[workspaces\]\]\nname = "new"/);
    await page.getByRole('button', { name: 'Move new up' }).click();
    await expect
      .poll(() => store.text('tiling.toml').indexOf('name = "new"'))
      .toBeLessThan(store.text('tiling.toml').indexOf('name = "comms"'));
    keepsComments(store.text('tiling.toml'), TILING_COMMENTS);
    await expect(workspaces.getByRole('group').nth(2)).toHaveAccessibleName('new');
    await page.getByRole('button', { name: 'Remove new' }).click();
    await expect.poll(() => store.text('tiling.toml')).not.toContain('"new"');
    keepsComments(store.text('tiling.toml'), TILING_COMMENTS);
    await expect(workspaces.getByRole('group')).toHaveCount(3);

    // Reset to default removes the key and marks the field default.
    const ratio = field(page, 'master.ratio');

    await ratio.getByRole('button', { name: 'Reset Master size to default' }).click();
    await expect.poll(() => store.text('tiling.toml')).not.toContain('ratio = 60');
    await expect(ratio.getByText('default', { exact: true })).toBeVisible();
    await expect(ratio.locator('input')).toHaveValue('60');

    // An out-of-range number: inline error, the file unchanged.
    const before = store.text('tiling.toml');

    await ratio.locator('input').fill('-5');
    await ratio.locator('input').press('Enter');
    await expect(ratio.getByRole('alert')).toHaveText('Must be at least 0.');
    await expect(ratio.locator('input')).toHaveValue('-5');
    expect(store.text('tiling.toml')).toBe(before);
    keepsComments(store.text('tiling.toml'), TILING_COMMENTS);
    expect(consoleErrors).toEqual([]);
  });

  test('keybindings: rename and remove a binding', async ({ page, host }) => {
    const store = await open(page, host, 'keybindings.toml');
    const comments = [
      '# Global shortcuts.',
      '# Terminal.',
      '# Browser, then a note.',
      '# two steps',
    ];

    const terminal = page.getByRole('group', { name: 'cmd+shift+t' });

    await terminal.getByRole('textbox', { name: 'Shortcut' }).fill('cmd+alt+t');
    await terminal.getByRole('textbox', { name: 'Shortcut' }).press('Enter');
    await expect
      .poll(() => store.text('keybindings.toml'))
      .toContain('"cmd+alt+t" = "open -a Ghostty"');
    keepsComments(store.text('keybindings.toml'), comments);

    await page.getByRole('button', { name: 'Remove cmd+shift+m' }).click();
    await expect.poll(() => store.text('keybindings.toml')).not.toContain('cmd+shift+m');
    keepsComments(store.text('keybindings.toml'), comments);
    await expect(page.getByRole('group', { name: 'cmd+shift+m' })).toHaveCount(0);
  });

  test('keybindings: add a binding through the inline new row', async ({ page, host }) => {
    const store = await open(page, host, 'keybindings.toml');
    const before = store.text('keybindings.toml');
    const add = page.getByRole('button', { name: 'Add shortcut' });
    const row = page.getByRole('group', { name: 'New shortcut' });

    // Esc cancels: nothing is written and the popover stays open.
    await add.click();
    await expect(row.getByRole('textbox', { name: 'Shortcut' })).toBeFocused();
    await expect(row.getByRole('textbox', { name: 'Shortcut' })).toHaveValue('');
    await page.keyboard.press('Escape');
    await expect(row).toHaveCount(0);
    expect(host.commands().filter((command) => command === 'hide_popover')).toEqual([]);
    expect(store.text('keybindings.toml')).toBe(before);

    // A typed key is written once on Enter, never a placeholder key.
    await add.click();
    await page.keyboard.type('cmd+shift+k');
    await page.keyboard.press('Enter');
    await expect.poll(() => store.text('keybindings.toml')).toContain('"cmd+shift+k" = ""');
    await expect(page.getByRole('group', { name: 'cmd+shift+k' })).toBeVisible();
    await expect(row).toHaveCount(0);
    expect(store.text('keybindings.toml')).not.toContain('new_key');
    keepsComments(store.text('keybindings.toml'), ['# Global shortcuts.', '# Terminal.']);
  });

  test('bar: change the height', async ({ page, host }) => {
    const store = await open(page, host, 'bar.toml');
    const height = field(page, 'height').locator('input');

    await expect(height).toHaveValue('32');
    await height.fill('36');
    await height.blur();
    await expect.poll(() => store.text('bar.toml')).toContain('height = 36 # points');
    keepsComments(store.text('bar.toml'), ['# The status bar.', '# Left to right.']);
  });

  test('an outside save while a field is focused, then an invalid save and its fix', async ({
    page,
    host,
  }) => {
    const store = await open(page, host, 'tiling.toml');
    const outer = field(page, 'gaps.outer').locator('input');

    await outer.fill('20');
    await store.externalWrite('tiling.toml', TILING.replace('outer = 12', 'outer = 16'));
    await expect(field(page, 'gaps.inner').locator('input')).toHaveValue('8');
    await expect(outer).toHaveValue('20');
    await expect(field(page, 'gaps.outer').getByText('Changed on disk')).toBeVisible();
    await field(page, 'gaps.outer').getByRole('button', { name: 'Use disk value' }).click();
    await expect(outer).toHaveValue('16');
    expect(store.text('tiling.toml')).toContain('outer = 16');
    keepsComments(store.text('tiling.toml'), TILING_COMMENTS);

    // An invalid save: diagnostic with its line, the form read-only.
    await store.externalWrite('tiling.toml', TILING.replace('ratio = 60', 'ratio 60'));
    await expect(page.getByRole('alert').filter({ hasText: 'read-only' })).toContainText('Line 12');
    await expect(page.getByTestId('file-tab')).toHaveAttribute('data-state', 'invalid');
    await expect(page.getByRole('button', { name: 'Default layout', exact: true })).toBeDisabled();
    await expect(page.getByRole('tab', { name: 'tiling.toml, invalid' })).toBeVisible();

    // The fix makes it editable again.
    await store.externalWrite('tiling.toml', TILING);
    await expect(page.getByTestId('file-tab')).toHaveAttribute('data-state', 'valid');
    await expect(page.getByRole('button', { name: 'Default layout', exact: true })).toBeEnabled();
    await page.getByRole('switch', { name: 'Enabled' }).last().click();
    await expect.poll(() => store.text('tiling.toml')).toContain('enabled = false');
    keepsComments(store.text('tiling.toml'), TILING_COMMENTS);
  });
});
