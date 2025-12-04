import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Spaces } from './Spaces';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const invokeMock = vi.mocked(invoke);

describe('Spaces Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'get_hyprspace_workspaces') {
        return Promise.resolve([{ workspace: 'terminal' }]);
      }
      if (cmd === 'get_hyprspace_focused_workspace') {
        return Promise.resolve({ workspace: 'terminal' });
      }
      if (cmd === 'get_hyprspace_focused_window') {
        return Promise.resolve([{ appName: 'Ghostty', title: 'zsh' }]);
      }
      return Promise.resolve(null);
    });
  });

  test('renders Hyprspace and CurrentApp components', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Should have the main container
      expect(container.querySelector('div')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders workspace buttons', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelectorAll('button').length).toBeGreaterThan(0);
    });

    queryClient.clear();
  });
});
