import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { colors } from '@/design-system';
import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Battery } from './Battery';

import {
  fetchBattery,
  getBatteryIcon,
  getBatteryIconColor,
  getPollingInterval,
} from './Battery.service';
import type { BatteryState } from './Battery.types';

const invokeMock = vi.mocked(invoke);

describe('Battery Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  describe('fetchBattery', () => {
    test('returns battery data when invoke succeeds', async () => {
      invokeMock.mockResolvedValue({ percentage: 75, state: 'Discharging' });

      const result = await fetchBattery();

      expect(invokeMock).toHaveBeenCalledWith('get_battery_info');
      expect(result).toEqual({
        label: '75% (Discharging)',
        percentage: 75,
        state: 'Discharging',
      });
    });

    test('returns null when battery data is not available', async () => {
      invokeMock.mockResolvedValue(null);

      const result = await fetchBattery();

      expect(result).toBeNull();
    });

    test('returns 100% label when battery is full', async () => {
      invokeMock.mockResolvedValue({ percentage: 100, state: 'Full' });

      const result = await fetchBattery();

      expect(result).toEqual({
        label: '100%',
        percentage: 100,
        state: 'Full',
      });
    });
  });

  describe('getBatteryIcon', () => {
    test('returns empty icon when percentage is undefined', () => {
      const icon = getBatteryIcon('Unknown', undefined);
      expect(icon).toBeDefined();
    });

    test('returns charging icon when charging', () => {
      const icon = getBatteryIcon('Charging', 50);
      expect(icon).toBeDefined();
    });

    test('returns full icon at 100%', () => {
      const icon = getBatteryIcon('Discharging', 100);
      expect(icon).toBeDefined();
    });

    test('returns appropriate icons for different percentage ranges', () => {
      expect(getBatteryIcon('Discharging', 80)).toBeDefined();
      expect(getBatteryIcon('Discharging', 60)).toBeDefined();
      expect(getBatteryIcon('Discharging', 30)).toBeDefined();
      expect(getBatteryIcon('Discharging', 10)).toBeDefined();
    });
  });

  describe('getBatteryIconColor', () => {
    test('returns green for charging state', () => {
      expect(getBatteryIconColor('Charging')).toBe(colors.green);
    });

    test('returns yellow for discharging state', () => {
      expect(getBatteryIconColor('Discharging')).toBe(colors.yellow);
    });

    test('returns red for empty state', () => {
      expect(getBatteryIconColor('Empty')).toBe(colors.red);
    });

    test('returns text color for other states', () => {
      expect(getBatteryIconColor('Full')).toBe(colors.text);
      expect(getBatteryIconColor('Unknown')).toBe(colors.text);
    });
  });

  describe('getPollingInterval', () => {
    test('returns 30 seconds for charging state', () => {
      expect(getPollingInterval('Charging')).toBe(30 * 1000);
    });

    test('returns 2 minutes for non-charging states', () => {
      const twoMinutes = 2 * 60 * 1000;
      expect(getPollingInterval('Discharging')).toBe(twoMinutes);
      expect(getPollingInterval('Full')).toBe(twoMinutes);
      expect(getPollingInterval(undefined)).toBe(twoMinutes);
    });
  });
});

describe('Battery Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('renders nothing when battery data is not available', async () => {
    invokeMock.mockResolvedValue(null);

    const queryClient = createTestQueryClient();
    const { container } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.innerHTML).toBe('');
    });

    queryClient.clear();
  });

  test('renders battery info when data is available', async () => {
    invokeMock.mockResolvedValue({ percentage: 50, state: 'Discharging' as BatteryState });

    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('50% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });
});
