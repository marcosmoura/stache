import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Bar } from './Bar';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const invokeMock = vi.mocked(invoke);

describe('Bar Component', () => {
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
      if (cmd === 'get_current_media_info') {
        return Promise.resolve({});
      }
      if (cmd === 'get_battery_info') {
        return Promise.resolve({ percentage: 100, state: 'Full' });
      }
      if (cmd === 'get_cpu_info') {
        return Promise.resolve({ usage: 25, temperature: 50 });
      }
      if (cmd === 'is_system_awake') {
        return Promise.resolve(false);
      }
      if (cmd === 'get_weather_config') {
        return Promise.resolve({});
      }
      return Promise.resolve('');
    });
  });

  test('renders main bar container', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('div')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders Spaces, Media, and Status containers', async () => {
    const queryClient = createTestQueryClient();
    const { getByTestId } = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('spaces-container')).toBeDefined();
      expect(getByTestId('media-container')).toBeDefined();
      expect(getByTestId('status-container')).toBeDefined();
    });

    queryClient.clear();
  });
});
