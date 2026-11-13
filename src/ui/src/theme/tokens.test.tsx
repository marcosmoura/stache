import * as stylex from '@stylexjs/stylex';
import { describe, expect, it } from 'vitest';
import { commands } from 'vitest/browser';

import { render } from '../../tests/render';

import { latte, mocha } from './palette.stylex';
import { bar, color, media } from './tokens.stylex';

const probe = stylex.create({
  swatch: (value: string) => ({ backgroundColor: value }),
});

/** One token of every color role and the palette entry it must resolve to. */
type ColorRole =
  | 'surface'
  | 'surfaceRaised'
  | 'surfaceSunken'
  | 'surfaceHover'
  | 'surfaceActive'
  | 'text'
  | 'textMuted'
  | 'textSubtle'
  | 'textSecondary'
  | 'textFaint'
  | 'border'
  | 'divider'
  | 'focus'
  | 'attention'
  | 'accent'
  | 'onAccent'
  | 'success'
  | 'warning'
  | 'danger'
  | 'info'
  | 'highlight';

type Swatch = keyof typeof latte & keyof typeof mocha;

const roles: ReadonlyArray<[ColorRole, Swatch, Swatch?]> = [
  ['surface', 'base'],
  ['surfaceRaised', 'mantle'],
  ['surfaceSunken', 'crust'],
  ['surfaceHover', 'surface0'],
  ['surfaceActive', 'surface1'],
  ['text', 'text'],
  ['textMuted', 'subtext0'],
  ['textSubtle', 'overlay1'],
  ['textSecondary', 'subtext1'],
  ['textFaint', 'overlay2'],
  ['border', 'surface1'],
  ['divider', 'surface0'],
  ['focus', 'lavender'],
  ['attention', 'peach'],
  ['accent', 'mauve'],
  ['success', 'green'],
  ['warning', 'yellow'],
  ['danger', 'red'],
  ['info', 'blue'],
  ['highlight', 'sky'],
  ['onAccent', 'base', 'crust'],
];

const rgb = (hex: string) => {
  const value = Number.parseInt(hex.slice(1), 16);

  return `rgb(${(value >> 16) & 255}, ${(value >> 8) & 255}, ${value & 255})`;
};

function Swatches() {
  return (
    <div>
      {roles.map(([role]) => (
        <div key={role} data-role={role} {...stylex.props(probe.swatch(color[role]))} />
      ))}
    </div>
  );
}

const computed = (container: HTMLElement) =>
  Object.fromEntries(
    roles.map(([role]) => {
      const element = container.querySelector(`[data-role="${role}"]`);

      return [role, element ? getComputedStyle(element).backgroundColor : 'missing'];
    }),
  );

const expected = (scheme: 'latte' | 'mocha') =>
  Object.fromEntries(
    roles.map(([role, light, dark = light]) => [
      role,
      rgb(scheme === 'latte' ? latte[light] : mocha[dark]),
    ]),
  );

describe('semantic color tokens (WebKit)', () => {
  it('resolve to Catppuccin Latte under prefers-color-scheme: light', async () => {
    await commands.emulateMedia({ colorScheme: 'light' });
    const screen = await render(<Swatches />);

    await expect.poll(() => computed(screen.container)).toEqual(expected('latte'));
  });

  it('resolve to Catppuccin Mocha under prefers-color-scheme: dark', async () => {
    await commands.emulateMedia({ colorScheme: 'dark' });
    const screen = await render(<Swatches />);

    await expect.poll(() => computed(screen.container)).toEqual(expected('mocha'));
  });

  it('switch live when the color scheme changes', async () => {
    await commands.emulateMedia({ colorScheme: 'light' });
    const screen = await render(<Swatches />);

    await expect.poll(() => computed(screen.container).surface).toBe(rgb(latte.base));
    await commands.emulateMedia({ colorScheme: 'dark' });
    await expect.poll(() => computed(screen.container).surface).toBe(rgb(mocha.base));
  });

  it('covers every color token', () => {
    const tokens = Object.keys(color).filter((key) => !key.startsWith('__'));

    expect(tokens.sort()).toEqual(roles.map(([role]) => role).sort());
  });
});

const barProbe = stylex.create({
  text: {
    fontSize: bar.fontSize,
    fontWeight: bar.fontWeight,
    lineHeight: bar.lineHeight,
  },
  item: {
    paddingInline: bar.itemPaddingInline,
    borderStyle: 'solid',
    borderWidth: bar.itemBorderWidth,
    borderRadius: bar.itemRadius,
  },
});

describe('Bar tokens', () => {
  it("resolve to today's Bar text style and item shape", async () => {
    const screen = await render(
      <div>
        <span data-probe="text" {...stylex.props(barProbe.text)}>
          1
        </span>
        <span data-probe="item" {...stylex.props(barProbe.item)} />
      </div>,
    );
    const style = (name: string) =>
      getComputedStyle(screen.container.querySelector(`[data-probe="${name}"]`)!);

    await expect.poll(() => style('text').fontSize).toBe('13px');
    expect(style('text').fontWeight).toBe('500');
    expect(Number.parseFloat(style('text').lineHeight)).toBeCloseTo(13 * 1.6, 3);
    expect(style('item').paddingLeft).toBe('10px');
    expect(style('item').paddingRight).toBe('10px');
    expect(style('item').borderTopLeftRadius).toBe('12px');
    expect(style('item').borderTopWidth).toBe('1px');
  });

  it('define the compact media query', () => {
    expect(media.compact).toBe('@media (width <= 2036px)');
  });
});

const override = stylex.createTheme(color, { accent: 'rgb(1, 2, 3)' });

const themed = stylex.create({
  accent: { color: color.accent },
});

describe('subtree overrides', () => {
  it('a createTheme overrides a token for its subtree and leaves siblings unchanged', async () => {
    await commands.emulateMedia({ colorScheme: 'light' });
    const screen = await render(
      <div>
        <section {...stylex.props(override)}>
          <span data-probe="inside" {...stylex.props(themed.accent)} />
        </section>
        <span data-probe="sibling" {...stylex.props(themed.accent)} />
      </div>,
    );
    const read = (name: string) => {
      const element = screen.container.querySelector(`[data-probe="${name}"]`);

      return element ? getComputedStyle(element).color : 'missing';
    };

    await expect.poll(() => read('inside')).toBe('rgb(1, 2, 3)');
    expect(read('sibling')).toBe(rgb(latte.mauve));
  });
});
