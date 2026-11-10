import { describe, expect, test } from 'vitest';

import { barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { Wifi } from './Wifi';
import { getSignalIcon } from './Wifi.state';
import type { WifiInfo } from './Wifi.types';

async function renderWifi(wifi: WifiInfo | null | undefined) {
  return renderWithBarData(<Wifi />, barData({ wifi }));
}

describe('Wifi', () => {
  test.each([
    ['unresolved', undefined],
    ['null', null],
    ['Unknown', { status: 'Unknown', networkName: null, signalStrength: null }],
  ] satisfies [string, WifiInfo | null | undefined][])(
    'renders nothing when %s',
    async (_, wifi) => {
      const { screen } = await renderWifi(wifi);

      expect(screen.container.querySelector('button')).toBeNull();
    },
  );

  test('renders the off state in red', async () => {
    const { screen } = await renderWifi({ status: 'Off', networkName: null, signalStrength: null });

    await expect.element(screen.getByText('Wi-Fi Off')).toBeVisible();
    expect(screen.container.querySelector('svg')?.getAttribute('color')).toBe('#f38ba8');
  });

  test('renders the disconnected state in yellow', async () => {
    const { screen } = await renderWifi({
      status: 'Disconnected',
      networkName: null,
      signalStrength: null,
    });

    await expect.element(screen.getByText('Disconnected')).toBeVisible();
    expect(screen.container.querySelector('svg')?.getAttribute('color')).toBe('#f9e2af');
  });

  test('renders the network name in sky blue', async () => {
    const { screen } = await renderWifi({
      status: 'Connected',
      networkName: 'Home',
      signalStrength: -50,
    });

    await expect.element(screen.getByText('Home')).toBeVisible();
    expect(screen.container.querySelector('svg')?.getAttribute('color')).toBe('#89dceb');
  });

  test('renders the icon only for a connected network without a name', async () => {
    const { screen } = await renderWifi({
      status: 'Connected',
      networkName: null,
      signalStrength: -50,
    });

    expect(screen.container.querySelector('button svg')).not.toBeNull();
    expect(screen.container.querySelector('button span')).toBeNull();
  });

  test('maps signal strength to the legacy icon thresholds', () => {
    const strong = getSignalIcon(-60);
    const medium = getSignalIcon(-61);
    const low = getSignalIcon(-71);
    const none = getSignalIcon(-81);

    expect(getSignalIcon(null)).toBe(none);
    expect(getSignalIcon(-70)).toBe(medium);
    expect(getSignalIcon(-80)).toBe(low);
    expect(new Set([strong, medium, low, none]).size).toBe(4);
  });
});
