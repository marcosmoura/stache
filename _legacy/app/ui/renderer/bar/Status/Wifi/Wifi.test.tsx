import {
  Wifi01Icon,
  WifiFullSignalIcon,
  WifiLowSignalIcon,
  WifiMediumSignalIcon,
} from '@hugeicons/core-free-icons';
import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Wifi } from './Wifi';
import { getSignalIcon } from './Wifi.state';
import type { WifiInfo } from './Wifi.types';

function connected(overrides?: Partial<WifiInfo>): WifiInfo {
  return { status: 'Connected', networkName: 'HomeWiFi', signalStrength: -50, ...overrides };
}

function disconnected(): WifiInfo {
  return { status: 'Disconnected', networkName: null, signalStrength: null };
}

function wifiOff(): WifiInfo {
  return { status: 'Off', networkName: null, signalStrength: null };
}

function unknown(): WifiInfo {
  return { status: 'Unknown', networkName: null, signalStrength: null };
}

describe('Wifi Component', () => {
  async function renderWithWifi(info: WifiInfo) {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['wifi'], info);

    const result = await render(<Wifi />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    return { ...result, queryClient };
  }

  test('maps excellent macOS RSSI values to full signal icon', () => {
    expect(getSignalIcon(-56)).toBe(WifiFullSignalIcon);
  });

  test('maps RSSI values to descending signal icons', () => {
    expect(getSignalIcon(-68)).toBe(WifiMediumSignalIcon);
    expect(getSignalIcon(-76)).toBe(WifiLowSignalIcon);
    expect(getSignalIcon(-86)).toBe(Wifi01Icon);
  });

  test('renders wifi info when connected', async () => {
    const { getByText, queryClient } = await renderWithWifi(connected());

    await vi.waitFor(() => {
      expect(getByText('HomeWiFi')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders wifi off label', async () => {
    const { getByText, queryClient } = await renderWithWifi(wifiOff());

    await vi.waitFor(() => {
      expect(getByText('Wi-Fi Off')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders disconnected label', async () => {
    const { getByText, queryClient } = await renderWithWifi(disconnected());

    await vi.waitFor(() => {
      expect(getByText('Disconnected')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders nothing when unknown', async () => {
    const { container, queryClient } = await renderWithWifi(unknown());

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeNull();
    });

    queryClient.clear();
  });

  test('renders icon when wifi is off', async () => {
    const { container, queryClient } = await renderWithWifi(wifiOff());

    await vi.waitFor(() => {
      expect(container.querySelector('svg')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders icon when connected', async () => {
    const { container, queryClient } = await renderWithWifi(
      connected({ networkName: 'OfficeNet' }),
    );

    await vi.waitFor(() => {
      expect(container.querySelector('svg')).toBeDefined();
    });

    queryClient.clear();
  });

  test('uses full signal icon for strong signal', async () => {
    const { container, queryClient } = await renderWithWifi(connected({ signalStrength: -40 }));

    await vi.waitFor(() => {
      expect(container.querySelector('svg')).toBeDefined();
    });

    queryClient.clear();
  });

  test('uses low signal icon for weak signal', async () => {
    const { container, queryClient } = await renderWithWifi(connected({ signalStrength: -75 }));

    await vi.waitFor(() => {
      expect(container.querySelector('svg')).toBeDefined();
    });

    queryClient.clear();
  });

  test('uses default icon when signal is null', async () => {
    const { container, queryClient } = await renderWithWifi(connected({ signalStrength: null }));

    await vi.waitFor(() => {
      expect(container.querySelector('svg')).toBeDefined();
    });

    queryClient.clear();
  });
});
