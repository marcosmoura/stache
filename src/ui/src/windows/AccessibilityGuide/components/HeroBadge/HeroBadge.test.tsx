import { Loading03Icon, ShieldKeyIcon } from '@hugeicons/core-free-icons';
import { describe, expect, it } from 'vitest';

import { render } from '../../../../../tests/render';

import { HeroBadge } from './HeroBadge';

describe('HeroBadge', () => {
  it('takes its color from the tone and grows when large', async () => {
    const screen = await render(
      <>
        <HeroBadge tone="accent" icon={ShieldKeyIcon} />
        <HeroBadge tone="success" icon={ShieldKeyIcon} large />
      </>,
    );
    const [accent, success] = Array.from(
      screen.container.querySelectorAll<HTMLElement>('[data-tone]'),
    );

    await expect.poll(() => getComputedStyle(accent!).width).toBe('44px');
    expect(getComputedStyle(success!).width).toBe('64px');
    expect(getComputedStyle(accent!).color).not.toBe(getComputedStyle(success!).color);
    expect(accent!.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
  });

  it('pulses and spins only when neutral', async () => {
    const screen = await render(<HeroBadge tone="neutral" icon={Loading03Icon} large />);
    const badge = screen.container.querySelector<HTMLElement>('[data-tone="neutral"]')!;

    await expect.poll(() => getComputedStyle(badge).animationIterationCount).toBe('infinite');
    expect(getComputedStyle(badge.firstElementChild!).animationIterationCount).toBe('infinite');
  });
});
