import * as stylex from '@stylexjs/stylex';
import { MotionGlobalConfig } from 'motion/react';
import { afterEach, describe, expect, it } from 'vitest';

import { scrolling } from '@/theme/motion';

import { installReducedMotionQuery, setReducedMotion } from '../../../tests/reducedMotion';
import { render } from '../../../tests/render';

import { ScrollingLabel } from './ScrollingLabel';

const probe = stylex.create({
  width: (px: number) => ({ width: px }),
});

const SHORT = 'Hi';
const MEDIUM = 'A medium title';
const LONG = 'A very long track title that cannot fit in the box';

function Label({ text, width }: { text: string; width: number }) {
  return (
    <div {...stylex.props(probe.width(width))}>
      <ScrollingLabel>{text}</ScrollingLabel>
    </div>
  );
}

const box = () => document.querySelector<HTMLElement>('[data-state]');
const stateOf = () => box()?.dataset.state;
const track = () => box()?.querySelector<HTMLElement>('[data-track]') ?? null;
const text = () => (track()?.firstElementChild as HTMLElement | null | undefined) ?? null;
const clone = () => box()?.querySelector<HTMLElement>('[data-clone]') ?? null;
const offsetX = () => {
  const element = track();

  return element ? new DOMMatrix(getComputedStyle(element).transform).m41 : Number.NaN;
};
const maskOf = () => {
  const element = box();

  return element
    ? getComputedStyle(element).webkitMaskImage || getComputedStyle(element).maskImage
    : '';
};

type Sample = {
  at: number;
  x: number;
  box: DOMRect;
  first: DOMRect;
  second: DOMRect | null;
};

/** Samples the layout every frame for `ms` milliseconds; stops when the label is gone. */
async function sample(ms: number) {
  const samples: Sample[] = [];
  const started = performance.now();

  while (performance.now() - started < ms) {
    const boxElement = box();
    const first = text();

    if (!boxElement || !first) {
      break;
    }

    samples.push({
      at: performance.now() - started,
      x: offsetX(),
      box: boxElement.getBoundingClientRect(),
      first: first.getBoundingClientRect(),
      second: clone()?.getBoundingClientRect() ?? null,
    });
    await new Promise((resolve) => requestAnimationFrame(resolve));
  }

  return samples;
}

const intersects = (rect: DOMRect | null, area: DOMRect) =>
  rect !== null && rect.right > area.left + 0.5 && rect.left < area.right - 0.5;

installReducedMotionQuery();

afterEach(() => {
  setReducedMotion(false);
});

describe('ScrollingLabel (real layout)', () => {
  it('is static, unmasked and unmoved when the text fits', async () => {
    await render(<Label text={SHORT} width={200} />);

    await expect.poll(stateOf).toBe('static');
    expect(offsetX()).toBe(0);
    expect(maskOf()).toBe('none');
    expect(clone()).toBeNull();
  });

  it('masks both edges only while scrolling', async () => {
    const screen = await render(<Label text={LONG} width={80} />);

    await expect.poll(stateOf).toBe('scrolling');
    expect(maskOf()).toContain('linear-gradient');
    await screen.rerender(<Label text={SHORT} width={80} />);
    await expect.poll(stateOf).toBe('static');
    expect(maskOf()).toBe('none');
  });

  it('enters from the right and is followed by a clone scrolling.gap box widths behind, forever', async () => {
    MotionGlobalConfig.skipAnimations = false;
    await render(<Label text={MEDIUM} width={80} />);

    await expect.poll(stateOf).toBe('scrolling');
    expect(clone()).not.toBeNull();
    expect(clone()!.getAttribute('aria-hidden')).toBe('true');
    expect(box()!.textContent).toBe(MEDIUM);
    await expect.poll(offsetX).toBeGreaterThan(0);

    const boxWidth = box()!.getBoundingClientRect().width;
    const textWidth = text()!.getBoundingClientRect().width;
    const gap = boxWidth * scrolling.gap;
    const period = textWidth + gap;
    const intro = boxWidth / scrolling.speed;
    const samples = await sample((intro + (2 * period) / scrolling.speed) * 1000 + 300);

    // Starts just past the right edge (the first frames may already have moved).
    expect(samples[0]!.first.left - samples[0]!.box.left).toBeGreaterThan(boxWidth - 10);
    expect(samples[0]!.first.left - samples[0]!.box.left).toBeLessThanOrEqual(boxWidth + 0.5);

    let wraps = 0;

    for (const [i, s] of samples.entries()) {
      // The clone always follows the text by exactly the gap.
      expect(s.second).not.toBeNull();
      expect(s.second!.left).toBeCloseTo(s.first.right + gap, 0);

      // After the intro, some copy is always in the box.
      if (s.at > intro * 1000 + 100) {
        expect(intersects(s.first, s.box) || intersects(s.second, s.box)).toBe(true);
      }

      if (i === 0) {
        continue;
      }

      const previous = samples[i - 1]!;

      if (s.x > previous.x + 1) {
        // A wrap jumps by exactly one period, so the clone lands where the text was.
        wraps++;
        const step = previous.x - s.x + period;
        const moved = ((s.at - previous.at) / 1000) * scrolling.speed;

        expect(Math.abs(step)).toBeLessThanOrEqual(moved + 1);
        expect(previous.second!.left - s.first.left).toBeLessThanOrEqual(moved + 1);
        expect(previous.second!.left - s.first.left).toBeGreaterThanOrEqual(-1);
      } else {
        // Otherwise it only ever moves left.
        expect(s.x).toBeLessThanOrEqual(previous.x + 0.5);
      }
    }

    expect(wraps).toBeGreaterThanOrEqual(1);
  });

  it('moves at about the configured speed', async () => {
    MotionGlobalConfig.skipAnimations = false;
    await render(<Label text={LONG} width={80} />);

    await expect.poll(stateOf).toBe('scrolling');
    await expect.poll(offsetX).toBeGreaterThan(0);
    const samples = await sample(800);
    const first = samples[0]!;
    const last = samples.at(-1)!;
    const speed = (first.x - last.x) / ((last.at - first.at) / 1000);

    expect(speed).toBeGreaterThan(scrolling.speed * 0.8);
    expect(speed).toBeLessThan(scrolling.speed * 1.2);
  });

  it('restarts cleanly from the right edge when the text or width changes', async () => {
    MotionGlobalConfig.skipAnimations = false;
    const screen = await render(<Label text={MEDIUM} width={80} />);

    await expect.poll(stateOf).toBe('scrolling');
    await sample(500);
    await screen.rerender(<Label text={LONG} width={80} />);
    await expect.poll(() => text()?.textContent).toBe(LONG);
    await expect.poll(offsetX).toBeGreaterThan(60);
    let samples = await sample(100);

    expect(samples[0]!.first.left - samples[0]!.box.left).toBeGreaterThan(80 - 10);
    expect(clone()!.dataset.text).toBe(LONG);

    await sample(500);
    await screen.rerender(<Label text={LONG} width={120} />);
    await expect.poll(() => box()!.getBoundingClientRect().width).toBeGreaterThan(100);
    await expect.poll(offsetX).toBeGreaterThan(100);
    samples = await sample(100);
    const boxWidth = samples[0]!.box.width;

    expect(samples[0]!.first.left - samples[0]!.box.left).toBeGreaterThan(boxWidth - 10);
    expect(samples[0]!.second!.left).toBeCloseTo(
      samples[0]!.first.right + boxWidth * scrolling.gap,
      0,
    );
  });

  it('stops animating on static and after unmount', async () => {
    MotionGlobalConfig.skipAnimations = false;
    const screen = await render(<Label text={LONG} width={80} />);

    await expect.poll(stateOf).toBe('scrolling');
    await sample(300);
    await screen.rerender(<Label text={SHORT} width={80} />);
    await expect.poll(stateOf).toBe('static');
    await sample(100);
    expect(offsetX()).toBe(0);
    expect(clone()).toBeNull();

    await screen.rerender(<Label text={LONG} width={80} />);
    await expect.poll(stateOf).toBe('scrolling');
    const element = track()!;

    screen.unmount();
    const before = element.style.transform;

    await new Promise((resolve) => setTimeout(resolve, 300));
    expect(element.style.transform).toBe(before);
  });

  it('re-measures when the text changes', async () => {
    const screen = await render(<Label text={SHORT} width={80} />);

    await expect.poll(stateOf).toBe('static');
    await screen.rerender(<Label text={LONG} width={80} />);
    await expect.poll(stateOf).toBe('scrolling');
    await screen.rerender(<Label text={SHORT} width={80} />);
    await expect.poll(stateOf).toBe('static');
  });

  it('re-measures when the box width changes', async () => {
    const screen = await render(<Label text={LONG} width={80} />);

    await expect.poll(stateOf).toBe('scrolling');
    await screen.rerender(<Label text={LONG} width={2000} />);
    await expect.poll(stateOf).toBe('static');
    await screen.rerender(<Label text={LONG} width={60} />);
    await expect.poll(stateOf).toBe('scrolling');
  });

  it('truncates with an ellipsis instead of scrolling under reduced motion', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(true);
    await render(<Label text={LONG} width={80} />);

    await expect.poll(stateOf).toBe('truncated');
    expect(getComputedStyle(box()!).textOverflow).toBe('ellipsis');
    expect(maskOf()).toBe('none');
    expect(clone()).toBeNull();
    await new Promise((resolve) => setTimeout(resolve, 1500));
    expect(offsetX()).toBe(0);
  });
});
