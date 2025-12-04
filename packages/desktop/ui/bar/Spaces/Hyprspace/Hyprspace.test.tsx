import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Hyprspace } from './Hyprspace';

import {
  fetchCurrentHyprspaceWorkspace,
  fetchHyprspaceWorkspaceList,
  getSortedWorkspaces,
  onCLIEvent,
  onWorkspaceClick,
} from './Hyprspace.service';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const invokeMock = vi.mocked(invoke);

describe('Hyprspace Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  describe('fetchHyprspaceWorkspaceList', () => {
    test('invokes get_hyprspace_workspaces', async () => {
      const mockWorkspaces = [{ workspace: 'terminal' }, { workspace: 'coding' }];
      invokeMock.mockResolvedValue(mockWorkspaces);

      const result = await fetchHyprspaceWorkspaceList();

      expect(invokeMock).toHaveBeenCalledWith('get_hyprspace_workspaces');
      expect(result).toEqual(mockWorkspaces);
    });
  });

  describe('fetchCurrentHyprspaceWorkspace', () => {
    test('invokes get_hyprspace_focused_workspace', async () => {
      const mockCurrent = { workspace: 'terminal' };
      invokeMock.mockResolvedValue(mockCurrent);

      const result = await fetchCurrentHyprspaceWorkspace();

      expect(invokeMock).toHaveBeenCalledWith('get_hyprspace_focused_workspace');
      expect(result).toEqual(mockCurrent);
    });
  });

  describe('getSortedWorkspaces', () => {
    test('returns null when data is undefined', () => {
      const result = getSortedWorkspaces(undefined);

      expect(result).toBeNull();
    });

    test('sorts workspaces according to default order', () => {
      const workspaces = [{ workspace: 'coding' }, { workspace: 'terminal' }];

      const result = getSortedWorkspaces(workspaces);

      expect(result).toBeDefined();
      expect(result?.[0].name).toBe('terminal');
      expect(result?.[1].name).toBe('coding');
    });

    test('assigns correct icons to known workspaces', () => {
      const workspaces = [{ workspace: 'terminal' }];

      const result = getSortedWorkspaces(workspaces);

      expect(result?.[0].icon).toBeDefined();
    });

    test('assigns default icon to unknown workspaces', () => {
      const workspaces = [{ workspace: 'unknown-workspace' }];

      const result = getSortedWorkspaces(workspaces);

      expect(result?.[0].icon).toBeDefined();
    });
  });

  describe('onCLIEvent', () => {
    test('handles workspace-changed event', () => {
      const queryClient = createTestQueryClient();
      const setDataSpy = vi.spyOn(queryClient, 'setQueryData');
      const refetchSpy = vi.spyOn(queryClient, 'refetchQueries');

      onCLIEvent({ name: 'workspace-changed', data: 'coding' }, queryClient);

      expect(setDataSpy).toHaveBeenCalledWith(['hyprspace_current_workspace'], {
        workspace: 'coding',
      });
      expect(refetchSpy).toHaveBeenCalledWith({ queryKey: ['hyprspace_workspaces'] });

      queryClient.clear();
    });

    test('handles focus-changed event', () => {
      const queryClient = createTestQueryClient();
      const refetchSpy = vi.spyOn(queryClient, 'refetchQueries');

      onCLIEvent({ name: 'focus-changed' }, queryClient);

      expect(refetchSpy).toHaveBeenCalledWith({ queryKey: ['hyprspace_current_workspace'] });
      expect(refetchSpy).toHaveBeenCalledWith({ queryKey: ['hyprspace_workspaces'] });

      queryClient.clear();
    });

    test('does nothing for unknown events', () => {
      const queryClient = createTestQueryClient();
      const refetchSpy = vi.spyOn(queryClient, 'refetchQueries');
      const setDataSpy = vi.spyOn(queryClient, 'setQueryData');

      onCLIEvent({ name: 'unknown-event' }, queryClient);

      expect(refetchSpy).not.toHaveBeenCalled();
      expect(setDataSpy).not.toHaveBeenCalled();

      queryClient.clear();
    });
  });

  describe('onWorkspaceClick', () => {
    test('invokes go_to_hyprspace_workspace', async () => {
      invokeMock.mockResolvedValue(undefined);

      await onWorkspaceClick('coding');

      expect(invokeMock).toHaveBeenCalledWith('go_to_hyprspace_workspace', { workspace: 'coding' });
    });

    test('handles error gracefully', async () => {
      const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
      invokeMock.mockRejectedValue(new Error('Failed'));

      await onWorkspaceClick('coding');

      expect(consoleSpy).toHaveBeenCalled();
      consoleSpy.mockRestore();
    });
  });
});

describe('Hyprspace Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('renders workspaces when data is available', async () => {
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'get_hyprspace_workspaces') {
        return Promise.resolve([{ workspace: 'terminal' }, { workspace: 'coding' }]);
      }
      if (cmd === 'get_hyprspace_focused_workspace') {
        return Promise.resolve({ workspace: 'terminal' });
      }
      return Promise.resolve(null);
    });

    const queryClient = createTestQueryClient();
    const { container } = await render(<Hyprspace />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelectorAll('button').length).toBeGreaterThan(0);
    });

    queryClient.clear();
  });

  test('renders nothing when no workspaces', async () => {
    invokeMock.mockResolvedValue([]);

    const queryClient = createTestQueryClient();
    const { container } = await render(<Hyprspace />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.innerHTML).toBe('');
    });

    queryClient.clear();
  });
});
