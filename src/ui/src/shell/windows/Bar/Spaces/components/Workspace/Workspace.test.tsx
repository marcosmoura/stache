import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Icon } from '../../../../../components/Icon';
import { workspaceIcons } from '../../Spaces.constants';

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

async function iconHtml(icon: Parameters<typeof Icon>[0]['icon']) {
  const { container } = await render(<Icon icon={icon} />);

  return container.innerHTML;
}

describe('Workspace icons and indicator', () => {
  test.each(Object.keys(workspaceIcons))('renders the legacy icon for %s', async (name) => {
    const expected = await iconHtml(workspaceIcons[name]);
    const { container } = await render(
      <Workspace name={name} isFocused={false} onClick={vi.fn()} />,
    );

    expect(container.querySelector('button')?.innerHTML).toBe(expected);
  });

  test('draws the indicator only for the focused workspace, with legacy styles', async () => {
    const { container } = await render(
      <>
        <Workspace name="terminal" isFocused onClick={vi.fn()} />
        <Workspace name="coding" isFocused={false} onClick={vi.fn()} />
      </>,
    );
    const [focused, other] = container.querySelectorAll('button');
    const indicator = focused?.querySelector('div');

    expect(other?.querySelector('div')).toBeNull();

    if (!indicator || !focused) {
      throw new Error('indicator missing');
    }

    const style = getComputedStyle(indicator);
    const button = getComputedStyle(focused);

    expect(style.position).toBe('absolute');
    expect(style.zIndex).toBe('-1');
    expect(style.borderTopLeftRadius).toBe('12px');
    expect(style.backgroundColor).toBe('rgb(69, 71, 90)');
    expect(button.position).toBe('relative');
    expect(button.paddingLeft).toBe('10px');
    expect(button.backgroundColor).toBe('rgba(0, 0, 0, 0)');
  });

  test('an unknown workspace renders its button without an icon instead of crashing the Bar', async () => {
    // Legacy passed `undefined` to Icon here and the Bar's error boundary replaced
    // the whole Bar (approved deviation: no crash, no fallback icon).
    const onClick = vi.fn();
    const { container } = await render(
      <>
        <Workspace name="workspace-1" isFocused onClick={onClick} />
        <Workspace name="toString" isFocused={false} onClick={onClick} />
      </>,
    );

    const buttons = container.querySelectorAll('button');

    expect(buttons).toHaveLength(2);
    expect(container.querySelector('svg')).toBeNull();
    expect(buttons[0].querySelector('[class]')).not.toBeNull();

    buttons[0].click();

    expect(onClick).toHaveBeenCalledWith('workspace-1');
  });
});
