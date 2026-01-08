import { faSpotify } from '@fortawesome/free-brands-svg-icons';
import { Home01Icon } from '@hugeicons/core-free-icons';
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

  describe('FontAwesome pack', () => {
    test('renders FontAwesome icon', async () => {
      const { container } = await render(<Icon pack="fontawesome" icon={faSpotify} />);

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg).toBeDefined();
        // FontAwesome adds specific data attribute
        expect(svg?.getAttribute('data-icon')).toBe('spotify');
      });
    });

    test('forwards FontAwesome-specific props', async () => {
      const { container } = await render(
        <Icon pack="fontawesome" icon={faSpotify} data-testid="fa-icon" className="fa-custom" />,
      );

      await vi.waitFor(() => {
        const svg = container.querySelector('svg');
        expect(svg?.getAttribute('data-testid')).toBe('fa-icon');
        expect(svg?.classList.contains('fa-custom')).toBe(true);
      });
    });
  });
});
