import { Suspense } from 'react';

import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import {
  createFetchMock,
  createQueryClientWrapper,
  createTestQueryClient,
  type FetchRoute,
} from '@/tests/utils';

import type { LocationData } from './location';
import { useWeatherStore } from './WeatherStore';

import type { WeatherConfig, WeatherData } from './WeatherStore.types';

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

const createMockWeatherConfig = (overrides: Partial<WeatherConfig> = {}): WeatherConfig => ({
  visualCrossingApiKey: 'test-api-key-12345',
  defaultLocation: 'Berlin, Germany',
  ...overrides,
});

const createMockWeatherData = (overrides: Partial<WeatherData> = {}): WeatherData => ({
  queryCost: 1,
  latitude: 52.52,
  longitude: 13.405,
  resolvedAddress: 'Berlin, Germany',
  address: 'Berlin',
  timezone: 'Europe/Berlin',
  tzoffset: 1,
  currentConditions: {
    datetime: '14:00:00',
    temp: 22.5,
    feelslike: 21.0,
    humidity: 65,
    dew: 15.5,
    windspeed: 12.0,
    winddir: 180,
    windgust: 18.0,
    precip: 0,
    precipprob: 10,
    preciptype: null,
    snow: 0,
    pressure: 1013,
    visibility: 10,
    cloudcover: 25,
    solarradiation: 450,
    solarenergy: 1.2,
    conditions: 'Partly Cloudy',
    icon: 'partly-cloudy-day',
    moonphase: 0.5,
  },
  days: [],
  ...overrides,
});

/* Default routes for weather tests */
const defaultRoutes: FetchRoute[] = [
  {
    pattern: 'ipapi.co',
    response: { city: 'Berlin', country_name: 'Germany', latitude: 52.52, longitude: 13.405 },
  },
  { pattern: 'visualcrossing', response: createMockWeatherData() },
];

/* Test component that renders weather data */
const WeatherTestComponent = ({
  renderFn,
}: {
  renderFn: (data: ReturnType<typeof useWeatherStore>) => React.ReactNode;
}) => {
  const result = useWeatherStore();
  return <>{renderFn(result)}</>;
};

/* Helper to render weather store tests */
const renderWeatherTest = async (
  renderFn: (data: ReturnType<typeof useWeatherStore>) => React.ReactNode,
  options: {
    config?: Partial<WeatherConfig>;
    weather?: Partial<WeatherData>;
    routes?: FetchRoute[];
  } = {},
) => {
  const queryClient = createTestQueryClient();
  const mockConfig = createMockWeatherConfig(options.config);
  const mockWeather = createMockWeatherData(options.weather);

  mockInvoke.mockResolvedValue(mockConfig);

  const routes = options.routes ?? [
    {
      pattern: 'ipapi.co',
      response: { city: 'Berlin', country_name: 'Germany', latitude: 52.52, longitude: 13.405 },
    },
    { pattern: 'visualcrossing', response: mockWeather },
  ];

  const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation(createFetchMock(routes));
  const SuspenseWrapper = createQueryClientWrapper(queryClient);

  const screen = await render(
    <SuspenseWrapper>
      <Suspense fallback={<div data-testid="loading">Loading...</div>}>
        <WeatherTestComponent renderFn={renderFn} />
      </Suspense>
    </SuspenseWrapper>,
  );

  return { screen, fetchSpy, queryClient };
};

describe('useWeatherStore', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  describe('config loading', () => {
    test('fetches and exposes config', async () => {
      const { screen } = await renderWeatherTest(
        ({ config }) => (
          <div>
            <span data-testid="api-key">{config?.visualCrossingApiKey}</span>
            <span data-testid="location">{config?.defaultLocation}</span>
          </div>
        ),
        { config: { defaultLocation: 'Tokyo, Japan' } },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('api-key')).toHaveTextContent('test-api-key-12345');
      });

      expect(mockInvoke).toHaveBeenCalledWith('get_weather_config', undefined);
      expect(screen.getByTestId('location')).toHaveTextContent('Tokyo, Japan');
    });
  });

  describe('location detection', () => {
    test('uses IP-based location from ipapi.co', async () => {
      const { screen } = await renderWeatherTest(
        ({ location }) => (
          <div data-testid="location">{(location as LocationData)?.displayName}</div>
        ),
        {
          routes: [
            {
              pattern: 'ipapi.co',
              response: {
                city: 'Munich',
                country_name: 'Germany',
                latitude: 48.1351,
                longitude: 11.582,
              },
            },
            { pattern: 'visualcrossing', response: createMockWeatherData() },
          ],
        },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('location')).toHaveTextContent('Munich, Germany');
      });
    });

    test('falls back to ipinfo.io when ipapi.co fails', async () => {
      const { screen } = await renderWeatherTest(
        ({ location }) => (
          <div data-testid="location">{(location as LocationData)?.displayName}</div>
        ),
        {
          routes: [
            { pattern: 'ipapi.co', shouldFail: true, response: null },
            {
              pattern: 'ipinfo.io',
              response: { city: 'Hamburg', country: 'DE', loc: '53.5511,9.9937' },
            },
            { pattern: 'visualcrossing', response: createMockWeatherData() },
          ],
        },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('location')).toHaveTextContent('Hamburg, DE');
      });
    });

    test('falls back to default location when all services fail', async () => {
      const { screen } = await renderWeatherTest(
        ({ location }) => (
          <div data-testid="location">{(location as LocationData)?.displayName}</div>
        ),
        {
          config: { defaultLocation: 'Default City' },
          routes: [
            { pattern: 'ipapi.co', shouldFail: true, response: null },
            { pattern: 'ipinfo.io', shouldFail: true, response: null },
            { pattern: 'visualcrossing', response: createMockWeatherData() },
          ],
        },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('location')).toHaveTextContent('Default City');
      });
    });
  });

  describe('weather data', () => {
    test('fetches and exposes current conditions', async () => {
      const { screen } = await renderWeatherTest(
        ({ weather }) => (
          <div>
            <span data-testid="temp">{weather?.currentConditions?.temp}</span>
            <span data-testid="feelslike">{weather?.currentConditions?.feelslike}</span>
            <span data-testid="humidity">{weather?.currentConditions?.humidity}</span>
            <span data-testid="conditions">{weather?.currentConditions?.conditions}</span>
            <span data-testid="icon">{weather?.currentConditions?.icon}</span>
          </div>
        ),
        {
          weather: {
            currentConditions: {
              datetime: '14:00:00',
              temp: 25.0,
              feelslike: 26.0,
              humidity: 50,
              dew: 14.0,
              windspeed: 8.0,
              winddir: 270,
              windgust: 12.0,
              precip: 0,
              precipprob: 0,
              preciptype: null,
              snow: 0,
              pressure: 1015,
              visibility: 15,
              cloudcover: 10,
              solarradiation: 600,
              conditions: 'Clear',
              icon: 'clear-day',
              moonphase: 0.25,
            },
          },
        },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('temp')).toHaveTextContent('25');
      });

      expect(screen.getByTestId('feelslike')).toHaveTextContent('26');
      expect(screen.getByTestId('humidity')).toHaveTextContent('50');
      expect(screen.getByTestId('conditions')).toHaveTextContent('Clear');
      expect(screen.getByTestId('icon')).toHaveTextContent('clearDay');
    });

    test('exposes forecast days', async () => {
      const { screen } = await renderWeatherTest(
        ({ weather }) => (
          <div>
            <span data-testid="days-count">{weather?.days?.length}</span>
            <span data-testid="day-0-max">{weather?.days?.[0]?.tempmax}</span>
            <span data-testid="day-1-conditions">{weather?.days?.[1]?.conditions}</span>
          </div>
        ),
        {
          weather: {
            days: [
              {
                datetime: '2026-01-08',
                temp: 20,
                tempmax: 24,
                tempmin: 16,
                precip: 0,
                precipprob: 5,
                preciptype: null,
                snow: 0,
                snowdepth: 0,
                conditions: 'Clear',
                icon: 'clear-day',
                hours: [],
              },
              {
                datetime: '2026-01-09',
                temp: 18,
                tempmax: 22,
                tempmin: 14,
                precip: 5,
                precipprob: 60,
                preciptype: ['rain'],
                snow: 0,
                snowdepth: 0,
                conditions: 'Rain',
                icon: 'rain',
                hours: [],
              },
            ],
          },
        },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('days-count')).toHaveTextContent('2');
      });

      expect(screen.getByTestId('day-0-max')).toHaveTextContent('24');
      expect(screen.getByTestId('day-1-conditions')).toHaveTextContent('Rain');
    });

    test('exposes address information', async () => {
      const { screen } = await renderWeatherTest(
        ({ weather }) => (
          <div>
            <span data-testid="resolved">{weather?.resolvedAddress}</span>
            <span data-testid="timezone">{weather?.timezone}</span>
          </div>
        ),
        {
          weather: {
            resolvedAddress: 'Berlin, Brandenburg, Germany',
            timezone: 'Europe/Berlin',
          },
        },
      );

      await vi.waitFor(async () => {
        await expect
          .element(screen.getByTestId('resolved'))
          .toHaveTextContent('Berlin, Brandenburg, Germany');
      });

      expect(screen.getByTestId('timezone')).toHaveTextContent('Europe/Berlin');
    });

    test('includes correct API parameters in request', async () => {
      let capturedUrl = '';

      const queryClient = createTestQueryClient();
      const mockConfig = createMockWeatherConfig({
        visualCrossingApiKey: 'my-secret-key',
        provider: 'visual-crossing',
      });
      mockInvoke.mockResolvedValue(mockConfig);

      const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
        const url = String(input);
        if (url.includes('visualcrossing')) capturedUrl = url;
        return createFetchMock(defaultRoutes)(input);
      });

      const SuspenseWrapper = createQueryClientWrapper(queryClient);
      const screen = await render(
        <SuspenseWrapper>
          <Suspense fallback={<div>Loading...</div>}>
            <WeatherTestComponent
              renderFn={({ weather }) => <div data-testid="loaded">{weather ? 'yes' : 'no'}</div>}
            />
          </Suspense>
        </SuspenseWrapper>,
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('loaded')).toHaveTextContent('yes');
      });

      expect(capturedUrl).toContain('key=my-secret-key');
      expect(capturedUrl).toContain('unitGroup=metric');
      expect(capturedUrl).toContain('iconSet=icons2');

      fetchSpy.mockRestore();
    });
  });

  describe('loading states', () => {
    test('does not fetch weather without API key', async () => {
      const { screen, fetchSpy } = await renderWeatherTest(
        ({ weather, config }) => (
          <div>
            <span data-testid="config-loaded">{config ? 'yes' : 'no'}</span>
            <span data-testid="weather-loaded">{weather ? 'yes' : 'no'}</span>
          </div>
        ),
        {
          config: { visualCrossingApiKey: '', provider: 'visual-crossing' },
          routes: [{ pattern: 'ipapi.co', response: { city: 'Berlin', country_name: 'Germany' } }],
        },
      );

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('config-loaded')).toHaveTextContent('yes');
      });

      expect(screen.getByTestId('weather-loaded')).toHaveTextContent('no');

      const weatherCalls = fetchSpy.mock.calls.filter((call: [RequestInfo | URL, RequestInit?]) =>
        String(call[0]).includes('visualcrossing'),
      );
      expect(weatherCalls).toHaveLength(0);
    });
  });

  describe('hook interface', () => {
    test('returns all expected properties', async () => {
      let captured: ReturnType<typeof useWeatherStore> | null = null;

      const { screen } = await renderWeatherTest((result) => {
        captured = result;
        return <div data-testid="done">Done</div>;
      });

      await vi.waitFor(async () => {
        await expect.element(screen.getByTestId('done')).toBeVisible();
      });

      expect(captured).toHaveProperty('config');
      expect(captured).toHaveProperty('location');
      expect(captured).toHaveProperty('weather');
      expect(captured).toHaveProperty('isLoading');
    });
  });
});
