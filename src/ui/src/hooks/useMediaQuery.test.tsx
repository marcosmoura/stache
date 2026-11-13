import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import { render } from '../../tests/render';

import { COMPACT_QUERY, mediaQueryOf, useCompact, useMediaQuery } from './useMediaQuery';

function Probe({ query }: { query: string }) {
  return <output data-testid="matches">{String(useMediaQuery(query))}</output>;
}

function CompactProbe() {
  return <output data-testid="compact">{String(useCompact())}</output>;
}

afterEach(async () => {
  vi.restoreAllMocks();
  await page.viewport(414, 896);
});

describe('useMediaQuery', () => {
  it('derives the compact query from the StyleX const', () => {
    expect(COMPACT_QUERY).toBe('(width <= 2036px)');
    expect(mediaQueryOf('@media (prefers-contrast: more)')).toBe('(prefers-contrast: more)');
  });

  it('follows the viewport across the compact breakpoint', async () => {
    await page.viewport(2036, 400);
    const screen = await render(<CompactProbe />);

    await expect.element(screen.getByTestId('compact')).toHaveTextContent('true');
    await page.viewport(2037, 400);
    await expect.element(screen.getByTestId('compact')).toHaveTextContent('false');
    await page.viewport(1440, 400);
    await expect.element(screen.getByTestId('compact')).toHaveTextContent('true');
  });

  it('subscribes once per query and unsubscribes on unmount', async () => {
    const list = Object.assign(new EventTarget(), { matches: false, media: '(test: 1)' });
    const add = vi.spyOn(list, 'addEventListener');
    const remove = vi.spyOn(list, 'removeEventListener');
    const real = window.matchMedia.bind(window);

    vi.spyOn(window, 'matchMedia').mockImplementation((query) =>
      query === '(test: 1)' ? (list as unknown as MediaQueryList) : real(query),
    );

    const screen = await render(<Probe query="(test: 1)" />);

    await expect.element(screen.getByTestId('matches')).toHaveTextContent('false');
    list.matches = true;
    list.dispatchEvent(new Event('change'));
    await expect.element(screen.getByTestId('matches')).toHaveTextContent('true');
    expect(add).toHaveBeenCalledTimes(1);

    await screen.unmount();
    expect(remove).toHaveBeenCalledTimes(1);
  });
});
