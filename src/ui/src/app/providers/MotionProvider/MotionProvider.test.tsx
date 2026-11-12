import { ErrorBoundary } from 'react-error-boundary';

// oxlint-disable-next-line no-restricted-imports -- proves strict mode rejects the full component
import { MotionGlobalConfig, motion } from 'motion/react';
import * as m from 'motion/react-m';
import { describe, expect, it } from 'vitest';

import { setReducedMotion, installReducedMotionQuery } from '../../../../tests/reducedMotion';
import { render } from '../../../../tests/render';

const transformOf = (element: Element | null) =>
  element ? getComputedStyle(element).transform : 'missing';

/** The element's current horizontal translation in pixels. */
const translateX = (element: Element | null) => {
  const transform = transformOf(element);

  if (transform === 'none' || transform === 'missing') {
    return 0;
  }

  return new DOMMatrixReadOnly(transform).m41;
};

installReducedMotionQuery();

describe('MotionProvider', () => {
  it('animates an m component', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(false);
    const screen = await render(
      <m.div
        data-testid="box"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: 0.05 }}
      />,
    );
    const box = screen.getByTestId('box').element();

    await expect.poll(() => getComputedStyle(box).opacity).toBe('1');
  });

  it('throws when a full motion component renders inside it (strict)', async () => {
    const screen = await render(
      <ErrorBoundary fallbackRender={({ error }) => <p role="alert">{String(error)}</p>}>
        <motion.div />
      </ErrorBoundary>,
    );

    const alert = screen.getByRole('alert');

    await expect.element(alert).toBeInTheDocument();
    expect(alert.element().textContent).toContain('render a `m` component');
  });

  it('animates transforms over time without reduced motion', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(false);
    const screen = await render(
      <m.div
        data-testid="box"
        initial={{ x: 0 }}
        animate={{ x: 100 }}
        transition={{ duration: 30, ease: 'linear' }}
      />,
    );
    const box = screen.getByTestId('box').element();

    await expect.poll(() => translateX(box)).toBeGreaterThan(0);
    expect(translateX(box)).toBeLessThan(100);
  });
});

describe('MotionProvider with reduced motion requested', () => {
  it('skips transform animations', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(true);
    const screen = await render(
      <m.div
        data-testid="box"
        initial={{ x: 0 }}
        animate={{ x: 100 }}
        transition={{ duration: 30, ease: 'linear' }}
      />,
    );
    const box = screen.getByTestId('box').element();

    await expect.poll(() => translateX(box)).toBe(100);
  });

  it('still animates opacity', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(true);
    const screen = await render(
      <m.div
        data-testid="fade"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: 30, ease: 'linear' }}
      />,
    );
    const fade = screen.getByTestId('fade').element();

    await expect.poll(() => Number(getComputedStyle(fade).opacity)).toBeGreaterThan(0);
    expect(Number(getComputedStyle(fade).opacity)).toBeLessThan(1);
  });

  it('animates transforms again when the setting is turned off', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(false);
    const screen = await render(
      <m.div
        data-testid="box"
        initial={{ x: 0 }}
        animate={{ x: 100 }}
        transition={{ duration: 30, ease: 'linear' }}
      />,
    );
    const box = screen.getByTestId('box').element();

    await expect.poll(() => translateX(box)).toBeGreaterThan(0);
    expect(translateX(box)).toBeLessThan(100);
  });
});
