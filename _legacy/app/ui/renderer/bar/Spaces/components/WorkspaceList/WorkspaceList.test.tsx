import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { WorkspaceList } from './WorkspaceList';

describe('WorkspaceList Component', () => {
  test('renders list of workspaces', async () => {
    const onSpaceClick = vi.fn(() => vi.fn());
    const workspaces = [
      { name: 'terminal', displayName: 'Terminal' },
      { name: 'coding', displayName: 'Coding' },
      { name: 'browser', displayName: 'Browser' },
    ];

    const { container } = await render(
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace={undefined}
        onSpaceClick={onSpaceClick}
      />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(3);
    });
  });

  test('renders empty list when no workspaces', async () => {
    const onSpaceClick = vi.fn(() => vi.fn());

    const { container } = await render(
      <WorkspaceList workspaces={[]} focusedWorkspace={undefined} onSpaceClick={onSpaceClick} />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(0);
    });
  });

  test('renders focused and unfocused workspaces', async () => {
    const onSpaceClick = vi.fn(() => vi.fn());
    const workspaces = [
      { name: 'terminal', displayName: 'Terminal' },
      { name: 'coding', displayName: 'Coding' },
    ];

    const { container } = await render(
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace="terminal"
        onSpaceClick={onSpaceClick}
      />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(2);
    });
  });

  test('calls onSpaceClick with correct workspace name', async () => {
    const clickHandler = vi.fn();
    const onSpaceClick = vi.fn(() => clickHandler);
    const workspaces = [{ name: 'terminal', displayName: 'Terminal' }];

    const { container } = await render(
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace={undefined}
        onSpaceClick={onSpaceClick}
      />,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      button?.click();
      expect(onSpaceClick).toHaveBeenCalledWith('terminal');
    });
  });

  test('renders workspace buttons', async () => {
    const onSpaceClick = vi.fn(() => vi.fn());
    const workspaces = [
      { name: 'terminal', displayName: 'Terminal' },
      { name: 'coding', displayName: 'Coding' },
    ];

    const { container } = await render(
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace={undefined}
        onSpaceClick={onSpaceClick}
      />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(2);
    });
  });

  test('renders all workspaces with one focused', async () => {
    const onSpaceClick = vi.fn(() => vi.fn());
    const workspaces = [
      { name: 'terminal', displayName: 'Terminal' },
      { name: 'coding', displayName: 'Coding' },
      { name: 'browser', displayName: 'Browser' },
    ];

    const { container } = await render(
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace="coding"
        onSpaceClick={onSpaceClick}
      />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(3);
    });
  });
});
