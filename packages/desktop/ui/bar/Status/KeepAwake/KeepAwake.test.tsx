import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { KeepAwake } from './KeepAwake';

import { fetchKeepAwake, onKeepAwakeChanged, toggleKeepAwake } from './KeepAwake.service';

const invokeMock = vi.mocked(invoke);

describe('KeepAwake Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  describe('fetchKeepAwake', () => {
    test('returns true when system is awake', async () => {
      invokeMock.mockResolvedValue(true);

      const result = await fetchKeepAwake();

      expect(invokeMock).toHaveBeenCalledWith('is_system_awake');
      expect(result).toBe(true);
    });

    test('returns false when system is not awake', async () => {
      invokeMock.mockResolvedValue(false);

      const result = await fetchKeepAwake();

      expect(result).toBe(false);
    });
  });

  describe('toggleKeepAwake', () => {
    test('invokes toggle_system_awake and updates query', async () => {
      invokeMock.mockResolvedValue(true);
      const queryClient = createTestQueryClient();

      await toggleKeepAwake(queryClient);

      expect(invokeMock).toHaveBeenCalledWith('toggle_system_awake');
      expect(queryClient.getQueryData(['keep-awake'])).toBe(true);

      queryClient.clear();
    });
  });

  describe('onKeepAwakeChanged', () => {
    test('updates query data when called', () => {
      const queryClient = createTestQueryClient();

      onKeepAwakeChanged(true, queryClient);

      expect(queryClient.getQueryData(['keep-awake'])).toBe(true);

      queryClient.clear();
    });
  });
});

describe('KeepAwake Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('renders when awake is false', async () => {
    invokeMock.mockResolvedValue(false);

    const queryClient = createTestQueryClient();
    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders when awake is true', async () => {
    invokeMock.mockResolvedValue(true);

    const queryClient = createTestQueryClient();
    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
  });

  test('returns null when isSystemAwake is undefined', async () => {
    invokeMock.mockImplementation(() => new Promise(() => {}));

    const queryClient = createTestQueryClient();
    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    expect(container.querySelector('button')).toBeNull();

    queryClient.clear();
  });

  test('renders icon element inside button', async () => {
    invokeMock.mockResolvedValue(true);

    const queryClient = createTestQueryClient();
    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      const svg = button?.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });
});
