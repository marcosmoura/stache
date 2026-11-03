import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import type { WeatherData } from '@/stores/WeatherStore';

import { Weather } from './Weather';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));

let mockWeather: WeatherData | undefined;

vi.mock('@/stores/WeatherStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/stores/WeatherStore')>();
  return {
    ...actual,
    useWeatherStore: () => ({
      weather: mockWeather,
      isLoading: false,
      isConfigured: true,
    }),
  };
});

const mockInvoke = vi.mocked(invoke);

const createHours = (precipProb: number, precipType: string[] | null = null) =>
  Array.from({ length: 24 }, (_, index) => ({
    datetime: `${String(index).padStart(2, '0')}:00`,
    temp: 20,
    precip: precipType ? 2 : 0,
    precipprob: precipProb,
    preciptype: precipType,
    icon: precipType ? 'rain' : 'clear-day',
    conditions: precipType ? 'Rain' : 'Clear',
  }));

const createMockWeather = (overrides: Partial<WeatherData> = {}): WeatherData => ({
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
    feelslike: 21,
    humidity: 65,
    dew: 15.5,
    windspeed: 12,
    winddir: 180,
    windgust: 18,
    precip: 0,
    precipprob: 10,
    preciptype: null,
    snow: 0,
    pressure: 1013,
    visibility: 10,
    cloudcover: 25,
    conditions: 'Partly Cloudy',
    icon: 'partly-cloudy-day',
    moonphase: 0.5,
  },
  days: [],
  ...overrides,
});

describe('Weather widget', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-08-18T14:00:00'));
    mockWeather = undefined;
    mockInvoke.mockClear();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  test('renders a loading state without weather data', async () => {
    const screen = await render(<Weather />);

    await expect.element(screen.getByText("Today's weather")).toBeVisible();
    await expect.element(screen.getByText('Loading weather data...')).toBeVisible();
  });

  test('renders temperature, location and stats', async () => {
    mockWeather = createMockWeather();

    const screen = await render(<Weather />);

    await expect.element(screen.getByText('23°')).toBeVisible();
    await expect.element(screen.getByText('Feels like 21°')).toBeVisible();
    await expect.element(screen.getByText('Berlin, Germany')).toBeVisible();

    await expect.element(screen.getByText('Humidity', { exact: true })).toBeVisible();
    await expect.element(screen.getByText(/^Wind/)).toBeVisible();
    await expect.element(screen.getByText('Visibility', { exact: true })).toBeVisible();
    await expect.element(screen.getByText('Cloud Cover', { exact: true })).toBeVisible();
    await expect.element(screen.getByText('Pressure', { exact: true })).toBeVisible();
    await expect.element(screen.getByText('Moon Phase', { exact: true })).toBeVisible();

    await expect.element(screen.getByText('Full Moon')).toBeVisible();
  });

  test('renders precipitation forecast bars', async () => {
    mockWeather = createMockWeather({
      currentConditions: { ...createMockWeather().currentConditions, temp: 20 },
      days: [
        {
          datetime: '2026-08-18',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
          precip: 5,
          precipprob: 40,
          preciptype: ['rain'],
          snow: 0,
          snowdepth: 0,
          conditions: 'Rain',
          icon: 'rain',
          hours: createHours(40, ['rain']),
        },
      ],
    });

    const screen = await render(<Weather />);

    await expect.element(screen.getByText('Precipitation Forecast')).toBeVisible();
    await expect.element(screen.getByText('Precipitation Forecast')).toBeVisible();
    await expect.element(screen.getByText('Very High')).toBeVisible();
  });

  test('renders the clear skies card when no precipitation is expected', async () => {
    mockWeather = createMockWeather({
      days: [
        {
          datetime: '2026-08-18',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
          precip: 0,
          precipprob: 0,
          preciptype: null,
          snow: 0,
          snowdepth: 0,
          conditions: 'Clear',
          icon: 'clear-day',
          hours: createHours(0),
        },
      ],
    });

    const screen = await render(<Weather />);

    await expect.element(screen.getByText('Clear skies ahead')).toBeVisible();
  });

  test('renders the next precipitation card', async () => {
    mockWeather = createMockWeather({
      days: [
        {
          datetime: '2026-08-18',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
          precip: 0,
          precipprob: 0,
          preciptype: null,
          snow: 0,
          snowdepth: 0,
          conditions: 'Clear',
          icon: 'clear-day',
          hours: createHours(0),
        },
        {
          datetime: '2026-08-19',
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
    });

    const screen = await render(<Weather />);

    await expect.element(screen.getByText('Rain expected tomorrow')).toBeVisible();
  });

  test('opens the weather app when the button is clicked', async () => {
    mockWeather = createMockWeather();

    const screen = await render(<Weather />);

    await screen.getByRole('button', { name: 'Open weather' }).click();

    expect(mockInvoke).toHaveBeenCalledWith('open_app', { name: 'Weather' });
  });
});
