import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import type { BatteryInfo } from '@/stores/BatteryStore';
import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Battery } from './Battery';

// Mock battery data for tests
let mockBattery: BatteryInfo | null = null;

// Mock the useBatteryStore hook
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
  temperature: null,
  cycle_count: null,
  time_to_full: null,
  time_to_empty: null,
  vendor: null,
  model: null,
  serial_number: null,
  ...overrides,
});

describe('Battery Component', () => {
  beforeEach(() => {
    // Reset mock battery state before each test
    mockBattery = null;
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  test('renders battery info when available', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 75, state: 'Discharging' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('75% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders nothing when battery percentage is not available', async () => {
    const queryClient = createTestQueryClient();

    // mockBattery is already null from beforeEach

    const { container } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Should not render anything
      expect(container.querySelector('button')).toBeNull();
    });

    queryClient.clear();
  });

  test('renders full battery label', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 100, state: 'Full' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('100%')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders charging battery label', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 50, state: 'Charging' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('50% (Charging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders low battery label', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 15, state: 'Discharging' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('15% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders empty battery state', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 5, state: 'Empty' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('5% (Empty)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders high battery percentage', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 85, state: 'Discharging' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('85% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders medium battery percentage', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 60, state: 'Discharging' });

    const { getByText } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('60% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders battery icon', async () => {
    const queryClient = createTestQueryClient();

    mockBattery = createMockBattery({ percentage: 50, state: 'Discharging' });

    const { container } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders with undefined percentage displays nothing', async () => {
    const queryClient = createTestQueryClient();

    // mockBattery is already null from beforeEach

    const { container } = await render(<Battery />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Component renders nothing without battery data
      expect(container.querySelector('button')).toBeNull();
    });

    queryClient.clear();
  });
});
