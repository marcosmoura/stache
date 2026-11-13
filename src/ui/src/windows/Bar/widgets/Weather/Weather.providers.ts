import type { BarWeatherProvider } from '@/ipc';

import { visualCrossingCondition, wmoCondition, type WeatherCondition } from './Weather.icons';
import type { Coordinates, ResolvedLocation } from './Weather.location';

/**
 * The two weather providers, fetched from the webview. The key and the
 * coordinates live only in these function scopes: nothing here logs, and
 * every thrown message is a fixed string (no URL, key or coordinate).
 */

export const VISUAL_CROSSING_URL =
  'https://weather.visualcrossing.com/VisualCrossingWebServices/rest/services/timeline';
export const OPEN_METEO_URL = 'https://api.open-meteo.com/v1/forecast';
export const GEOCODING_URL = 'https://geocoding-api.open-meteo.com/v1/search';

const VISUAL_CROSSING_ELEMENTS =
  'name,address,resolvedAddress,temp,tempmax,tempmin,feelslike,humidity,windspeed,winddir,pressure,visibility,cloudcover,conditions,icon,moonphase,precip,precipprob,preciptype,snow,snowdepth,datetime';

const OPEN_METEO_CURRENT = [
  'temperature_2m',
  'relative_humidity_2m',
  'apparent_temperature',
  'is_day',
  'precipitation',
  'weather_code',
  'wind_speed_10m',
  'wind_direction_10m',
  'wind_gusts_10m',
].join(',');
const OPEN_METEO_HOURLY = [
  'temperature_2m',
  'relative_humidity_2m',
  'precipitation_probability',
  'precipitation',
  'weather_code',
  'cloud_cover',
  'wind_speed_10m',
  'wind_direction_10m',
  'wind_gusts_10m',
  'visibility',
  'pressure_msl',
].join(',');
const OPEN_METEO_DAILY = [
  'weather_code',
  'temperature_2m_max',
  'temperature_2m_min',
  'precipitation_sum',
  'precipitation_probability_max',
  'wind_speed_10m_max',
  'sunrise',
  'sunset',
].join(',');

/** Upper bound of one provider request. */
export const WEATHER_REQUEST_TIMEOUT_MS = 15_000;

/** The rendered subset of a provider answer. */
export type CurrentConditions = {
  /** Feels-like temperature in Celsius; `null` renders as 0. */
  feelslike: number | null;
  condition: WeatherCondition;
};

/** A failure with a fixed, secret-free message. */
export class WeatherError extends Error {
  /** The configured refresh interval, once `bar_weather` reported it (not secret). */
  refreshIntervalMinutes: number | null = null;

  constructor(message: string) {
    super(message);
    this.name = 'WeatherError';
  }
}

export type ProviderChoice = 'visualCrossing' | 'openMeteo';

/**
 * The provider to use, or `null` when unconfigured: `auto` is Visual Crossing
 * with a non-empty key and Open Meteo otherwise; `visualCrossing` needs a key;
 * `openMeteo` never uses one.
 */
export function chooseProvider(
  provider: BarWeatherProvider,
  apiKey: string,
): ProviderChoice | null {
  switch (provider) {
    case 'openMeteo':
      return 'openMeteo';
    case 'visualCrossing':
      return apiKey ? 'visualCrossing' : null;
    default:
      return apiKey ? 'visualCrossing' : 'openMeteo';
  }
}

/** One bounded, abortable GET returning JSON; every failure is a fixed message. */
async function getJson(url: string, signal: AbortSignal): Promise<unknown> {
  const controller = new AbortController();
  const abort = () => controller.abort();
  const timer = setTimeout(abort, WEATHER_REQUEST_TIMEOUT_MS);

  signal.addEventListener('abort', abort);

  if (signal.aborted) {
    abort();
  }

  try {
    if (signal.aborted) {
      throw new WeatherError('Weather request cancelled');
    }

    const response = await fetch(url, { signal: controller.signal });

    if (!response.ok) {
      throw new WeatherError(`Weather API error: ${response.status}`);
    }

    const text = await response.text();

    try {
      return JSON.parse(text) as unknown;
    } catch {
      throw new WeatherError('Weather response is invalid');
    }
  } catch (error) {
    if (error instanceof WeatherError) {
      throw error;
    }

    if (signal.aborted) {
      throw new WeatherError('Weather request cancelled');
    }

    if (controller.signal.aborted) {
      throw new WeatherError('Weather request timed out');
    }

    throw new WeatherError('Weather request failed');
  } finally {
    clearTimeout(timer);
    signal.removeEventListener('abort', abort);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value);
}

function feelsLike(value: unknown): number | null {
  if (value === null || value === undefined) {
    return null;
  }

  if (isFiniteNumber(value)) {
    return value;
  }

  throw new WeatherError('Weather response is invalid');
}

/** Visual Crossing timeline for `location` (coordinates text or a place name, as typed). */
export async function fetchVisualCrossing(
  location: ResolvedLocation,
  apiKey: string,
  signal: AbortSignal,
): Promise<CurrentConditions> {
  const params = new URLSearchParams({
    key: apiKey,
    unitGroup: 'metric',
    elements: VISUAL_CROSSING_ELEMENTS,
    include: 'current,hours,days',
    iconSet: 'icons2',
    contentType: 'json',
  });
  const url = `${VISUAL_CROSSING_URL}/${encodeURIComponent(location.text)}/next5days?${params.toString()}`;
  const data = await getJson(url, signal);
  const current = isRecord(data) ? data.currentConditions : undefined;

  if (!isRecord(current) || typeof current.icon !== 'string') {
    throw new WeatherError('Weather response is invalid');
  }

  return {
    feelslike: feelsLike(current.feelslike),
    condition: visualCrossingCondition(current.icon),
  };
}

/** The first Open Meteo geocoding result for a place name. */
export async function geocode(name: string, signal: AbortSignal): Promise<Coordinates> {
  const params = new URLSearchParams({ name, count: '1', language: 'en', format: 'json' });
  let data: unknown;

  try {
    data = await getJson(`${GEOCODING_URL}?${params.toString()}`, signal);
  } catch (error) {
    if (signal.aborted) {
      throw error;
    }

    throw new WeatherError('Failed to geocode location');
  }

  const first = isRecord(data) && Array.isArray(data.results) ? (data.results[0] as unknown) : null;

  if (!isRecord(first) || !isFiniteNumber(first.latitude) || !isFiniteNumber(first.longitude)) {
    throw new WeatherError('Failed to geocode location');
  }

  return { lat: first.latitude, lon: first.longitude };
}

/** Open Meteo forecast; a place name is geocoded first. */
export async function fetchOpenMeteo(
  location: ResolvedLocation,
  signal: AbortSignal,
): Promise<CurrentConditions> {
  const { lat, lon } =
    location.kind === 'coordinates' ? location.coordinates : await geocode(location.text, signal);
  const params = new URLSearchParams({
    latitude: String(lat),
    longitude: String(lon),
    current: OPEN_METEO_CURRENT,
    hourly: OPEN_METEO_HOURLY,
    daily: OPEN_METEO_DAILY,
    timezone: 'auto',
    forecast_days: '5',
  });
  const data = await getJson(`${OPEN_METEO_URL}?${params.toString()}`, signal);
  const current = isRecord(data) ? data.current : undefined;

  if (!isRecord(current) || !isFiniteNumber(current.weather_code)) {
    throw new WeatherError('Weather response is invalid');
  }

  return {
    feelslike: feelsLike(current.apparent_temperature),
    condition: wmoCondition(current.weather_code, current.is_day === 1),
  };
}
