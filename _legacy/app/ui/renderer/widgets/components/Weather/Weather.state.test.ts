import { beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import type { WeatherData } from '@/stores/WeatherStore';

import { useWeatherWidget, type WeatherStat } from './Weather.state';

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

const moonPhaseStat = (stats: WeatherStat[]) => stats.find((stat) => stat.id === 'moonPhase');

describe('useWeatherWidget', () => {
  beforeEach(() => {
    mockWeather = undefined;
  });

  test('returns null when there are no current conditions', async () => {
    const { result } = await renderHook(() => useWeatherWidget());

    expect(result.current.weather).toBeNull();
  });

  test('formats a full moon phase', async () => {
    mockWeather = createMockWeather();

    const { result } = await renderHook(() => useWeatherWidget());

    const stat = moonPhaseStat(result.current.weather?.stats ?? []);
    expect(stat?.displayValue).toBe('50');
    expect(stat?.percentage).toBe(50);
    expect(stat?.status).toBe('Full Moon');
    expect(stat?.value).toBe(0.5);
  });

  test('formats a new moon phase', async () => {
    mockWeather = createMockWeather({
      currentConditions: {
        ...createMockWeather().currentConditions,
        moonphase: 0,
      },
    });

    const { result } = await renderHook(() => useWeatherWidget());

    const stat = moonPhaseStat(result.current.weather?.stats ?? []);
    expect(stat?.displayValue).toBe('0');
    expect(stat?.status).toBe('New Moon');
  });

  test('never emits NaN for a missing or invalid moon phase', async () => {
    mockWeather = createMockWeather({
      currentConditions: {
        ...createMockWeather().currentConditions,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        moonphase: undefined as any,
      },
    });

    const { result } = await renderHook(() => useWeatherWidget());

    for (const stat of result.current.weather?.stats ?? []) {
      expect(stat.displayValue).not.toBe('NaN');
      expect(Number.isNaN(stat.percentage)).toBe(false);
      expect(stat.status).not.toContain('NaN');
    }

    const stat = moonPhaseStat(result.current.weather?.stats ?? []);
    expect(stat?.displayValue).toBe('N/A');
    expect(stat?.percentage).toBe(0);
    expect(stat?.status).toBe('New Moon');
  });

  test('limits the hourly rain forecast to the next 12 hours', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-08-18T14:00:00'));

    const hours = Array.from({ length: 24 }, (_, index) => ({
      datetime: `${String(index).padStart(2, '0')}:00`,
      temp: 20,
      precip: index % 3 === 0 ? 2 : 0,
      precipprob: 30,
      preciptype: index % 3 === 0 ? (['rain'] as string[]) : null,
      icon: 'rain',
      conditions: 'Rain',
    }));

    mockWeather = createMockWeather({
      days: [
        {
          datetime: '2026-08-18',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
          precip: 2,
          precipprob: 30,
          preciptype: ['rain'],
          snow: 0,
          snowdepth: 0,
          conditions: 'Rain',
          icon: 'rain',
          hours,
        },
      ],
    });

    const { result } = await renderHook(() => useWeatherWidget());

    expect(result.current.weather?.hourlyRainForecast.length).toBeGreaterThan(0);
    expect(result.current.weather?.hourlyRainForecast.length).toBeLessThanOrEqual(12);

    vi.useRealTimers();
  });

  test('formats OpenMeteo ISO timestamps and includes hours from the next day', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-08-18T22:30:00'));

    const hour = (datetime: string) => ({
      datetime,
      temp: 20,
      precip: 1,
      precipprob: 30,
      preciptype: ['rain'],
      icon: 'rain',
      conditions: 'Rain',
    });
    mockWeather = createMockWeather({
      days: [
        {
          datetime: '2026-08-18',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
          precip: 2,
          precipprob: 30,
          preciptype: ['rain'],
          snow: 0,
          snowdepth: 0,
          conditions: 'Rain',
          icon: 'rain',
          hours: [hour('2026-08-18T22:00'), hour('2026-08-18T23:00')],
        },
        {
          datetime: '2026-08-19',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
          precip: 2,
          precipprob: 30,
          preciptype: ['rain'],
          snow: 0,
          snowdepth: 0,
          conditions: 'Rain',
          icon: 'rain',
          hours: [hour('2026-08-19T00:00'), hour('2026-08-19T01:00')],
        },
      ],
    });

    const { result } = await renderHook(() => useWeatherWidget());

    const forecast = result.current.weather?.hourlyRainForecast ?? [];
    expect(forecast.map(({ hour: displayHour, time }) => ({ displayHour, time }))).toEqual([
      { displayHour: '10', time: '10:00 PM' },
      { displayHour: '11', time: '11:00 PM' },
      { displayHour: '12', time: '12:00 AM' },
      { displayHour: '1', time: '1:00 AM' },
    ]);
    expect(new Set(forecast.map(({ id }) => id)).size).toBe(forecast.length);

    vi.useRealTimers();
  });

  test('detects the next precipitation event', async () => {
    mockWeather = createMockWeather({
      days: [
        {
          datetime: '2026-08-18',
          temp: 20,
          tempmax: 24,
          tempmin: 16,
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

    const { result } = await renderHook(() => useWeatherWidget());

    expect(result.current.weather?.nextPrecipitation).toMatchObject({
      precipType: 'rain',
      isToday: true,
      precipProb: 60,
    });
  });
});
