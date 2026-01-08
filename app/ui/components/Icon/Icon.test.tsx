import { Home01Icon } from '@hugeicons/core-free-icons';
import { SiTidal } from '@icons-pack/react-simple-icons';
import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Icon } from './Icon';

describe('Icon Component', () => {
  describe('HugeIcons (default pack)', () => {
    test('renders HugeIcons icon by default', async () => {
      const { container } = await render(<Icon icon={Home01Icon} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('renders HugeIcons icon with explicit pack prop', async () => {
      const { container } = await render(<Icon pack="hugeicons" icon={Home01Icon} />);

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

  describe('SimpleIcons pack', () => {
    test('renders SimpleIcons icon', async () => {
      const { container } = await render(<Icon pack="simple-icons" icon={SiTidal} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('renders SimpleIcons with custom size', async () => {
      const { container } = await render(<Icon pack="simple-icons" icon={SiTidal} size={24} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
      });
    });

    test('forwards SimpleIcons-specific props', async () => {
      const { container } = await render(
        <Icon pack="simple-icons" icon={SiTidal} data-testid="si-icon" className="si-custom" />,
      );

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg?.getAttribute('data-testid')).toBe('si-icon');
        expect(svg?.classList.contains('si-custom')).toBe(true);
      });
    });
  });
});
