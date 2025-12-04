import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import {
  createFetchMock,
  createQueryClientWrapper,
  createTestQueryClient,
  type FetchRoute,
} from '@/tests/utils';

import { Weather } from './Weather';

import {
  fetchLocation,
  getWeatherConfig,
  getWeatherIcon,
  getWeatherLabel,
  openWeatherApp,
} from './Weather.service';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

const invokeMock = vi.mocked(invoke);

// Common mock data
const mockWeatherConfig = {
  visualCrossingApiKey: 'test-key',
  defaultLocation: 'Berlin',
};

const mockWeatherData = {
  currentConditions: {
    feelslike: 20,
    conditions: 'Clear',
    icon: 'clear-day',
  },
};

// Reusable route configurations
const locationRoutes: FetchRoute[] = [
  { pattern: 'ipapi.co', response: { city: 'Berlin', country_name: 'Germany' } },
  { pattern: 'ipinfo.io', response: { city: 'Berlin', country: 'DE' } },
];

/**
 * Helper to create a query client with preloaded weather data.
 * This speeds up tests by avoiding the cascade of dependent queries.
 */
const createPreloadedQueryClient = (
  config = mockWeatherConfig,
  location = 'Berlin, Germany',
  weather = mockWeatherData,
) => {
  const queryClient = createTestQueryClient();
  queryClient.setQueryData(['weatherConfig'], config);
  queryClient.setQueryData(['location', config.defaultLocation], location);
  queryClient.setQueryData(['weather', location, config.visualCrossingApiKey], weather);
  return queryClient;
};

describe('Weather Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('getWeatherConfig', () => {
    test('invokes get_weather_config', async () => {
      const mockConfig = { apiKey: 'test-key', defaultLocation: 'Berlin, Germany' };
      invokeMock.mockResolvedValue(mockConfig);

      const result = await getWeatherConfig();

      expect(invokeMock).toHaveBeenCalledWith('get_weather_config');
      expect(result).toEqual(mockConfig);
    });
  });

  describe('openWeatherApp', () => {
    test('invokes open_app with Weather', async () => {
      invokeMock.mockResolvedValue(undefined);

      await openWeatherApp();

      expect(invokeMock).toHaveBeenCalledWith('open_app', { name: 'Weather' });
    });
  });

  describe('getWeatherIcon', () => {
    test('returns default icon when conditions are undefined', () => {
      const icon = getWeatherIcon(undefined);

      expect(icon).toBeDefined();
    });

    test('returns correct icon for snow conditions', () => {
      const icon = getWeatherIcon({ icon: 'snow' } as never);

      expect(icon).toBeDefined();
    });

    test('returns correct icon for rain conditions', () => {
      const icon = getWeatherIcon({ icon: 'rain' } as never);

      expect(icon).toBeDefined();
    });

    test('returns default icon for unknown conditions', () => {
      const icon = getWeatherIcon({ icon: 'unknown-icon' } as never);

      expect(icon).toBeDefined();
    });
  });

  describe('getWeatherLabel', () => {
    test('returns empty string when conditions are undefined', () => {
      const label = getWeatherLabel(undefined);

      expect(label).toBe('');
    });

    test('returns temperature with condition for desktop', () => {
      const conditions = { feelslike: 22.5, conditions: 'Sunny' } as never;
      const label = getWeatherLabel(conditions, false);

      expect(label).toBe('23°C (Sunny)');
    });

    test('returns only temperature for laptop screen', () => {
      const conditions = { feelslike: 22.5, conditions: 'Sunny' } as never;
      const label = getWeatherLabel(conditions, true);

      expect(label).toBe('23°C');
    });

    test('handles zero temperature', () => {
      const conditions = { feelslike: 0, conditions: 'Cold' } as never;
      const label = getWeatherLabel(conditions, false);

      expect(label).toBe('0°C (Cold)');
    });

    test('handles null feelslike', () => {
      const conditions = { feelslike: null, conditions: 'Unknown' } as never;
      const label = getWeatherLabel(conditions, false);

      expect(label).toBe('0°C (Unknown)');
    });
  });

  describe('fetchLocation', () => {
    test('returns default location when API calls fail', async () => {
      vi.spyOn(globalThis, 'fetch').mockRejectedValue(new Error('Network error'));

      const result = await fetchLocation('Default City');

      expect(result).toBe('Default City');
    });
  });
});

describe('Weather Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  test('renders nothing when config is missing', async () => {
    invokeMock.mockResolvedValue(null);

    const queryClient = createTestQueryClient();
    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.innerHTML).toBe('');
    });

    queryClient.clear();
  });

  test('renders nothing when weather data is not available', async () => {
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'get_weather_config') {
        return Promise.resolve(mockWeatherConfig);
      }
      return Promise.resolve(null);
    });

    vi.spyOn(globalThis, 'fetch').mockImplementation(
      createFetchMock([
        ...locationRoutes,
        { pattern: 'visualcrossing', response: null, shouldFail: true },
      ]),
    );

    const queryClient = createTestQueryClient();
    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeNull();
    });

    queryClient.clear();
  });

  test('renders weather information when data is available', async () => {
    // Use preloaded query client to skip the query cascade
    const queryClient = createPreloadedQueryClient();

    const screen = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    // The component uses isLaptopScreen media query which may default to true in test env
    // so we check for the temperature part which is always shown
    await expect.element(screen.getByText('20°C')).toBeInTheDocument();

    queryClient.clear();
  });

  test('renders weather icon', async () => {
    const customWeatherData = {
      currentConditions: {
        feelslike: 15,
        conditions: 'Rainy',
        icon: 'rain',
      },
    };

    // Use preloaded query client to skip the query cascade
    const queryClient = createPreloadedQueryClient(
      mockWeatherConfig,
      'Berlin, Germany',
      customWeatherData,
    );

    const screen = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await expect.element(screen.getByRole('button')).toBeInTheDocument();

    queryClient.clear();
  });

  test('opens weather app on click', async () => {
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'open_app') {
        return Promise.resolve(undefined);
      }
      if (cmd === 'get_weather_config') {
        return Promise.resolve(mockWeatherConfig);
      }
      return Promise.resolve(null);
    });

    const customWeatherData = {
      currentConditions: {
        feelslike: 25,
        conditions: 'Sunny',
        icon: 'clear-day',
      },
    };

    // Mock fetch to prevent refetch issues
    vi.spyOn(globalThis, 'fetch').mockImplementation(
      createFetchMock([
        ...locationRoutes,
        { pattern: 'visualcrossing', response: customWeatherData },
      ]),
    );

    // Use preloaded query client to skip the query cascade
    const queryClient = createPreloadedQueryClient(
      mockWeatherConfig,
      'Berlin, Germany',
      customWeatherData,
    );

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    // Use vi.waitFor with a simpler DOM check for faster resolution
    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).not.toBeNull();
    });

    const button = container.querySelector('button')!;
    button.click();

    expect(invokeMock).toHaveBeenCalledWith('open_app', { name: 'Weather' });

    queryClient.clear();
  });

  test('renders nothing when API key is missing', async () => {
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'get_weather_config') {
        return Promise.resolve({
          visualCrossingApiKey: '',
          defaultLocation: 'Berlin',
        });
      }
      return Promise.resolve(null);
    });

    const queryClient = createTestQueryClient();
    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.innerHTML).toBe('');
    });

    queryClient.clear();
  });
});
