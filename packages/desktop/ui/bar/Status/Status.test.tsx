import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Status } from './Status';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const invokeMock = vi.mocked(invoke);

describe('Status Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'get_battery_info') {
        return Promise.resolve({ percentage: 75, state: 'Discharging' });
      }
      if (cmd === 'get_cpu_info') {
        return Promise.resolve({ usage: 30, temperature: 55 });
      }
      if (cmd === 'is_system_awake') {
        return Promise.resolve(false);
      }
      if (cmd === 'get_weather_config') {
        return Promise.resolve(null);
      }
      return Promise.resolve(null);
    });
  });

  test('renders status container with child components', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Status />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('div')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders battery info', async () => {
    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Status />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('75% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders cpu info', async () => {
    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Status />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('30%')).toBeDefined();
    });

    queryClient.clear();
  });
});
