import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';
import { userEvent } from 'vitest/browser';

import { Select } from './Select';

const OPTIONS = [
  { value: 'dwindle', title: 'Dwindle', hint: 'Each window splits the last' },
  { value: 'master', title: 'Master and stack', hint: null },
];

describe('Select', () => {
  it('shows the value, opens a menu with the selected item checked and chooses', async () => {
    const onSelect = vi.fn();
    const screen = await render(
      <Select id="layout" label="Layout" value="dwindle" options={OPTIONS} onSelect={onSelect} />,
    );
    const trigger = screen.getByRole('button', { name: 'Layout' });

    await expect.element(trigger).toHaveTextContent('Dwindle');
    await trigger.click();
    await expect
      .element(screen.getByRole('option', { name: /^Dwindle/ }))
      .toHaveAttribute('aria-selected', 'true');
    await expect.element(screen.getByText('Each window splits the last')).toBeVisible();
    await screen.getByRole('option', { name: /^Master/ }).click();
    expect(onSelect).toHaveBeenCalledWith('master');
  });

  it('closes on Esc without choosing', async () => {
    const onSelect = vi.fn();
    const screen = await render(
      <Select id="layout" label="Layout" value="dwindle" options={OPTIONS} onSelect={onSelect} />,
    );

    await screen.getByRole('button', { name: 'Layout' }).click();
    await expect.element(screen.getByRole('listbox')).toBeVisible();
    await userEvent.keyboard('{Escape}');
    await expect.element(screen.getByRole('listbox')).not.toBeInTheDocument();
    expect(onSelect).not.toHaveBeenCalled();
  });
});
