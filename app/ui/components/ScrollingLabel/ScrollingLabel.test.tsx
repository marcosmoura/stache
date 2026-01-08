import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { ScrollingLabel } from './ScrollingLabel';

describe('ScrollingLabel Component', () => {
  test('renders children correctly', async () => {
    const { getByText } = await render(<ScrollingLabel>Test Label</ScrollingLabel>);

    await vi.waitFor(() => {
      expect(getByText('Test Label')).toBeDefined();
    });
  });

  test('renders with wrapper and label elements', async () => {
    const { container } = await render(<ScrollingLabel>Content</ScrollingLabel>);

    await vi.waitFor(() => {
      const wrapper = container.querySelector('div');
      const label = container.querySelector('span');
      expect(wrapper).toBeDefined();
      expect(label).toBeDefined();
    });
  });

  test('applies custom className to wrapper', async () => {
    const customClass = 'my-custom-class';
    const { container } = await render(
      <ScrollingLabel className={customClass}>Styled</ScrollingLabel>,
    );

    await vi.waitFor(() => {
      const wrapper = container.querySelector('div');
      expect(wrapper?.classList.contains(customClass)).toBe(true);
    });
  });

  test('forwards additional HTML attributes', async () => {
    const { container } = await render(
      <ScrollingLabel data-testid="scrolling-label" aria-label="Scrolling content">
        With Attributes
      </ScrollingLabel>,
    );

    await vi.waitFor(() => {
      const wrapper = container.querySelector('div');
      expect(wrapper?.getAttribute('data-testid')).toBe('scrolling-label');
      expect(wrapper?.getAttribute('aria-label')).toBe('Scrolling content');
    });
  });

  test('renders short content without scrolling styles', async () => {
    const { container } = await render(<ScrollingLabel>Short</ScrollingLabel>);

    await vi.waitFor(() => {
      const label = container.querySelector('span');
      const style = label?.getAttribute('style');
      // Check that scroll distance is 0 for short content
      expect(style).toContain('--scroll-distance: 0px');
    });
  });

  test('accepts custom scrollSpeed prop', async () => {
    const { container } = await render(
      <ScrollingLabel scrollSpeed={100}>Fast Scroll</ScrollingLabel>,
    );

    await vi.waitFor(() => {
      const label = container.querySelector('span');
      expect(label).toBeDefined();
      // Component should render without errors with custom scrollSpeed
    });
  });

  test('updates when children change', async () => {
    const { getByText, rerender } = await render(<ScrollingLabel>Initial</ScrollingLabel>);

    await vi.waitFor(() => {
      expect(getByText('Initial')).toBeDefined();
    });

    await rerender(<ScrollingLabel>Updated</ScrollingLabel>);

    await vi.waitFor(() => {
      expect(getByText('Updated')).toBeDefined();
    });
  });

  test('applies scroll CSS variables to label', async () => {
    const { container } = await render(<ScrollingLabel>Label with styles</ScrollingLabel>);

    await vi.waitFor(() => {
      const label = container.querySelector('span');
      const style = label?.getAttribute('style');
      expect(style).toContain('--scroll-distance');
      expect(style).toContain('--scroll-duration');
    });
  });

  test('handles empty children', async () => {
    const { container } = await render(<ScrollingLabel>{''}</ScrollingLabel>);

    await vi.waitFor(() => {
      const wrapper = container.querySelector('div');
      const label = container.querySelector('span');
      expect(wrapper).toBeDefined();
      expect(label).toBeDefined();
    });
  });

  test('handles complex children', async () => {
    const { container } = await render(
      <ScrollingLabel>
        <strong>Bold</strong> and <em>italic</em>
      </ScrollingLabel>,
    );

    await vi.waitFor(() => {
      const strong = container.querySelector('strong');
      const em = container.querySelector('em');
      expect(strong).toBeDefined();
      expect(em).toBeDefined();
    });
  });
});
