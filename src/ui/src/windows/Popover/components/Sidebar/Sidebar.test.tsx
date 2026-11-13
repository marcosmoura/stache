import { DashboardSquare02Icon } from '@hugeicons/core-free-icons';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';
import { userEvent } from 'vitest/browser';

import { Sidebar } from './Sidebar';

const TABS = [
  { id: 'overview', label: 'Overview', icon: DashboardSquare02Icon, badge: null },
  { id: 'bar.toml', label: 'bar.toml', icon: DashboardSquare02Icon, badge: 'invalid' as const },
];

describe('Sidebar', () => {
  it('keeps only the selected tab in the Tab order and selects on click', async () => {
    const onSelect = vi.fn();
    const screen = await render(<Sidebar tabs={TABS} selected="bar.toml" onSelect={onSelect} />);

    await expect
      .element(screen.getByRole('tab', { name: 'bar.toml, invalid' }))
      .toHaveAttribute('tabindex', '0');
    await expect
      .element(screen.getByRole('tab', { name: 'Overview' }))
      .toHaveAttribute('tabindex', '-1');
    await screen.getByRole('tab', { name: 'Overview' }).click();
    expect(onSelect).toHaveBeenLastCalledWith('overview');
  });

  it('ignores keys other than the arrows, Home and End', async () => {
    const onSelect = vi.fn();
    const screen = await render(<Sidebar tabs={TABS} selected="overview" onSelect={onSelect} />);

    (screen.getByRole('tab', { name: 'Overview' }).element() as HTMLElement).focus();
    await userEvent.keyboard('{ArrowRight}a');
    expect(onSelect).not.toHaveBeenCalled();
    await userEvent.keyboard('{ArrowDown}');
    expect(onSelect).toHaveBeenLastCalledWith('bar.toml');
  });

  for (const scheme of ['light', 'dark'] as const) {
    it(`a source-list selection in system colours (${scheme})`, async () => {
      document.documentElement.style.colorScheme = scheme;
      const resolve = (keyword: string) => {
        const probe = document.createElement('span');

        probe.style.color = keyword;
        document.body.append(probe);
        const value = getComputedStyle(probe).color;

        probe.remove();

        return value;
      };
      const screen = await render(<Sidebar tabs={TABS} selected="overview" onSelect={vi.fn()} />);
      const selected = getComputedStyle(
        screen.getByRole('tab', { name: 'Overview' }).element() as HTMLElement,
      );
      const other = getComputedStyle(
        screen.getByRole('tab', { name: 'bar.toml, invalid' }).element() as HTMLElement,
      );

      try {
        expect(selected.backgroundColor).toBe(resolve('-apple-system-control-accent'));
        expect(selected.color).toBe(resolve('-apple-system-alternate-selected-text'));
        expect(other.color).toBe(resolve('-apple-system-secondary-label'));
      } finally {
        document.documentElement.style.colorScheme = '';
      }
    });
  }
});
