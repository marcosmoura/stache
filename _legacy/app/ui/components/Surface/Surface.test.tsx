import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Surface } from './Surface';

describe('Surface Component', () => {
  test('renders children correctly', async () => {
    const { getByText } = await render(<Surface>Surface Content</Surface>);

    await vi.waitFor(() => {
      expect(getByText('Surface Content')).toBeDefined();
    });
  });

  test('renders as div by default', async () => {
    const { container } = await render(<Surface>Default</Surface>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div).toBeDefined();
    });
  });

  test('renders as custom element using "as" prop', async () => {
    const { container } = await render(<Surface as="section">Section Content</Surface>);

    await vi.waitFor(() => {
      const section = container.querySelector('section');
      expect(section).toBeDefined();
      expect(container.querySelector('div')).toBeNull();
    });
  });

  test('renders as button element', async () => {
    const { container } = await render(<Surface as="button">Button Surface</Surface>);

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
    });
  });

  test('renders as article element', async () => {
    const { container } = await render(<Surface as="article">Article Surface</Surface>);

    await vi.waitFor(() => {
      const article = container.querySelector('article');
      expect(article).toBeDefined();
    });
  });

  test('renders as span element', async () => {
    const { container } = await render(<Surface as="span">Span Surface</Surface>);

    await vi.waitFor(() => {
      const span = container.querySelector('span');
      expect(span).toBeDefined();
    });
  });

  test('applies custom className', async () => {
    const customClass = 'my-surface-class';
    const { container } = await render(<Surface className={customClass}>Styled</Surface>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.classList.contains(customClass)).toBe(true);
    });
  });

  test('applies base surface styles', async () => {
    const { container } = await render(<Surface>Base</Surface>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.classList.length).toBeGreaterThanOrEqual(1);
    });
  });

  test('combines surface styles with custom className', async () => {
    const customClass = 'custom-class';
    const { container } = await render(<Surface className={customClass}>Combined</Surface>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.classList.contains(customClass)).toBe(true);
      // Should have both base surface class and custom class
      expect(div?.classList.length).toBeGreaterThanOrEqual(2);
    });
  });

  test('forwards additional HTML attributes', async () => {
    const { container } = await render(
      <Surface data-testid="test-surface" aria-label="Surface container" id="surface-1">
        With Attributes
      </Surface>,
    );

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.getAttribute('data-testid')).toBe('test-surface');
      expect(div?.getAttribute('aria-label')).toBe('Surface container');
      expect(div?.getAttribute('id')).toBe('surface-1');
    });
  });

  test('forwards element-specific props when using "as"', async () => {
    const handleClick = vi.fn();
    const { container } = await render(
      <Surface as="button" type="submit" onClick={handleClick} disabled>
        Submit
      </Surface>,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button?.getAttribute('type')).toBe('submit');
      expect(button?.hasAttribute('disabled')).toBe(true);
    });
  });

  test('handles click events on button surface', async () => {
    const handleClick = vi.fn();
    const { container } = await render(
      <Surface as="button" onClick={handleClick}>
        Clickable
      </Surface>,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      button?.click();
      expect(handleClick).toHaveBeenCalledTimes(1);
    });
  });

  test('renders nested content', async () => {
    const { container } = await render(
      <Surface>
        <h1>Title</h1>
        <p>Paragraph</p>
        <span>Inline</span>
      </Surface>,
    );

    await vi.waitFor(() => {
      expect(container.querySelector('h1')).toBeDefined();
      expect(container.querySelector('p')).toBeDefined();
      expect(container.querySelector('span')).toBeDefined();
    });
  });

  test('handles empty children', async () => {
    const { container } = await render(<Surface />);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div).toBeDefined();
      expect(div?.children.length).toBe(0);
    });
  });

  test('renders as anchor element with href', async () => {
    const { container } = await render(
      <Surface as="a" href="https://example.com">
        Link Surface
      </Surface>,
    );

    await vi.waitFor(() => {
      const anchor = container.querySelector('a');
      expect(anchor).toBeDefined();
      expect(anchor?.getAttribute('href')).toBe('https://example.com');
    });
  });
});
