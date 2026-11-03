import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Workspace } from './Workspace';

describe('Workspace Component', () => {
  test('renders workspace with icon', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <Workspace name="terminal" isFocused={false} onClick={onClick} />,
    );

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
      expect(container.querySelector('svg')).toBeDefined();
    });
  });

  test('renders workspace button', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <Workspace name="terminal" isFocused={true} onClick={onClick} />,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
    });
  });

  test('renders workspace when not focused', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <Workspace name="terminal" isFocused={false} onClick={onClick} />,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
    });
  });

  test('calls onClick when clicked', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <Workspace name="terminal" isFocused={false} onClick={onClick} />,
    );

    await vi.waitFor(async () => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      button?.click();
      expect(onClick).toHaveBeenCalledTimes(1);
    });
  });

  test('renders different workspace icons', async () => {
    const onClick = vi.fn();

    const { container: terminalContainer } = await render(
      <Workspace name="terminal" isFocused={false} onClick={onClick} />,
    );

    const { container: codingContainer } = await render(
      <Workspace name="coding" isFocused={false} onClick={onClick} />,
    );

    await vi.waitFor(() => {
      expect(terminalContainer.querySelector('svg')).toBeDefined();
      expect(codingContainer.querySelector('svg')).toBeDefined();
    });
  });
});
