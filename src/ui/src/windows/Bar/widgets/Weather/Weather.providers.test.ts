import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { stubRemoteFetch } from '../../../../../tests/fetch';

import type { ResolvedLocation } from './Weather.location';
import {
  chooseProvider,
  fetchOpenMeteo,
  fetchVisualCrossing,
  geocode,
  WEATHER_REQUEST_TIMEOUT_MS,
  WeatherError,
} from './Weather.providers';

const KEY = 'SENTINEL-KEY-0123';
const BERLIN: ResolvedLocation = {
  kind: 'coordinates',
  coordinates: { lat: 52.52, lon: 13.405 },
  text: '52.52,13.405',
};
const NAMED: ResolvedLocation = { kind: 'name', text: 'São Paulo' };

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  stubRemoteFetch(fetchMock);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const urlOf = (call = 0) => new URL(String(fetchMock.mock.calls[call]![0]));
const signal = () => new AbortController().signal;

/** Asserts a thrown error is a fixed WeatherError without any secret or coordinate. */
async function expectFixedError(promise: Promise<unknown>, message: string) {
  const error = await promise.then(
    () => null,
    (reason: unknown) => reason,
  );

  expect(error).toBeInstanceOf(WeatherError);
  expect((error as Error).message).toBe(message);

  for (const secret of [KEY, '52.52', '13.405', 'São Paulo', 'http']) {
    expect(String((error as Error).message)).not.toContain(secret);
    expect(String((error as Error).stack ?? '')).not.toContain(KEY);
  }
}

describe('provider choice', () => {
  it('follows the configured mode and the key', () => {
    expect(chooseProvider('auto', KEY)).toBe('visualCrossing');
    expect(chooseProvider('auto', '')).toBe('openMeteo');
    expect(chooseProvider('visualCrossing', KEY)).toBe('visualCrossing');
    expect(chooseProvider('visualCrossing', '')).toBeNull();
    expect(chooseProvider('openMeteo', KEY)).toBe('openMeteo');
    expect(chooseProvider('openMeteo', '')).toBe('openMeteo');
  });
});

describe('Visual Crossing', () => {
  it('requests the timeline with exactly the documented parameters', async () => {
    fetchMock.mockResolvedValue(
      json({ currentConditions: { feelslike: 12.2, icon: 'partly-cloudy-night' } }),
    );

    await expect(fetchVisualCrossing(NAMED, KEY, signal())).resolves.toEqual({
      feelslike: 12.2,
      condition: 'partlyCloudyNight',
    });

    const url = urlOf();

    expect(url.origin + url.pathname).toBe(
      'https://weather.visualcrossing.com/VisualCrossingWebServices/rest/services/timeline/S%C3%A3o%20Paulo/next5days',
    );
    expect(Object.fromEntries(url.searchParams)).toEqual({
      key: KEY,
      unitGroup: 'metric',
      elements:
        'name,address,resolvedAddress,temp,tempmax,tempmin,feelslike,humidity,windspeed,winddir,pressure,visibility,cloudcover,conditions,icon,moonphase,precip,precipprob,preciptype,snow,snowdepth,datetime',
      include: 'current,hours,days',
      iconSet: 'icons2',
      contentType: 'json',
    });
  });

  it('sends coordinates as their text', async () => {
    fetchMock.mockResolvedValue(json({ currentConditions: { feelslike: null, icon: 'rain' } }));

    await expect(fetchVisualCrossing(BERLIN, KEY, signal())).resolves.toEqual({
      feelslike: null,
      condition: 'rain',
    });
    expect(urlOf().pathname).toBe(
      '/VisualCrossingWebServices/rest/services/timeline/52.52%2C13.405/next5days',
    );
  });

  it('rejects a wrong shape with a fixed message', async () => {
    for (const body of [
      {},
      { currentConditions: null },
      { currentConditions: { feelslike: 3 } },
      { currentConditions: { icon: 'rain', feelslike: 'warm' } },
      [],
    ]) {
      fetchMock.mockResolvedValueOnce(json(body));
      await expectFixedError(
        fetchVisualCrossing(BERLIN, KEY, signal()),
        'Weather response is invalid',
      );
    }
  });
});

describe('Open Meteo', () => {
  it('requests the forecast with exactly the documented parameters', async () => {
    fetchMock.mockResolvedValue(
      json({ current: { apparent_temperature: -1.4, weather_code: 0, is_day: 0 } }),
    );

    await expect(fetchOpenMeteo(BERLIN, signal())).resolves.toEqual({
      feelslike: -1.4,
      condition: 'clearNight',
    });

    const url = urlOf();

    expect(url.origin + url.pathname).toBe('https://api.open-meteo.com/v1/forecast');
    expect(Object.fromEntries(url.searchParams)).toEqual({
      latitude: '52.52',
      longitude: '13.405',
      current:
        'temperature_2m,relative_humidity_2m,apparent_temperature,is_day,precipitation,weather_code,wind_speed_10m,wind_direction_10m,wind_gusts_10m',
      hourly:
        'temperature_2m,relative_humidity_2m,precipitation_probability,precipitation,weather_code,cloud_cover,wind_speed_10m,wind_direction_10m,wind_gusts_10m,visibility,pressure_msl',
      daily:
        'weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max,sunrise,sunset',
      timezone: 'auto',
      forecast_days: '5',
    });
    expect(url.searchParams.has('key')).toBe(false);
  });

  it('geocodes a place name first', async () => {
    fetchMock
      .mockResolvedValueOnce(json({ results: [{ latitude: -23.55, longitude: -46.63 }] }))
      .mockResolvedValueOnce(
        json({ current: { apparent_temperature: 25, weather_code: 2, is_day: 1 } }),
      );

    await expect(fetchOpenMeteo(NAMED, signal())).resolves.toEqual({
      feelslike: 25,
      condition: 'partlyCloudyDay',
    });

    const geocoding = urlOf(0);

    expect(geocoding.origin + geocoding.pathname).toBe(
      'https://geocoding-api.open-meteo.com/v1/search',
    );
    expect(Object.fromEntries(geocoding.searchParams)).toEqual({
      name: 'São Paulo',
      count: '1',
      language: 'en',
      format: 'json',
    });
    expect(urlOf(1).searchParams.get('latitude')).toBe('-23.55');
    expect(urlOf(1).searchParams.get('longitude')).toBe('-46.63');
  });

  it('fails geocoding on no result, an HTTP error or a malformed answer', async () => {
    for (const response of [
      json({ results: [] }),
      json({}),
      json({ results: [{ latitude: 'x', longitude: 1 }] }),
      json({}, 500),
      new Response('not json', { status: 200 }),
    ]) {
      fetchMock.mockResolvedValueOnce(response);
      await expectFixedError(geocode('São Paulo', signal()), 'Failed to geocode location');
    }
  });

  it('rejects a wrong shape', async () => {
    for (const body of [{}, { current: {} }, { current: { weather_code: 'x' } }]) {
      fetchMock.mockResolvedValueOnce(json(body));
      await expectFixedError(fetchOpenMeteo(BERLIN, signal()), 'Weather response is invalid');
    }
  });
});

describe('request failures', () => {
  it('turns a non-OK status into a fixed message', async () => {
    fetchMock.mockResolvedValue(json({ message: KEY }, 401));
    await expectFixedError(fetchVisualCrossing(BERLIN, KEY, signal()), 'Weather API error: 401');
  });

  it('turns malformed JSON into a fixed message', async () => {
    fetchMock.mockResolvedValue(new Response('<html>', { status: 200 }));
    await expectFixedError(fetchOpenMeteo(BERLIN, signal()), 'Weather response is invalid');
  });

  it('turns a network failure into a fixed message', async () => {
    fetchMock.mockRejectedValue(new TypeError(`Load failed for ${KEY}`));
    await expectFixedError(fetchVisualCrossing(BERLIN, KEY, signal()), 'Weather request failed');
  });

  /** A fetch that never answers until its signal aborts. */
  const hangingFetch = (_url: string, init: RequestInit) =>
    new Promise<Response>((_resolve, reject) => {
      init.signal!.addEventListener('abort', () =>
        reject(new DOMException('aborted', 'AbortError')),
      );
    });

  it('times out after 15 seconds', async () => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(hangingFetch);

    const pending = fetchOpenMeteo(BERLIN, signal());
    const settled = expectFixedError(pending, 'Weather request timed out');

    await vi.advanceTimersByTimeAsync(WEATHER_REQUEST_TIMEOUT_MS - 1);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect((fetchMock.mock.calls[0]![1] as RequestInit).signal!.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await settled;
    expect(vi.getTimerCount()).toBe(0);
  });

  it('aborts with the caller signal', async () => {
    fetchMock.mockImplementation(hangingFetch);

    const controller = new AbortController();
    const pending = fetchVisualCrossing(BERLIN, KEY, controller.signal);

    controller.abort();
    await expectFixedError(pending, 'Weather request cancelled');
    expect((fetchMock.mock.calls[0]![1] as RequestInit).signal!.aborted).toBe(true);
  });

  it('aborts at once with an already aborted signal', async () => {
    fetchMock.mockImplementation(hangingFetch);

    const controller = new AbortController();

    controller.abort();
    await expectFixedError(fetchOpenMeteo(NAMED, controller.signal), 'Weather request cancelled');
  });
});
