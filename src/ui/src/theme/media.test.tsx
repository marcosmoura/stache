import * as stylex from '@stylexjs/stylex';
import { afterEach, describe, expect, it } from 'vitest';
import { commands, page } from 'vitest/browser';

import { render } from '../../tests/render';

import { media } from './tokens.stylex';

/*
 * `media` consts used as StyleX conditional keys: the const is a placeholder at
 * compile time and the plugin substitutes the real query when it writes the CSS.
 */
const probe = stylex.create({
  compact: {
    width: { default: 20, [media.compact]: 10 },
  },
  motion: {
    opacity: { default: 1, [media.reducedMotion]: 0.5 },
  },
});

const read = () => {
  const element = document.querySelector<HTMLElement>('[data-probe]')!;

  return getComputedStyle(element);
};

afterEach(async () => {
  await page.viewport(414, 896);
  await commands.emulateMedia({ reducedMotion: null });
});

describe('media consts as StyleX keys (WebKit)', () => {
  it('media.compact applies at or below 2036px and not above', async () => {
    await render(<div data-probe {...stylex.props(probe.compact)} />);

    await page.viewport(2036, 400);
    await expect.poll(() => read().width).toBe('10px');
    await page.viewport(2037, 400);
    await expect.poll(() => read().width).toBe('20px');
  });

  it('media.reducedMotion applies under emulated reduced motion', async () => {
    await render(<div data-probe {...stylex.props(probe.motion)} />);

    await commands.emulateMedia({ reducedMotion: 'no-preference' });
    await expect.poll(() => read().opacity).toBe('1');
    await commands.emulateMedia({ reducedMotion: 'reduce' });
    await expect.poll(() => read().opacity).toBe('0.5');
  });
});
