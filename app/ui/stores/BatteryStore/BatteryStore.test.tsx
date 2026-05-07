import { Suspense } from 'react';

import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { useBatteryStore } from './BatteryStore';
import type { BatteryInfo } from './BatteryStore.types';

// Mock Tauri APIs
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock('@/hooks/useCrossWindowSync', () => ({
  useCrossWindowSync: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

const createMockBatteryInfo = (overrides: Partial<BatteryInfo> = {}): BatteryInfo => ({
  percentage: 80,
  state: 'Discharging',
  health: 95,
  technology: 'LithiumIon',
  energy: 45.5,
  energy_full: 60.0,
  energy_full_design: 65.0,
  energy_rate: 15.0,
  voltage: 12.5,
  temperature: 35,
  cycle_count: 150,
  time_to_full: null,
  time_to_empty: 10800,
  vendor: 'Apple',
  model: 'MacBook Pro Battery',
  serial_number: 'ABC123',
  ...overrides,
});

/* Test component that renders battery data */
const BatteryTestComponent = ({
  renderFn,
}: {
  renderFn: (data: ReturnType<typeof useBatteryStore>) => React.ReactNode;
}) => {
  const result = useBatteryStore();
  return <>{renderFn(result)}</>;
};

/* Helper to render battery store tests */
const renderBatteryTest = async (
  renderFn: (data: ReturnType<typeof useBatteryStore>) => React.ReactNode,
) => {
  const queryClient = createTestQueryClient();
  const SuspenseWrapper = createQueryClientWrapper(queryClient);

  return render(
    <SuspenseWrapper>
      <Suspense fallback={<div data-testid="loading">Loading...</div>}>
        <BatteryTestComponent renderFn={renderFn} />
      </Suspense>
    </SuspenseWrapper>,
  );
};

describe('useBatteryStore', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  describe('data fetching', () => {
    test('fetches and exposes battery info', async () => {
      const mockBattery = createMockBatteryInfo({
        percentage: 75,
        state: 'Charging',
        health: 92,
        technology: 'LithiumPolymer',
      });
      mockInvoke.mockResolvedValue(mockBattery);

      const screen = await renderBatteryTest(({ battery }) => (
        <div>
          <span data-testid="percentage">{battery?.percentage}</span>
          <span data-testid="state">{battery?.state}</span>
          <span data-testid="health">{battery?.health}</span>
          <span data-testid="technology">{battery?.technology}</span>
        </div>
      ));

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('percentage')).toHaveTextContent('75');
      });

      expect(mockInvoke).toHaveBeenCalledWith('get_battery_info', undefined);
      expect(screen.getByTestId('state')).toHaveTextContent('Charging');
      expect(screen.getByTestId('health')).toHaveTextContent('92');
      expect(screen.getByTestId('technology')).toHaveTextContent('LithiumPolymer');
    });

    test('returns null when no battery present', async () => {
      mockInvoke.mockResolvedValue(null);

      const screen = await renderBatteryTest(({ battery }) => (
        <div data-testid="result">{battery === null ? 'no-battery' : 'has-battery'}</div>
      ));

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('result')).toHaveTextContent('no-battery');
      });
    });

    test('exposes energy and voltage information', async () => {
      const mockBattery = createMockBatteryInfo({
        energy: 30.5,
        energy_full: 50.0,
        energy_rate: 12.5,
        voltage: 11.8,
      });
      mockInvoke.mockResolvedValue(mockBattery);

      const screen = await renderBatteryTest(({ battery }) => (
        <div>
          <span data-testid="energy">{battery?.energy}</span>
          <span data-testid="energy-full">{battery?.energy_full}</span>
          <span data-testid="energy-rate">{battery?.energy_rate}</span>
          <span data-testid="voltage">{battery?.voltage}</span>
        </div>
      ));

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('energy')).toHaveTextContent('30.5');
      });

      expect(screen.getByTestId('energy-full')).toHaveTextContent('50');
      expect(screen.getByTestId('energy-rate')).toHaveTextContent('12.5');
      expect(screen.getByTestId('voltage')).toHaveTextContent('11.8');
    });

    test('exposes time estimates', async () => {
      const mockBattery = createMockBatteryInfo({
        time_to_empty: 7200,
        time_to_full: null,
      });
      mockInvoke.mockResolvedValue(mockBattery);

      const screen = await renderBatteryTest(({ battery }) => (
        <div>
          <span data-testid="time-empty">{battery?.time_to_empty ?? 'null'}</span>
          <span data-testid="time-full">{battery?.time_to_full ?? 'null'}</span>
        </div>
      ));

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('time-empty')).toHaveTextContent('7200');
      });

      expect(screen.getByTestId('time-full')).toHaveTextContent('null');
    });
  });

  describe('battery states', () => {
    test.each<BatteryInfo['state']>(['Unknown', 'Charging', 'Discharging', 'Empty', 'Full'])(
      'handles %s state',
      async (state) => {
        mockInvoke.mockResolvedValue(createMockBatteryInfo({ state }));

        const screen = await renderBatteryTest(({ battery }) => (
          <div data-testid="state">{battery?.state}</div>
        ));

        await vi.waitFor(async () => {
          await expect.element(screen.getByTestId('state')).toHaveTextContent(state);
        });
      },
    );
  });

  describe('hook interface', () => {
    test('returns battery and isLoading properties', async () => {
      mockInvoke.mockResolvedValue(createMockBatteryInfo());

      let captured: ReturnType<typeof useBatteryStore> | undefined;

      const screen = await renderBatteryTest((result) => {
        captured = result;
        return <div data-testid="done">Done</div>;
      });

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('done')).toBeVisible();
      });

      expect(captured).toBeDefined();
      expect(captured).toHaveProperty('battery');
      expect(captured).toHaveProperty('isLoading');
      expect(captured!.battery).not.toBeNull();
    });
  });
});
