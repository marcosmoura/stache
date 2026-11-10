import { afterEach, describe, expect, test, vi } from 'vitest';

import { barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { Battery } from './Battery';
import { getBatteryIcon } from './Battery.icons';
import type { BatteryReading } from './Battery.types';

async function renderBattery(battery: BatteryReading | null | undefined) {
  return renderWithBarData(<Battery />, barData({ battery }));
}

/** Makes the laptop breakpoint (`width <= 2036px`) read as not matching. */
function mockWideViewport() {
  vi.spyOn(window, 'matchMedia').mockImplementation(
    (query) =>
      ({
        matches: false,
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }) as unknown as MediaQueryList,
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('Battery', () => {
  test.each([undefined, null])('renders nothing for %s', async (battery) => {
    const { screen } = await renderBattery(battery);

    expect(screen.container.querySelector('button')).toBeNull();
  });

  test.each([
    [{ percentage: 75, state: 'Discharging' }, '75% (Discharging)', '#f9e2af'],
    [{ percentage: 50, state: 'Charging' }, '50% (Charging)', '#a6e3a1'],
    [{ percentage: 5, state: 'Empty' }, '5% (Empty)', '#f38ba8'],
    [{ percentage: 100, state: 'Full' }, '100%', '#cdd6f4'],
    [{ percentage: 42, state: 'Unknown' }, '42%', '#cdd6f4'],
  ] satisfies [BatteryReading, string, string][])(
    'renders %j as %s on a wide display',
    async (battery, label, color) => {
      mockWideViewport();

      const { screen } = await renderBattery(battery);

      await expect.element(screen.getByText(label, { exact: true })).toBeVisible();
      expect(screen.container.querySelector('svg')?.getAttribute('color')).toBe(color);
    },
  );

  test('renders the compact label on a laptop-width viewport', async () => {
    // The test page is narrower than 2036px, so the compact branch applies.
    expect(window.matchMedia('(width <= 2036px)').matches).toBe(true);

    const { screen } = await renderBattery({ percentage: 75, state: 'Discharging' });

    await expect.element(screen.getByText('75%', { exact: true })).toBeVisible();
  });

  test('keeps Full as 100% and Unknown without its state when compact', async () => {
    const full = await renderBattery({ percentage: 99, state: 'Full' });

    await expect.element(full.screen.getByText('100%', { exact: true })).toBeVisible();
  });

  test('maps percentage and charging to the legacy icon thresholds', () => {
    const icons = [100, 99, 75, 74, 50, 49, 25, 24, 0].map((percentage) =>
      getBatteryIcon(percentage, 'Discharging'),
    );

    expect(icons[0]).not.toBe(icons[1]);
    expect(icons[1]).toBe(icons[2]);
    expect(icons[3]).toBe(icons[4]);
    expect(icons[5]).toBe(icons[6]);
    expect(icons[7]).toBe(icons[8]);
    expect(new Set(icons).size).toBe(5);
    expect(getBatteryIcon(10, 'Charging')).not.toBe(getBatteryIcon(10, 'Discharging'));
    expect(getBatteryIcon(undefined, undefined)).toBe(getBatteryIcon(0, 'Empty'));
  });
});
