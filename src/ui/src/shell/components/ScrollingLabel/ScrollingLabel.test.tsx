import { describe, expect, test } from 'vitest';
import { render } from 'vitest-browser-react';

import { ScrollingLabel } from './ScrollingLabel';

describe('ScrollingLabel', () => {
  test('does not scroll when the label fits', async () => {
    const screen = await render(
      <div style={{ width: 300 }}>
        <ScrollingLabel>short</ScrollingLabel>
      </div>,
    );
    const label = screen.container.querySelector('span') as HTMLElement;

    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(getComputedStyle(label).animationName).toBe('none');
    expect(getComputedStyle(label).whiteSpace).toBe('nowrap');
  });

  test('scrolls an overflowing label with the legacy duration and edge fades', async () => {
    const screen = await render(
      <div style={{ width: 100 }}>
        <ScrollingLabel>{'a very long label that cannot fit in one hundred pixels'}</ScrollingLabel>
      </div>,
    );
    const wrapper = screen.container.firstElementChild?.firstElementChild as HTMLElement;
    const label = screen.container.querySelector('span') as HTMLElement;

    await expect.poll(() => getComputedStyle(label).animationName).not.toBe('none');

    const duration = Number.parseFloat(label.style.getPropertyValue('--scroll-duration'));
    const start = Number.parseFloat(label.style.getPropertyValue('--scroll-start'));
    const end = Number.parseFloat(label.style.getPropertyValue('--scroll-end'));

    expect(start).toBe(wrapper.offsetWidth);
    expect(end).toBeLessThan(-wrapper.offsetWidth);
    expect(duration).toBeCloseTo(Math.max(1, 1 + (start - end) / 60), 5);
    expect(getComputedStyle(label).animationIterationCount).toBe('infinite');
    expect(getComputedStyle(label).animationTimingFunction).toBe('linear');
    expect(getComputedStyle(wrapper).marginLeft).toBe('-4px');
  });
});
