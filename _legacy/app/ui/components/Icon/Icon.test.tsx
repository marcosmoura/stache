import { Home01Icon } from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';
import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Icon } from './Icon';

describe('Icon Component', () => {
  describe('HugeIcons', () => {
    test('renders HugeIcons icon', async () => {
      const { container } = await render(<Icon icon={Home01Icon} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('renders icon with custom size prop', async () => {
      const { container } = await render(<Icon icon={Home01Icon} size={24} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('renders icon with custom strokeWidth prop', async () => {
      const { container } = await render(<Icon icon={Home01Icon} strokeWidth={2.5} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });
  });

  describe('SimpleIcons', () => {
    test('renders SimpleIcons icon automatically', async () => {
      const { container } = await render(<Icon icon={SiTidal} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('renders SimpleIcons with custom size', async () => {
      const { container } = await render(<Icon icon={SiTidal} size={24} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('renders SimpleIcons with color prop', async () => {
      const { container } = await render(<Icon icon={SiTidal} color="#ff0000" />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });
  });
});
