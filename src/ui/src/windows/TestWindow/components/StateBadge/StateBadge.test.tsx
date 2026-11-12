import { describe, expect, it } from 'vitest';

import { render } from '../../../../../tests/render';

import { StateBadge } from './StateBadge';

describe('StateBadge', () => {
  it('shows its label, carries the raw state and tints by tone', async () => {
    const screen = await render(
      <>
        <StateBadge state="running">Running</StateBadge>
        <StateBadge state="disabled">Disabled</StateBadge>
      </>,
    );
    const running = screen.getByText('Running').element();
    const disabled = screen.getByText('Disabled').element();

    expect(running).toHaveAttribute('data-state', 'running');
    await expect.poll(() => getComputedStyle(running).display).toBe('inline-flex');
    expect(getComputedStyle(running).color).not.toBe(getComputedStyle(disabled).color);
  });
});
