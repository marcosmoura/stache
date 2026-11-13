import { describe, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';

import type { ConfigFileView } from '@/ipc';

import { hostStatusFixture } from '../../../tests/fixtures';
import { flush, mockHost } from '../../../tests/ipc';
import { render } from '../../../tests/render';

import { Popover } from './Popover';

const file = (name: string, overrides: Partial<ConfigFileView> = {}): ConfigFileView => ({
  file: name,
  path: `/config/${name}`,
  state: 'valid',
  active: true,
  diagnostic: null,
  warnings: [],
  ...overrides,
});

const FILES = [
  file('global.toml'),
  file('bar.toml', { state: 'invalid', active: false, diagnostic: 'expected a table' }),
  file('tiling.toml', { diagnostic: 'held: bad gap' }),
  file('wallpapers.toml', { warnings: ['unknown key `foo`'] }),
];

function setup(files: ConfigFileView[] = FILES) {
  return mockHost()
    .answer('config_files', files)
    .answer('hide_popover', null)
    .answer('host_status', hostStatusFixture());
}

describe('Popover', () => {
  it('renders the header and the sidebar with Overview first and badges', async () => {
    setup();
    const screen = await render(<Popover />);

    await expect.element(screen.getByRole('heading', { level: 1, name: 'Stache' })).toBeVisible();
    await expect
      .element(screen.getByRole('tab', { name: 'wallpapers.toml, has warnings' }))
      .toBeVisible();
    const names = screen
      .getByRole('tab')
      .elements()
      .map((tab) => tab.getAttribute('aria-label'));

    expect(names).toEqual([
      'Overview',
      'global.toml',
      'bar.toml, invalid',
      'tiling.toml, has a diagnostic',
      'wallpapers.toml, has warnings',
    ]);
    const badges = screen
      .getByRole('tab')
      .elements()
      .map((tab) => tab.dataset.badge ?? null);

    expect(badges).toEqual([null, null, 'invalid', 'diagnostic', 'warnings']);
    await expect
      .element(screen.getByRole('tab', { name: 'Overview' }))
      .toHaveAttribute('aria-selected', 'true');
    await expect.element(screen.getByRole('tabpanel')).toHaveAccessibleName('Overview');
    await expect.element(screen.getByRole('heading', { level: 2, name: 'Overview' })).toBeVisible();
  });

  it('moves the selection with the arrow keys, wrapping, and Tab enters the pane', async () => {
    setup();
    const screen = await render(<Popover />);

    await expect.element(screen.getByRole('tab', { name: 'global.toml' })).toBeVisible();
    await screen.getByRole('tab', { name: 'Overview' }).click();
    await userEvent.keyboard('{ArrowDown}');
    await expect.element(screen.getByRole('tab', { name: 'global.toml' })).toHaveFocus();
    await expect
      .element(screen.getByRole('heading', { level: 2, name: 'global.toml' }))
      .toBeVisible();
    await userEvent.keyboard('{ArrowUp}{ArrowUp}');
    await expect
      .element(screen.getByRole('tab', { name: 'wallpapers.toml, has warnings' }))
      .toHaveFocus();
    await userEvent.keyboard('{Home}');
    await expect.element(screen.getByRole('tab', { name: 'Overview' })).toHaveFocus();
    await userEvent.keyboard('{End}');
    await expect
      .element(screen.getByRole('tab', { name: 'wallpapers.toml, has warnings' }))
      .toHaveAttribute('aria-selected', 'true');
    // Only the selected tab is in the Tab order, so Tab goes to the pane.
    await userEvent.keyboard('{Tab}');
    await expect.element(screen.getByRole('tabpanel')).toHaveFocus();
  });

  it('remembers the tab when shown again and refetches the files', async () => {
    const host = setup();
    const screen = await render(<Popover />);

    await screen.getByRole('tab', { name: 'tiling.toml, has a diagnostic' }).click();
    await expect.element(screen.getByRole('main')).toHaveAttribute('data-tab', 'tiling.toml');
    const before = host.callsOf('config_files').length;

    // The shell and the open file tab each listen.
    await expect.poll(() => host.listenerCount('popover-shown')).toBe(2);
    host.emit('popover-shown', {});
    await expect.poll(() => host.callsOf('config_files').length).toBe(before + 1);
    await expect
      .element(screen.getByRole('tab', { name: 'tiling.toml, has a diagnostic' }))
      .toHaveFocus();
    await expect.element(screen.getByRole('main')).toHaveAttribute('data-tab', 'tiling.toml');
  });

  it('refetches the files on config-changed and updates the badges', async () => {
    const host = setup();
    const screen = await render(<Popover />);

    await expect.element(screen.getByRole('tab', { name: 'bar.toml, invalid' })).toBeVisible();
    host.answer('config_files', [file('global.toml'), file('bar.toml')]);
    await expect.poll(() => host.listenerCount('config-changed')).toBe(1);
    host.emit('config-changed', { files: ['bar.toml'] });
    await expect.element(screen.getByRole('tab', { name: 'bar.toml' })).toBeVisible();
  });

  it('calls hide_popover on Escape', async () => {
    const host = setup();
    const screen = await render(<Popover />);

    await expect.element(screen.getByRole('tab', { name: 'Overview' })).toBeVisible();
    await userEvent.keyboard('{Escape}');
    await expect.poll(() => host.callsOf('hide_popover').length).toBe(1);
  });

  it('leaves an Escape a child handled alone', async () => {
    const host = setup();
    const screen = await render(
      <>
        <Popover />
        <input
          aria-label="draft"
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.preventDefault();
            }
          }}
        />
      </>,
    );

    await screen.getByRole('textbox', { name: 'draft' }).click();
    await userEvent.keyboard('{Escape}');
    await flush();
    expect(host.callsOf('hide_popover')).toHaveLength(0);
  });
});
