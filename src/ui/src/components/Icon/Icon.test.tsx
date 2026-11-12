import { Home01Icon } from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';
import * as stylex from '@stylexjs/stylex';
import { describe, expect, it } from 'vitest';

import { render } from '../../../tests/render';

import { Icon } from './Icon';

const overrides = stylex.create({
  tinted: { color: 'rgb(10, 20, 30)' },
});

const svgIn = (container: HTMLElement) => {
  const svg = container.querySelector('svg');

  if (!svg) {
    throw new Error('no svg');
  }

  return svg;
};

describe('Icon', () => {
  it('renders an SVG at the default token size', async () => {
    const screen = await render(<Icon icon={Home01Icon} />);
    const svg = svgIn(screen.container);

    await expect.poll(() => getComputedStyle(svg).width).toBe('16px');
    expect(getComputedStyle(svg).height).toBe('16px');
  });

  it('honors an explicit size and stroke width', async () => {
    const screen = await render(<Icon icon={Home01Icon} size={24} strokeWidth={2} />);
    const svg = svgIn(screen.container);

    await expect.poll(() => getComputedStyle(svg).width).toBe('24px');
    expect(svg.querySelector('[stroke-width="2"]')).not.toBeNull();
  });

  it('defaults to a 1.5 stroke in currentColor and inherits the text color', async () => {
    const screen = await render(
      <span style={{ color: 'rgb(200, 0, 0)' }}>
        <Icon icon={Home01Icon} />
      </span>,
    );
    const svg = svgIn(screen.container);
    const stroked = svg.querySelector('[stroke]');

    expect(stroked?.getAttribute('stroke')).toBe('currentColor');
    expect(svg.querySelector('[stroke-width="1.5"]')).not.toBeNull();
    await expect.poll(() => getComputedStyle(svg).color).toBe('rgb(200, 0, 0)');
  });

  it('is hidden from assistive technology without a label', async () => {
    const screen = await render(<Icon icon={Home01Icon} />);
    const svg = svgIn(screen.container);

    expect(svg.getAttribute('aria-hidden')).toBe('true');
    expect(svg.getAttribute('role')).toBeNull();
  });

  it('is an image named by its label', async () => {
    const screen = await render(<Icon icon={Home01Icon} label="Home" />);

    await expect.element(screen.getByRole('img', { name: 'Home' })).toBeInTheDocument();
    expect(svgIn(screen.container).getAttribute('aria-hidden')).toBeNull();
  });

  it('merges the style prop last', async () => {
    const screen = await render(<Icon icon={Home01Icon} style={overrides.tinted} />);
    const svg = svgIn(screen.container);

    await expect.poll(() => getComputedStyle(svg).color).toBe('rgb(10, 20, 30)');
  });

  describe('with a simple-icons component', () => {
    it('renders a filled SVG at the default token size and an explicit size', async () => {
      const screen = await render(
        <span style={{ color: 'rgb(200, 0, 0)' }}>
          <Icon icon={SiTidal} />
          <Icon icon={SiTidal} size={24} />
        </span>,
      );
      const [small, large] = screen.container.querySelectorAll('svg');

      await expect.poll(() => getComputedStyle(small!).width).toBe('16px');
      expect(getComputedStyle(large!).width).toBe('24px');
      expect(small!.getAttribute('fill')).toBe('currentColor');
      expect(getComputedStyle(small!).color).toBe('rgb(200, 0, 0)');
    });

    it('is hidden without a label and has no title text', async () => {
      const screen = await render(<Icon icon={SiTidal} />);
      const svg = svgIn(screen.container);

      expect(svg.getAttribute('aria-hidden')).toBe('true');
      expect(svg.getAttribute('role')).toBeNull();
      expect(svg.querySelector('title')?.textContent ?? '').toBe('');
    });

    it('is an image named by its label', async () => {
      const screen = await render(<Icon icon={SiTidal} label="TIDAL" />);

      await expect.element(screen.getByRole('img', { name: 'TIDAL' })).toBeInTheDocument();
      expect(svgIn(screen.container).getAttribute('aria-hidden')).toBeNull();
    });

    it('merges the style prop last', async () => {
      const screen = await render(<Icon icon={SiTidal} style={overrides.tinted} />);

      await expect
        .poll(() => getComputedStyle(svgIn(screen.container)).color)
        .toBe('rgb(10, 20, 30)');
    });
  });
});
