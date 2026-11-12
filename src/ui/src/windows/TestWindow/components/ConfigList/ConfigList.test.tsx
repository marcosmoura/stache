import { describe, expect, it } from 'vitest';

import type { ConfigFileView_Serialize } from '@/ipc';

import { render } from '../../../../../tests/render';

import { ConfigList } from './ConfigList';

const file = (overrides: Partial<ConfigFileView_Serialize> = {}): ConfigFileView_Serialize => ({
  file: 'global.toml',
  path: '/Users/me/.config/stache/global.toml',
  state: 'valid',
  active: true,
  diagnostic: null,
  warnings: [],
  ...overrides,
});

describe('ConfigList', () => {
  it('says when there is no configuration file', async () => {
    const screen = await render(<ConfigList files={[]} />);

    await expect.element(screen.getByText('No configuration files.')).toBeVisible();
  });

  it('shows each file with its state, path and whether it is active', async () => {
    const screen = await render(
      <ConfigList
        files={[
          file(),
          file({ file: 'bar.toml', path: '/x/bar.toml', state: 'missing', active: false }),
        ]}
      />,
    );
    const cards = screen.getByRole('listitem').elements();

    expect(cards[0]!.querySelector('h3')?.textContent).toBe('global.toml');
    expect(cards[0]!.querySelector('[data-state]')?.textContent).toBe('Valid');
    expect(cards[0]!.textContent).toContain('Active configuration');
    expect(cards[1]!.querySelector('[data-state]')?.textContent).toBe('Missing');
    expect(cards[1]!.textContent).toContain('Not part of the active configuration');
    await expect.element(screen.getByText('/x/bar.toml')).toBeVisible();
  });

  it('labels the file diagnostic and the warnings separately', async () => {
    const screen = await render(
      <ConfigList
        files={[
          file({
            state: 'invalid',
            diagnostic: 'expected a table',
            warnings: ['unknown key `a`', 'unknown key `b`'],
          }),
        ]}
      />,
    );

    await expect.element(screen.getByText('Invalid')).toBeVisible();
    expect(
      screen.getByRole('heading', { name: 'File diagnostic' }).element().nextElementSibling
        ?.textContent,
    ).toBe('expected a table');
    const warnings = screen
      .getByRole('heading', { name: 'Warnings' })
      .element().nextElementSibling!;

    expect(Array.from(warnings.querySelectorAll('li'), (item) => item.textContent)).toEqual([
      'unknown key `a`',
      'unknown key `b`',
    ]);
  });
});
