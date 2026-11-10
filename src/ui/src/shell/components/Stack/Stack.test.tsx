import { afterEach, describe, expect, test } from 'vitest';
import { render } from 'vitest-browser-react';
import { page } from 'vitest/browser';

import { Stack } from './Stack';

describe('Stack', () => {
  test('is a one-row column-flow grid with the compact gap on laptop widths', async () => {
    const screen = await render(
      <Stack data-testid="stack" className="extra">
        <span>a</span>
        <span>b</span>
      </Stack>,
    );
    const stack = screen.container.firstElementChild as HTMLElement;
    const style = getComputedStyle(stack);

    expect(stack.dataset.testid).toBe('stack');
    expect(stack.classList.contains('extra')).toBe(true);
    expect(style.display).toBe('grid');
    expect(style.gridAutoFlow).toBe('column');
    // The test viewport is below 2036px: compact gap 2px (4px above it).
    expect(window.matchMedia('(width <= 2036px)').matches).toBe(true);
    expect(style.columnGap).toBe('2px');
  });
});

describe('Stack on a wide display', () => {
  afterEach(async () => {
    await page.viewport(414, 896);
  });

  test('uses the regular 4px gap above 2036px', async () => {
    await page.viewport(2560, 900);

    const screen = await render(<Stack>content</Stack>);
    const stack = screen.container.firstElementChild as HTMLElement;

    expect(window.matchMedia('(width <= 2036px)').matches).toBe(false);
    expect(getComputedStyle(stack).columnGap).toBe('4px');
  });
});
