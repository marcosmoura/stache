import { describe, expect, test } from 'vitest';

import { barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { Status } from './Status';

describe('Status', () => {
  test('renders only the Clock while the providers are absent', async () => {
    const { screen } = await renderWithBarData(<Status />);
    const container = screen.container.querySelector('[data-testid="status-container"]');

    expect(container?.querySelectorAll('button')).toHaveLength(1);
  });

  test('renders Weather, CPU, Battery, Keep Awake, Wi-Fi and Clock in that order', async () => {
    const { screen } = await renderWithBarData(
      <Status />,
      barData({
        weather: {
          isConfigured: true,
          isLoading: false,
          currentConditions: { feelslike: 10, icon: 'cloudy' },
        },
        cpu: { usage: 12, temperature: null },
        battery: { percentage: 80, state: 'Discharging' },
        keepAwake: true,
        wifi: { status: 'Connected', networkName: 'Home', signalStrength: -50 },
      }),
    );
    const buttons = [
      ...screen.container.querySelectorAll('[data-testid="status-container"] button'),
    ];

    expect(buttons).toHaveLength(6);
    expect(buttons[0]?.textContent).toContain('10°C');
    expect(buttons[1]?.textContent).toContain('12%');
    expect(buttons[2]?.textContent).toContain('80%');
    expect(buttons[3]?.getAttribute('data-test-state')).toBe('awake');
    expect(buttons[4]?.textContent).toContain('Home');
    expect(buttons[5]?.textContent).toMatch(/\d\d:\d\d:\d\d/);
  });

  test('lays the items out with the legacy 4px / compact 2px gap', async () => {
    const { screen } = await renderWithBarData(<Status />);
    const container = screen.container.querySelector('[data-testid="status-container"]');

    if (!container) {
      throw new Error('status missing');
    }

    expect(getComputedStyle(container).display).toBe('grid');
    // The test viewport is below 2036px, so the compact gap applies.
    expect(getComputedStyle(container).columnGap).toBe('2px');
  });
});
