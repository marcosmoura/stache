import {
  Wifi01Icon,
  WifiDisconnected04Icon,
  WifiFullSignalIcon,
  WifiLowSignalIcon,
  WifiMediumSignalIcon,
  WifiOff02Icon,
} from '@hugeicons/core-free-icons';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';

import { Wifi } from './Wifi';
import { selectWifi, signalIcon, wifiLook } from './Wifi.state';

afterEach(async () => {
  vi.restoreAllMocks();
  await page.viewport(414, 896);
});

const label = () => document.querySelector<HTMLElement>('[data-state]');

describe('Wi-Fi state', () => {
  it('picks the signal icon at each RSSI boundary', () => {
    const table: Array<[number | null | undefined, unknown]> = [
      [-30, WifiFullSignalIcon],
      [-60, WifiFullSignalIcon],
      [-61, WifiMediumSignalIcon],
      [-70, WifiMediumSignalIcon],
      [-71, WifiLowSignalIcon],
      [-80, WifiLowSignalIcon],
      [-81, Wifi01Icon],
      [null, Wifi01Icon],
      [undefined, Wifi01Icon],
    ];

    for (const [rssi, icon] of table) {
      expect(signalIcon(rssi), String(rssi)).toBe(icon);
    }
  });

  it('maps each status to its icon, tone and label', () => {
    expect(wifiLook(null)).toBeNull();
    expect(wifiLook({ status: 'Unknown' })).toBeNull();
    expect(wifiLook({ status: 'Connected', networkName: 'Home', signalStrength: -65 })).toEqual({
      icon: WifiMediumSignalIcon,
      tone: 'highlight',
      label: 'Home',
    });
    expect(wifiLook({ status: 'Connected', networkName: null })).toEqual({
      icon: Wifi01Icon,
      tone: 'highlight',
      label: null,
    });
    expect(wifiLook({ status: 'Disconnected' })).toEqual({
      icon: WifiDisconnected04Icon,
      tone: 'warning',
      label: 'Disconnected',
    });
    expect(wifiLook({ status: 'Off' })).toEqual({
      icon: WifiOff02Icon,
      tone: 'danger',
      label: 'Wi-Fi Off',
    });
  });

  it('selects the reading only while available', () => {
    expect(selectWifi(barAvailable('1'))).toBeNull();
    expect(selectWifi(barUnavailable('1'))).toBeNull();
  });
});

describe('Wi-Fi', () => {
  it('is hidden while unknown or absent', async () => {
    const { screen, setView } = await renderWithSnapshot(
      <Wifi id="wifi" zone="end" options={{}} />,
      barAvailable('1', { wifi: null }),
    );

    await new Promise((done) => setTimeout(done, 20));
    expect(screen.container.childElementCount).toBe(0);
    setView({ wifi: { status: 'Unknown', networkName: null, signalStrength: null } });
    await new Promise((done) => setTimeout(done, 20));
    expect(screen.container.childElementCount).toBe(0);
  });

  it('renders connected, disconnected and off', async () => {
    const { screen, setView } = await renderWithSnapshot(
      <Wifi id="wifi" zone="end" options={{}} />,
      barAvailable('1', {
        wifi: { status: 'Connected', networkName: 'Home', signalStrength: -50 },
      }),
    );
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('Home');
    setView({ wifi: { status: 'Connected', networkName: null, signalStrength: -50 } });
    await expect.element(screen.getByRole('button', { name: 'Wi-Fi connected' })).toBeVisible();
    setView({ wifi: { status: 'Disconnected', networkName: null, signalStrength: null } });
    await expect.element(button).toHaveTextContent('Disconnected');
    setView({ wifi: { status: 'Off', networkName: null, signalStrength: null } });
    await expect.element(button).toHaveTextContent('Wi-Fi Off');
  });

  it('scrolls the network name only when it overflows', async () => {
    await page.viewport(2400, 200);
    const { setView } = await renderWithSnapshot(
      <Wifi id="wifi" zone="end" options={{}} />,
      barAvailable('1', {
        wifi: { status: 'Connected', networkName: 'Home', signalStrength: -50 },
      }),
    );

    await vi.waitFor(() => expect(label()?.dataset.state).toBe('static'));
    setView({
      wifi: {
        status: 'Connected',
        networkName: 'A very long network name that never fits in the Bar item',
        signalStrength: -50,
      },
    });
    await vi.waitFor(() => expect(label()?.dataset.state).toBe('scrolling'));
    expect(label()!.offsetWidth).toBeLessThanOrEqual(256 + 8);
  });

  it('narrows the label in compact mode', async () => {
    await page.viewport(2036, 200);
    await renderWithSnapshot(
      <Wifi id="wifi" zone="end" options={{}} />,
      barAvailable('1', {
        wifi: { status: 'Connected', networkName: 'x'.repeat(80), signalStrength: -50 },
      }),
    );

    await vi.waitFor(() => expect(label()?.dataset.state).toBe('scrolling'));
    expect(label()!.offsetWidth).toBeLessThanOrEqual(160 + 8);
  });

  it('sends bar_action("wifi") on click', async () => {
    const host = mockHost().answer('bar_action', { kind: 'completed' });
    const { screen } = await renderWithSnapshot(
      <Wifi id="wifi" zone="end" options={{}} />,
      barAvailable('1', { wifi: { status: 'Off', networkName: null, signalStrength: null } }),
    );

    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
    expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'wifi' });
  });
});
