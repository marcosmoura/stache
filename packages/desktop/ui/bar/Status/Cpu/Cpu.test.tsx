import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { colors } from '@/design-system';
import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Cpu } from './Cpu';

import { fetchCpu, getCPUElements, openActivityMonitor } from './Cpu.service';
import type { CPUInfo } from './Cpu.types';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

const invokeMock = vi.mocked(invoke);

describe('Cpu Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  describe('fetchCpu', () => {
    test('returns cpu data when invoke succeeds', async () => {
      const mockCpuInfo: CPUInfo = { usage: 45.5, temperature: 65.0 };
      invokeMock.mockResolvedValue(mockCpuInfo);

      const result = await fetchCpu();

      expect(invokeMock).toHaveBeenCalledWith('get_cpu_info');
      expect(result).toEqual(mockCpuInfo);
    });

    test('returns data with null temperature', async () => {
      const mockCpuInfo: CPUInfo = { usage: 30.0, temperature: null };
      invokeMock.mockResolvedValue(mockCpuInfo);

      const result = await fetchCpu();

      expect(result).toEqual(mockCpuInfo);
    });
  });

  describe('openActivityMonitor', () => {
    test('invokes open_app with Activity Monitor', async () => {
      invokeMock.mockResolvedValue(undefined);

      await openActivityMonitor();

      expect(invokeMock).toHaveBeenCalledWith('open_app', { name: 'Activity Monitor' });
    });
  });

  describe('getCPUElements', () => {
    test('returns red color and charge icon when temperature is hot', () => {
      const result = getCPUElements(90);

      expect(result.color).toBe(colors.red);
    });

    test('returns text color and regular icon when temperature is normal', () => {
      const result = getCPUElements(60);

      expect(result.color).toBe(colors.text);
    });

    test('returns text color when temperature is null', () => {
      const result = getCPUElements(null);

      expect(result.color).toBe(colors.text);
    });
  });
});

describe('Cpu Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('renders cpu usage when data is available', async () => {
    const mockCpuInfo: CPUInfo = { usage: 45.5, temperature: 65.0 };
    invokeMock.mockResolvedValue(mockCpuInfo);

    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Cpu />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('45%')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders cpu usage with temperature', async () => {
    const mockCpuInfo: CPUInfo = { usage: 80.0, temperature: 85.0 };
    invokeMock.mockResolvedValue(mockCpuInfo);

    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Cpu />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('80%')).toBeDefined();
      expect(getByText('85°C')).toBeDefined();
    });

    queryClient.clear();
  });
});
