import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import type { BatteryInfo } from '@/stores/BatteryStore';

import { Battery } from './Battery';

let mockBattery: BatteryInfo | null = null;

vi.mock('@/stores/BatteryStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/stores/BatteryStore')>();
  return {
    ...actual,
    useBatteryStore: () => ({
      battery: mockBattery,
      isLoading: false,
    }),
  };
});

const createMockBattery = (overrides: Partial<BatteryInfo> = {}): BatteryInfo => ({
  percentage: 75,
  state: 'Discharging',
  health: 100,
  technology: 'LithiumIon',
  energy: 50,
  energy_full: 100,
  energy_full_design: 100,
  energy_rate: 10,
  voltage: 12,
  temperature: 35.5,
  cycle_count: 120,
  time_to_full: null,
  time_to_empty: 7200,
  vendor: null,
  model: null,
  serial_number: null,
  ...overrides,
});

describe('Battery widget', () => {
  beforeEach(() => {
    mockBattery = null;
  });

  test('renders no battery detected state', async () => {
    const screen = await render(<Battery />);

    await expect.element(screen.getByText('No battery detected')).toBeVisible();
    await expect.element(screen.getByText('Battery', { exact: true })).toBeVisible();
  });

  test('renders battery stats', async () => {
    mockBattery = createMockBattery();

    const screen = await render(<Battery />);

    await expect.element(screen.getByText('75%', { exact: true })).toBeVisible();
    await expect.element(screen.getByText('On Battery')).toBeVisible();
    await expect.element(screen.getByText('100%', { exact: true }).first()).toBeVisible();
    await expect.element(screen.getByText('120')).toBeVisible();
    await expect.element(screen.getByText('35.5°C')).toBeVisible();
    await expect.element(screen.getByText('12.00V')).toBeVisible();
    await expect.element(screen.getByText('2h 0m remaining')).toBeVisible();
  });

  test('renders charging time remaining', async () => {
    mockBattery = createMockBattery({
      state: 'Charging',
      time_to_full: 3600,
      time_to_empty: null,
    });

    const screen = await render(<Battery />);

    await expect.element(screen.getByText('Charging')).toBeVisible();
    await expect.element(screen.getByText('1h 0m until full')).toBeVisible();
  });

  test('renders N/A for missing temperature and cycles', async () => {
    mockBattery = createMockBattery({ temperature: null, cycle_count: null });

    const screen = await render(<Battery />);

    await expect.element(screen.getByText('N/A', { exact: true }).first()).toBeVisible();
    expect(await screen.getByText('N/A', { exact: true }).all()).toHaveLength(2);
  });
});
