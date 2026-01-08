import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { App } from './App';

describe('App Component', () => {
  test('renders app with icon', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId={100}
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
      expect(container.querySelector('svg')).toBeDefined();
    });
  });

  test('renders display name when focused', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId={100}
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(getByText('Ghostty')).toBeDefined();
    });
  });

  test('does not render display name when not focused', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId={100}
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(container.textContent).not.toContain('Ghostty');
    });
  });

  test('calls onClick when clicked', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId={100}
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(async () => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      button?.click();
      expect(onClick).toHaveBeenCalledTimes(1);
    });
  });

  test('applies focused styles when focused', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId={100}
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      // When focused, display name should be visible
      expect(getByText('Ghostty')).toBeDefined();
    });
  });

  test('does not show display name when not focused', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId={100}
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      // When not focused, display name should not be visible
      expect(container.textContent).not.toContain('Ghostty');
    });
  });

  test('renders window title as display name when provided', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Code"
        displayName="index.ts"
        windowId={100}
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(getByText('index.ts')).toBeDefined();
    });
  });

  test('renders truncated display name', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Code"
        displayName="This is a very long ..."
        windowId={100}
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(getByText('This is a very long ...')).toBeDefined();
    });
  });
});
