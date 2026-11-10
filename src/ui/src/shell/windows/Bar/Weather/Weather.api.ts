import type { BarWeather } from '../../../../sdk';

import { translateOpenMeteoIcon, translateVisualCrossingIcon } from './Weather.codes';

/**
 * Weather HTTP, ported from legacy `WeatherStore/providers`. The key and the
 * coordinates live only in these function scopes: nothing here logs, and every
 * thrown message is a fixed string (no URL, key or coordinate).
 */

const VISUAL_CROSSING_URL =
  'https://weather.visualcrossing.com/VisualCrossingWebServices/rest/services';
const VISUAL_CROSSING_ELEMENTS =
  'name,address,resolvedAddress,temp,tempmax,tempmin,feelslike,humidity,windspeed,winddir,pressure,visibility,cloudcover,conditions,icon,moonphase,precip,precipprob,preciptype,snow,snowdepth,datetime';
const VISUAL_CROSSING_INCLUDE = 'current,hours,days';
const OPEN_METEO_URL = 'https://api.open-meteo.com/v1/forecast';
const GEOCODING_URL = 'https://geocoding-api.open-meteo.com/v1/search';

/** Upper bound of one provider request. */
export const WEATHER_REQUEST_TIMEOUT_MS = 15_000;

/** The rendered subset of legacy `NormalizedCurrentConditions`. */
export type CurrentConditions = {
  /** Feels-like temperature in Celsius; `null` renders as 0. */
  feelslike: number | null;
  /** Normalized icon key. */
  icon: string;
};

/** A failure with a fixed, secret-free message. */
export class WeatherError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'WeatherError';
  }
}

export type ProviderChoice = 'visualCrossing' | 'openMeteo';

/** Legacy `isProviderAvailable`: `auto` and `openMeteo` always work, `visualCrossing` needs a key. */
export function isProviderAvailable(provider: BarWeather['provider'], apiKey: string): boolean {
  return provider !== 'visualCrossing' || apiKey !== '';
}

/** Legacy `createWeatherProvider`; `undefined` means unconfigured. Never switches provider silently. */
export function chooseProvider(
  provider: BarWeather['provider'],
  apiKey: string,
): ProviderChoice | undefined {
  if (provider === 'openMeteo') {
    return 'openMeteo';
  }

  if (provider === 'visualCrossing') {
    return apiKey ? 'visualCrossing' : undefined;
  }

  return apiKey ? 'visualCrossing' : 'openMeteo';
}

type Coordinates = { lat: number; lon: number };

const COORDINATES = /^(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)$/;

/** Parses `lat,lon` text (legacy's own coordinate form); anything else is a place name. */
export function parseCoordinates(text: string): Coordinates | undefined {
  const match = COORDINATES.exec(text.trim());

  if (!match) {
    return undefined;
  }

  const lat = Number(match[1]);
  const lon = Number(match[2]);

  return Math.abs(lat) <= 90 && Math.abs(lon) <= 180 ? { lat, lon } : undefined;
}

type Resolved =
  | { kind: 'coordinates'; coordinates: Coordinates; text: string }
  | { kind: 'name'; text: string };

function resolveLocation(location: BarWeather['location']): Resolved {
  if (location.source === 'coreLocation') {
    const { latitude, longitude } = location;

    if (
      latitude === null ||
      longitude === null ||
      !Number.isFinite(latitude) ||
      !Number.isFinite(longitude)
    ) {
      throw new WeatherError('Weather location is unavailable');
    }

    return {
      kind: 'coordinates',
      coordinates: { lat: latitude, lon: longitude },
      text: `${latitude},${longitude}`,
    };
  }

  if (location.source === 'none') {
    throw new WeatherError('Weather location is not configured');
  }

  const query = location.query.trim();

  if (!query) {
    throw new WeatherError('Weather location is not configured');
  }

  const coordinates = parseCoordinates(query);

  return coordinates
    ? { kind: 'coordinates', coordinates, text: query }
    : { kind: 'name', text: query };
}

/** Fetches JSON with a bounded duration; every failure is a fixed message. */
async function getJson(url: string, signal: AbortSignal): Promise<unknown> {
  const controller = new AbortController();
  const abort = () => controller.abort();
  const timer = setTimeout(abort, WEATHER_REQUEST_TIMEOUT_MS);

  signal.addEventListener('abort', abort);

  if (signal.aborted) {
    abort();
  }

  try {
    const response = await fetch(url, { signal: controller.signal });

    if (!response.ok) {
      throw new WeatherError(`Weather API error: ${response.status}`);
    }

    return (await response.json()) as unknown;
  } catch (error) {
    if (error instanceof WeatherError) {
      throw error;
    }

    throw new WeatherError(
      signal.aborted
        ? 'Weather request cancelled'
        : controller.signal.aborted
          ? 'Weather request timed out'
          : 'Weather request failed',
    );
  } finally {
    clearTimeout(timer);
    signal.removeEventListener('abort', abort);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function numberOrNull(value: unknown): number | null {
  if (value === null || value === undefined) {
    return null;
  }

  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }

  throw new WeatherError('Weather response is invalid');
}

async function fetchVisualCrossing(
  resolved: Resolved,
  apiKey: string,
  signal: AbortSignal,
): Promise<CurrentConditions> {
  const params = new URLSearchParams({
    key: apiKey,
    unitGroup: 'metric',
    elements: VISUAL_CROSSING_ELEMENTS,
    include: VISUAL_CROSSING_INCLUDE,
    iconSet: 'icons2',
    contentType: 'json',
  });
  const url = `${VISUAL_CROSSING_URL}/timeline/${encodeURIComponent(resolved.text)}/next5days?${params.toString()}`;
  const data = await getJson(url, signal);
  const current = isRecord(data) ? data.currentConditions : undefined;

  if (!isRecord(current) || typeof current.icon !== 'string') {
    throw new WeatherError('Weather response is invalid');
  }

  return {
    feelslike: numberOrNull(current.feelslike),
    icon: translateVisualCrossingIcon(current.icon),
  };
}

async function geocode(name: string, signal: AbortSignal): Promise<Coordinates> {
  const params = new URLSearchParams({ name, count: '1', language: 'en', format: 'json' });
  let data: unknown;

  try {
    data = await getJson(`${GEOCODING_URL}?${params.toString()}`, signal);
  } catch (error) {
    if (signal.aborted) {
      throw error;
    }

    // Legacy collapsed every geocoding failure into one message.
    throw new WeatherError('Failed to geocode location');
  }

  const first = isRecord(data) && Array.isArray(data.results) ? (data.results[0] as unknown) : null;

  if (
    !isRecord(first) ||
    typeof first.latitude !== 'number' ||
    typeof first.longitude !== 'number' ||
    !Number.isFinite(first.latitude) ||
    !Number.isFinite(first.longitude)
  ) {
    throw new WeatherError('Failed to geocode location');
  }

  return { lat: first.latitude, lon: first.longitude };
}

async function fetchOpenMeteo(resolved: Resolved, signal: AbortSignal): Promise<CurrentConditions> {
  const { lat, lon } =
    resolved.kind === 'coordinates' ? resolved.coordinates : await geocode(resolved.text, signal);
  const params = new URLSearchParams({
    latitude: lat.toString(),
    longitude: lon.toString(),
    current: [
      'temperature_2m',
      'relative_humidity_2m',
      'apparent_temperature',
      'is_day',
      'precipitation',
      'weather_code',
      'wind_speed_10m',
      'wind_direction_10m',
      'wind_gusts_10m',
    ].join(','),
    hourly: [
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
    ].join(','),
    daily: [
      'weather_code',
      'temperature_2m_max',
      'temperature_2m_min',
      'precipitation_sum',
      'precipitation_probability_max',
      'wind_speed_10m_max',
      'sunrise',
      'sunset',
    ].join(','),
    timezone: 'auto',
    forecast_days: '5',
  });
  const data = await getJson(`${OPEN_METEO_URL}?${params.toString()}`, signal);
  const current = isRecord(data) ? data.current : undefined;

  if (!isRecord(current) || typeof current.weather_code !== 'number') {
    throw new WeatherError('Weather response is invalid');
  }

  return {
    feelslike: numberOrNull(current.apparent_temperature),
    icon: translateOpenMeteoIcon(current.weather_code, current.is_day === 1),
  };
}

/** One weather fetch for already-resolved inputs. */
export async function fetchCurrentConditions(
  choice: ProviderChoice,
  weather: Pick<BarWeather, 'location'> & { apiKey: string },
  signal: AbortSignal,
): Promise<CurrentConditions> {
  const resolved = resolveLocation(weather.location);

  return await (choice === 'visualCrossing'
    ? fetchVisualCrossing(resolved, weather.apiKey, signal)
    : fetchOpenMeteo(resolved, signal));
}
