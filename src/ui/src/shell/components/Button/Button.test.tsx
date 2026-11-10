import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';
import { userEvent } from 'vitest/browser';

import { Surface } from '../Surface';

import { Button } from './Button';

describe('Button', () => {
  test('defaults to type=button and keeps the legacy geometry', async () => {
    const screen = await render(<Button>Go</Button>);
    const button = screen.container.querySelector('button') as HTMLButtonElement;
    const style = getComputedStyle(button);

    expect(button.type).toBe('button');
    expect(style.display).toBe('grid');
    expect(style.gridAutoFlow).toBe('column');
    expect(style.columnGap).toBe('6px');
    expect(style.paddingLeft).toBe('10px');
    expect(style.paddingRight).toBe('10px');
    expect(style.paddingTop).toBe('0px');
    expect(style.borderTopWidth).toBe('1px');
    expect(style.borderRadius).toBe('12px');
    expect(style.cursor).toBe('pointer');
    expect(style.transitionProperty).toBe('background-color, border-color');
    expect(style.transitionDuration).toBe('0.15s');
    expect(style.transitionTimingFunction).toBe('cubic-bezier(0.42, 0, 0.58, 1)');
    expect(style.borderTopColor).toBe('rgba(0, 0, 0, 0)');
  });

  test('forwards props, ref and clicks', async () => {
    const onClick = vi.fn();
    const ref = { current: null as HTMLButtonElement | null };
    const screen = await render(
      <Button ref={ref} onClick={onClick} className="extra">
        Go
      </Button>,
    );

    await userEvent.click(screen.getByRole('button'));
    expect(onClick).toHaveBeenCalledTimes(1);
    expect(ref.current?.classList.contains('extra')).toBe(true);
  });

  test('hover highlights with surface1 and a base border', async () => {
    const screen = await render(<Button>Go</Button>);
    const button = screen.container.querySelector('button') as HTMLButtonElement;

    await userEvent.hover(screen.getByRole('button'));
    await expect.poll(() => getComputedStyle(button).backgroundColor).toBe('rgb(69, 71, 90)');
    await expect.poll(() => getComputedStyle(button).borderTopColor).toBe('rgb(30, 30, 46)');
  });

  test('active uses the same highlight tokens', async () => {
    // Outside a Surface the Button's own active background applies.
    const screen = await render(<Button active>Go</Button>);
    const button = screen.container.querySelector('button') as HTMLButtonElement;

    expect(getComputedStyle(button).backgroundColor).toBe('rgb(69, 71, 90)');
    expect(getComputedStyle(button).borderTopColor).toBe('rgb(30, 30, 46)');
  });

  test('inside a Surface the rest background is crust and hover wins', async () => {
    const screen = await render(
      <Surface as={Button} animated={false}>
        Go
      </Surface>,
    );
    const button = screen.container.querySelector('button') as HTMLButtonElement;

    expect(getComputedStyle(button).backgroundColor).toBe('rgb(17, 17, 27)');
    await userEvent.hover(screen.getByRole('button'));
    await expect.poll(() => getComputedStyle(button).backgroundColor).toBe('rgb(69, 71, 90)');
  });
});
