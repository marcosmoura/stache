import * as stylex from '@stylexjs/stylex';
import { MotionGlobalConfig } from 'motion/react';
import { describe, expect, it, vi } from 'vitest';
import { commands, userEvent } from 'vitest/browser';

import { color } from '@/theme/tokens.stylex';

import { render } from '../../../../../tests/render';

import { BarItem } from './BarItem';

const probe = stylex.create({
  frame: { height: 32 },
  swatch: (value: string) => ({ color: value }),
});

/** The computed colors of the tokens an item uses, in the current scheme. */
function Swatches() {
  return (
    <>
      <i data-swatch="rest" {...stylex.props(probe.swatch(color.surfaceSunken))} />
      <i data-swatch="hover" {...stylex.props(probe.swatch(color.surfaceActive))} />
      <i data-swatch="border" {...stylex.props(probe.swatch(color.surface))} />
      <i data-swatch="focus" {...stylex.props(probe.swatch(color.accent))} />
    </>
  );
}

const swatch = (name: string) =>
  getComputedStyle(document.querySelector(`[data-swatch="${name}"]`)!).color;

const looks = (element: Element) => {
  const style = getComputedStyle(element);

  return { background: style.backgroundColor, border: style.borderTopColor };
};

async function renderItems() {
  return render(
    <div {...stylex.props(probe.frame)}>
      <Swatches />
      <BarItem>static</BarItem>
      <BarItem onClick={() => {}}>click</BarItem>
      <BarItem onClick={() => {}} active>
        on
      </BarItem>
    </div>,
  );
}

describe('BarItem', () => {
  it('is a div without onClick and a button with it', async () => {
    const onClick = vi.fn();
    const screen = await render(
      <>
        <BarItem>plain</BarItem>
        <BarItem onClick={onClick}>press</BarItem>
      </>,
    );

    expect(screen.getByText('plain').element().tagName).toBe('DIV');
    expect(screen.getByRole('button')).toHaveTextContent('press');
    await screen.getByRole('button').click();
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it('names icon-only items with label', async () => {
    const screen = await render(
      <>
        <BarItem label="Battery" onClick={() => {}} />
        <BarItem label="Keep Awake" />
      </>,
    );

    await expect.element(screen.getByRole('button', { name: 'Battery' })).toBeVisible();
    await expect.element(screen.getByRole('group', { name: 'Keep Awake' })).toBeInTheDocument();
  });

  it('fills the Bar height', async () => {
    const screen = await renderItems();

    expect(screen.getByText('static').element().getBoundingClientRect().height).toBe(32);
  });

  for (const scheme of ['light', 'dark'] as const) {
    it(`shows rest, hover and active looks from tokens (${scheme})`, async () => {
      await commands.emulateMedia({ colorScheme: scheme });
      const screen = await renderItems();
      const rest = { background: swatch('rest'), border: 'rgba(0, 0, 0, 0)' };
      const on = { background: swatch('hover'), border: swatch('border') };

      expect(rest.background).not.toBe(on.background);
      await expect.poll(() => looks(screen.getByText('static').element())).toEqual(rest);
      await expect.poll(() => looks(screen.getByText('click').element())).toEqual(rest);
      await expect.poll(() => looks(screen.getByText('on').element())).toEqual(on);

      await userEvent.hover(screen.getByText('click'));
      await expect.poll(() => looks(screen.getByText('click').element())).toEqual(on);

      // A static item has no hover look.
      await userEvent.hover(screen.getByText('static'));
      await expect.poll(() => looks(screen.getByText('static').element())).toEqual(rest);
    });
  }

  it('shows a visible focus ring for keyboard focus only', async () => {
    const screen = await renderItems();
    const button = screen.getByText('click').element() as HTMLButtonElement;

    expect(getComputedStyle(button).outlineStyle).toBe('none');
    /*
     * WebKit's Tab reaches buttons only when macOS "Keyboard navigation" is on,
     * so the button is focused after a real pointer event (no ring) and then
     * receives a real key press, which turns that focus into keyboard focus.
     */
    await userEvent.click(button);
    button.focus();
    expect(document.activeElement).toBe(button);
    expect(getComputedStyle(button).outlineStyle).toBe('none');

    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(button);
    await expect.poll(() => getComputedStyle(button).outlineStyle).toBe('solid');
    expect(getComputedStyle(button).outlineColor).toBe(swatch('focus'));
  });

  it('transitions only paint properties on hover', async () => {
    const screen = await renderItems();
    const style = getComputedStyle(screen.getByText('click').element());

    expect(style.transitionProperty).toBe('background-color, border-color');
    expect(style.transitionDuration).toBe('0.15s');
  });

  it('fades in once on mount (opacity only)', async () => {
    MotionGlobalConfig.skipAnimations = false;
    const screen = await render(<BarItem>fade</BarItem>);
    const element = screen.getByText('fade').element() as HTMLElement;

    expect(Number(getComputedStyle(element).opacity)).toBeLessThan(1);
    expect(element.style.transform).toBe('');
    await expect.poll(() => getComputedStyle(element).opacity, { timeout: 2000 }).toBe('1');

    await screen.rerender(<BarItem>fade</BarItem>);
    expect(getComputedStyle(element).opacity).toBe('1');
  });
});
