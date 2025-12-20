import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Button } from './Button';

describe('Button Component', () => {
  test('renders children correctly', async () => {
    const { getByText } = await render(<Button>Click me</Button>);

    await vi.waitFor(() => {
      expect(getByText('Click me')).toBeDefined();
    });
  });

  test('renders as button element by default', async () => {
    const { container } = await render(<Button>Test</Button>);

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      expect(button?.getAttribute('type')).toBe('button');
    });
  });

  test('applies type attribute correctly', async () => {
    const { container } = await render(<Button type="submit">Submit</Button>);

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button?.getAttribute('type')).toBe('submit');
    });
  });

  test('applies active state class when active is true', async () => {
    const { container } = await render(<Button active>Active Button</Button>);

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button?.classList.length).toBeGreaterThan(1);
    });
  });

  test('does not apply active class when active is false', async () => {
    const { container: activeContainer } = await render(<Button active>Active</Button>);
    const { container: inactiveContainer } = await render(<Button active={false}>Inactive</Button>);

    await vi.waitFor(() => {
      const activeButton = activeContainer.querySelector('button');
      const inactiveButton = inactiveContainer.querySelector('button');

      // Active button should have more classes than inactive
      expect(activeButton?.classList.length).toBeGreaterThan(inactiveButton?.classList.length ?? 0);
    });
  });

  test('applies custom className', async () => {
    const customClass = 'my-custom-class';
    const { container } = await render(<Button className={customClass}>Styled</Button>);

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button?.classList.contains(customClass)).toBe(true);
    });
  });

  test('forwards additional HTML attributes', async () => {
    const { container } = await render(
      <Button data-testid="test-button" disabled aria-label="Test button">
        Disabled
      </Button>,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button?.getAttribute('data-testid')).toBe('test-button');
      expect(button?.hasAttribute('disabled')).toBe(true);
      expect(button?.getAttribute('aria-label')).toBe('Test button');
    });
  });

  test('handles click events', async () => {
    const handleClick = vi.fn();
    const { container } = await render(<Button onClick={handleClick}>Click me</Button>);

    await vi.waitFor(async () => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      button?.click();
      expect(handleClick).toHaveBeenCalledTimes(1);
    });
  });

  test('combines active state with custom className', async () => {
    const customClass = 'custom-class';
    const { container } = await render(
      <Button active className={customClass}>
        Combined
      </Button>,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button?.classList.contains(customClass)).toBe(true);
      // Should have base class, active class, and custom class
      expect(button?.classList.length).toBeGreaterThanOrEqual(3);
    });
  });
});
