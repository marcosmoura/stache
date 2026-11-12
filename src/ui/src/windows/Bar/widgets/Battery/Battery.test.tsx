import {
  BatteryChargingIcon,
  BatteryEmptyIcon,
  BatteryFullIcon,
  BatteryLowIcon,
  BatteryMedium02Icon,
  BatteryMediumIcon,
} from '@hugeicons/core-free-icons';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import type { BatteryChargeState } from '@/ipc';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';

import { Battery } from './Battery';
import { batteryIcon, batteryLabel, batteryTone, selectBattery } from './Battery.state';

afterEach(async () => {
  vi.restoreAllMocks();
  await page.viewport(414, 896);
});

describe('Battery state', () => {
  it('labels each state', () => {
    expect(batteryLabel({}, false)).toBe('Loading...');
    expect(batteryLabel({ percentage: 50 }, false)).toBe('Loading...');
    expect(batteryLabel({ state: 'Charging' }, false)).toBe('Loading...');
    expect(batteryLabel({ percentage: 100, state: 'Full' }, false)).toBe('100%');
    expect(batteryLabel({ percentage: 100, state: 'Full' }, true)).toBe('100%');
    expect(batteryLabel({ percentage: 42, state: 'Unknown' }, false)).toBe('42%');
    expect(batteryLabel({ percentage: 42, state: 'Discharging' }, false)).toBe('42% (Discharging)');
    expect(batteryLabel({ percentage: 42, state: 'Charging' }, false)).toBe('42% (Charging)');
    expect(batteryLabel({ percentage: 0, state: 'Empty' }, false)).toBe('0% (Empty)');
    expect(batteryLabel({ percentage: 42, state: 'Discharging' }, true)).toBe('42%');
  });

  it('picks the icon by charge, at each step boundary', () => {
    const table: Array<[number, BatteryChargeState, unknown]> = [
      [10, 'Charging', BatteryChargingIcon],
      [100, 'Charging', BatteryChargingIcon],
      [100, 'Full', BatteryFullIcon],
      [99, 'Discharging', BatteryMedium02Icon],
      [75, 'Discharging', BatteryMedium02Icon],
      [74, 'Discharging', BatteryMediumIcon],
      [50, 'Discharging', BatteryMediumIcon],
      [49, 'Discharging', BatteryLowIcon],
      [25, 'Discharging', BatteryLowIcon],
      [24, 'Discharging', BatteryEmptyIcon],
      [0, 'Empty', BatteryEmptyIcon],
    ];

    for (const [percentage, state, icon] of table) {
      expect(batteryIcon({ percentage, state }), `${percentage} ${state}`).toBe(icon);
    }

    expect(batteryIcon({})).toBe(BatteryEmptyIcon);
  });

  it('colors by state', () => {
    expect(batteryTone({ state: 'Charging' })).toBe('success');
    expect(batteryTone({ state: 'Discharging' })).toBe('warning');
    expect(batteryTone({ state: 'Empty' })).toBe('danger');
    expect(batteryTone({ state: 'Full' })).toBe('text');
    expect(batteryTone({ state: 'Unknown' })).toBe('text');
  });

  it('selects the reading only while available', () => {
    expect(selectBattery(barAvailable('1'))).toBeNull();
    expect(selectBattery(barUnavailable('1'))).toBeNull();
    expect(
      selectBattery(barAvailable('1', { battery: { percentage: 5, state: 'Empty' } })),
    ).toEqual({ percentage: 5, state: 'Empty' });
  });
});

describe('Battery', () => {
  it('is hidden without a reading (no battery, or not sampled yet)', async () => {
    const { screen } = await renderWithSnapshot(
      <Battery id="battery" zone="end" options={{}} />,
      barAvailable('1', { battery: null }),
    );

    await new Promise((done) => setTimeout(done, 20));
    expect(screen.container.childElementCount).toBe(0);
  });

  it('renders each state and follows compact mode', async () => {
    await page.viewport(2400, 200);
    const { screen, setView } = await renderWithSnapshot(
      <Battery id="battery" zone="end" options={{}} />,
      barAvailable('1', { battery: { percentage: 80, state: 'Discharging' } }),
    );
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('80% (Discharging)');
    setView({ battery: { percentage: 81, state: 'Charging' } });
    await expect.element(button).toHaveTextContent('81% (Charging)');
    setView({ battery: { percentage: 100, state: 'Full' } });
    await expect.element(button).toHaveTextContent('100%');
    setView({ battery: { percentage: 60, state: 'Unknown' } });
    await expect.element(button).toHaveTextContent('60%');
    setView({ battery: { percentage: 30, state: 'Discharging' } });
    await expect.element(button).toHaveTextContent('30% (Discharging)');

    await page.viewport(2036, 200);
    await expect.element(button).toHaveTextContent('30%');
    await page.viewport(2037, 200);
    await expect.element(button).toHaveTextContent('30% (Discharging)');
  });

  it('sends bar_action("battery") on click', async () => {
    const host = mockHost().answer('bar_action', { kind: 'completed' });
    const { screen } = await renderWithSnapshot(
      <Battery id="battery" zone="end" options={{}} />,
      barAvailable('1', { battery: { percentage: 50, state: 'Discharging' } }),
    );

    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
    expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'battery' });
  });
});
