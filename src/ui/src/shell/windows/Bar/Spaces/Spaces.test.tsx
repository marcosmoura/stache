import { describe, expect, test } from 'vitest';

import { renderWithBarData } from '../../../../../tests/renderBar';

import { Spaces, SpacesFallback } from './Spaces';

describe('Spaces', () => {
  test('keeps the empty first-grid-slot div while tiling is absent', async () => {
    const { screen } = await renderWithBarData(<Spaces />);
    const slot = screen.container.firstElementChild;

    expect(slot?.tagName).toBe('DIV');
    expect(slot?.childElementCount).toBe(0);
    expect(screen.container.querySelector('[data-testid="spaces-container"]')).toBeNull();
  });

  test('the legacy Loading... fallback keeps its inline-flex surface (unreachable in production)', async () => {
    const { screen } = await renderWithBarData(<SpacesFallback />);
    const surface = screen.container.firstElementChild?.firstElementChild;

    await expect.element(screen.getByText('Loading...')).toBeVisible();

    if (!surface) {
      throw new Error('fallback missing');
    }

    const style = getComputedStyle(surface);

    // `inline-flex` is blockified because the Stack grid makes the surface a grid item.
    expect(style.display).toBe('flex');
    expect(style.alignItems).toBe('center');
    expect(style.paddingLeft).toBe('10px');
    expect(style.paddingRight).toBe('10px');
  });
});
