import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Clock } from './Clock';

import { getClockInfo, openClockApp } from './Clock.service';

const invokeMock = vi.mocked(invoke);

describe('Clock Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe('getClockInfo', () => {
    test('returns formatted date string', () => {
      vi.setSystemTime(new Date(2024, 0, 15, 14, 30, 45));

      const result = getClockInfo();

      expect(typeof result).toBe('string');
      expect(result).toContain('14:30:45');
      expect(result).toContain('Mon');
      expect(result).toContain('Jan');
      expect(result).toContain('15');
    });

    test('returns correctly formatted string with padded values', () => {
      vi.setSystemTime(new Date(2024, 0, 5, 9, 5, 3));

      const result = getClockInfo();

      expect(result).toContain('09:05:03');
      expect(result).toContain('05');
    });

    test('returns correct day of month', () => {
      vi.setSystemTime(new Date(2024, 0, 1, 12, 0, 0));

      const result = getClockInfo();

      expect(result).toContain('01');
    });
  });

  describe('openClockApp', () => {
    test('invokes open_app with Clock', async () => {
      invokeMock.mockResolvedValue(undefined);

      await openClockApp();

      expect(invokeMock).toHaveBeenCalledWith('open_app', { name: 'Clock' });
    });
  });
});

describe('Clock Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2024, 0, 15, 14, 30, 0));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  test('renders clock with time', async () => {
    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Clock />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('14:30:00')).toBeDefined();
    });

    queryClient.clear();
  });
});
