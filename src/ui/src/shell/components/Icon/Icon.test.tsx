import { Coffee02Icon } from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';
import { describe, expect, test } from 'vitest';
import { render } from 'vitest-browser-react';

import { Icon } from './Icon';
import { isSimpleIcon } from './Icon.types';

describe('Icon', () => {
  test('renders a Hugeicons icon at 18px with the 1.8 stroke by default', async () => {
    const screen = await render(<Icon icon={Coffee02Icon} />);
    const svg = screen.container.querySelector('svg');

    expect(svg?.getAttribute('width')).toBe('18');
    expect(svg?.getAttribute('height')).toBe('18');
    expect(svg?.querySelector('path')?.getAttribute('stroke-width')).toBe('1.8');
  });

  test('honors size, strokeWidth and color', async () => {
    const screen = await render(<Icon icon={Coffee02Icon} size={22} strokeWidth={2} color="red" />);
    const svg = screen.container.querySelector('svg');

    expect(svg?.getAttribute('width')).toBe('22');
    expect(svg?.getAttribute('color')).toBe('red');
    expect(svg?.querySelector('path')?.getAttribute('stroke-width')).toBe('2');
  });

  test('renders a Simple Icons component through the same API', async () => {
    const screen = await render(<Icon icon={SiTidal} size={18} color="red" />);
    const svg = screen.container.querySelector('svg');

    expect(isSimpleIcon(SiTidal)).toBe(true);
    expect(isSimpleIcon(Coffee02Icon)).toBe(false);
    expect(svg?.getAttribute('width')).toBe('18');
  });
});
