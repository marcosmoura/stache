import { onlineManager, QueryObserver, type QueryClient } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { BarWeatherOutcome_Serialize, BarWeather_Serialize } from '@/ipc';

import { stubRemoteFetch } from '../../../../../tests/fetch';
import { barAvailable, barUnauthorized, barUnavailable } from '../../../../../tests/fixtures';
import { flush, mockHost, type MockHost } from '../../../../../tests/ipc';
import { createQueryClient } from '../../../../../tests/queryClient';

import {
  DEFAULT_WEATHER_REFRESH_MINUTES,
  selectWeatherKey,
  weatherOptions,
  weatherStateOptions,
  type WeatherKeyInputs,
} from './Weather.query';

const KEY = 'SENTINEL-KEY-0123';
const LAT = 41.123456;
const LON = -8.654321;
const MINUTE = 60_000;

const ready = (overrides: Partial<BarWeather_Serialize> = {}): BarWeatherOutcome_Serialize => ({
  kind: 'ready',
  weather: {
    configRevision: '1',
    provider: 'auto',
    apiKey: KEY,
    location: { source: 'coreLocation', latitude: LAT, longitude: LON, reducedAccuracy: true },
    refreshIntervalMinutes: 7,
    ...overrides,
  },
});

const vcAnswer = (feelslike: number) =>
  new Response(JSON.stringify({ currentConditions: { feelslike, icon: 'rain' } }));

const inputs = (configRevision = '1', weatherEpoch = '0'): WeatherKeyInputs => ({
  configRevision,
  weatherEpoch,
});

let client: QueryClient;
let host: MockHost;
let fetchMock: ReturnType<typeof vi.fn>;
let stops: Array<() => void>;

beforeEach(() => {
  client = createQueryClient();
  client.mount();
  host = mockHost();
  fetchMock = vi.fn(() => Promise.resolve(vcAnswer(10)));
  stubRemoteFetch(fetchMock);
  stops = [];
});

afterEach(() => {
  stops.forEach((stop) => stop());
  client.clear();
  client.unmount();
  onlineManager.setOnline(true);
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** Observes the weather query for `keyInputs`, as a mounted widget would. */
function observe(keyInputs: WeatherKeyInputs | null) {
  const observer = new QueryObserver(client, weatherOptions(keyInputs));

  stops.push(observer.subscribe(() => undefined));

  return observer;
}

const settle = async () => {
  await vi.waitFor(() => expect(client.isFetching()).toBe(0));
  await flush();
};

/** A fetch that never answers until its signal aborts. */
const hangingFetch = (_url: string, init: RequestInit) =>
  new Promise<Response>((_resolve, reject) => {
    init.signal!.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
  });

describe('weather query key', () => {
  it('holds only the revision and the epoch', () => {
    expect(weatherOptions(inputs('3', '2')).queryKey).toEqual(['bar', 'weather', '3', '2']);
    expect(selectWeatherKey(barAvailable('5', { configRevision: '4', weatherEpoch: '9' }))).toEqual(
      inputs('4', '9'),
    );
    expect(selectWeatherKey(barUnavailable('5'))).toBeNull();
    expect(selectWeatherKey(barUnauthorized)).toBeNull();
  });

  it('keeps the key and coordinates out of every key and cache entry', async () => {
    host.answer('bar_weather', ready());
    await client.fetchQuery(weatherOptions(inputs()));

    const cache = JSON.stringify(
      client
        .getQueryCache()
        .getAll()
        .map((query) => [query.queryKey, query.state.data, query.state.error]),
    );

    expect(cache).toContain('"feelslike":10');
    expect(cache).not.toContain(KEY);
    expect(cache).not.toContain(String(LAT));
    expect(cache).not.toContain('41.12');
    expect(cache).not.toContain(String(LON));
  });
});

describe('weather query', () => {
  it('does not run while the Bar is not available', async () => {
    host.answer('bar_weather', ready());
    observe(null);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(0);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('answers ready with the rendered subset and marks the revision configured', async () => {
    host.answer('bar_weather', ready());
    await expect(client.fetchQuery(weatherOptions(inputs()))).resolves.toEqual({
      kind: 'ready',
      conditions: { feelslike: 10, condition: 'rain' },
      refreshIntervalMinutes: 7,
    });
    expect(client.getQueryData(weatherStateOptions('1').queryKey)).toEqual({
      configured: true,
      refreshIntervalMinutes: 7,
    });
    expect(new URL(String(fetchMock.mock.calls[0]![0])).host).toBe('weather.visualcrossing.com');
  });

  it('uses Open Meteo with auto and no key, and never sends a key', async () => {
    host.answer('bar_weather', ready({ apiKey: '   ' }));
    fetchMock.mockResolvedValue(
      new Response(JSON.stringify({ current: { apparent_temperature: 3, weather_code: 3 } })),
    );
    await client.fetchQuery(weatherOptions(inputs()));
    expect(new URL(String(fetchMock.mock.calls[0]![0])).host).toBe('api.open-meteo.com');
  });

  it('uses Open Meteo when configured even with a key', async () => {
    host.answer('bar_weather', ready({ provider: 'openMeteo' }));
    fetchMock.mockResolvedValue(
      new Response(JSON.stringify({ current: { apparent_temperature: 3, weather_code: 3 } })),
    );
    await client.fetchQuery(weatherOptions(inputs()));

    const url = String(fetchMock.mock.calls[0]![0]);

    expect(new URL(url).host).toBe('api.open-meteo.com');
    expect(url).not.toContain(KEY);
  });

  it.each([['visual_crossing without a key', ready({ provider: 'visualCrossing', apiKey: null })]])(
    'is unconfigured for %s and fetches nothing',
    async (_name, outcome) => {
      host.answer('bar_weather', outcome);
      await expect(client.fetchQuery(weatherOptions(inputs()))).resolves.toEqual({
        kind: 'unconfigured',
      });
      expect(client.getQueryData(weatherStateOptions('1').queryKey)).toEqual({
        configured: false,
        refreshIntervalMinutes: null,
      });
      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

  it.each([
    ['unavailable', { kind: 'unavailable', reason: 'stopped', detail: 'secret detail' } as const],
    ['unauthorized', { kind: 'unauthorized' } as const],
  ])('fails on %s, stays configured and fetches nothing', async (_name, outcome) => {
    host.answer('bar_weather', outcome);

    const error = await client.fetchQuery(weatherOptions(inputs())).catch((e: Error) => e);

    expect(String(error)).toBe('WeatherError: Weather inputs are unavailable');
    expect(client.getQueryData(weatherStateOptions('1').queryKey)).toEqual({
      configured: true,
      refreshIntervalMinutes: null,
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('makes no request without a location and stays configured', async () => {
    host.answer('bar_weather', ready({ location: { source: 'none' } }));
    await expect(client.fetchQuery(weatherOptions(inputs()))).rejects.toThrow(
      'Weather location is not configured',
    );
    expect(client.getQueryData(weatherStateOptions('1').queryKey)?.configured).toBe(true);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('asks again once after stale or an older revision', async () => {
    const answers = [
      { kind: 'stale' },
      ready({ configRevision: '1' }),
      ready({ configRevision: '2' }),
    ];

    host.answer('bar_weather', () => answers.shift());
    await expect(client.fetchQuery(weatherOptions(inputs('2')))).rejects.toThrow(
      'Weather inputs are not ready',
    );
    expect(host.callsOf('bar_weather')).toHaveLength(2);
    await expect(client.fetchQuery(weatherOptions(inputs('2', '1')))).resolves.toMatchObject({
      kind: 'ready',
    });
  });

  it('keeps every thrown message free of the key and coordinates', async () => {
    host.answer('bar_weather', ready());
    fetchMock.mockResolvedValue(new Response(`${KEY} ${LAT}`, { status: 403 }));

    const error = await client.fetchQuery(weatherOptions(inputs())).catch((e: Error) => e);

    expect(String(error)).toBe('WeatherError: Weather API error: 403');
  });
});

describe('weather refresh', () => {
  it('refetches every configured interval, in the background too', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    host.answer('bar_weather', ready({ refreshIntervalMinutes: 7 }));
    observe(inputs());
    await vi.advanceTimersByTimeAsync(0);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(7 * MINUTE - 10);
    expect(host.callsOf('bar_weather')).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(10);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(2);
  });

  it('keeps the configured interval after a provider failure', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    host.answer('bar_weather', ready({ refreshIntervalMinutes: 5 }));
    fetchMock.mockResolvedValue(new Response('', { status: 500 }));
    observe(inputs());
    await vi.advanceTimersByTimeAsync(0);
    await flush(20);
    await vi.advanceTimersByTimeAsync(5 * MINUTE);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(2);
  });

  it('uses 20 minutes until an answer is known', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    host.answer('bar_weather', () => Promise.reject(new Error('no host')));
    observe(inputs());
    await vi.advanceTimersByTimeAsync(0);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(DEFAULT_WEATHER_REFRESH_MINUTES * MINUTE - 10);
    expect(host.callsOf('bar_weather')).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(10);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(2);
  });

  it('recovers from an unavailable answer at the next interval', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    const answers: BarWeatherOutcome_Serialize[] = [
      { kind: 'unavailable', reason: 'stopped', detail: null },
      ready(),
    ];

    host.answer('bar_weather', () => answers.shift());
    const observer = observe(inputs());

    await vi.advanceTimersByTimeAsync(0);
    await flush(20);
    expect(observer.getCurrentResult().status).toBe('error');
    await vi.advanceTimersByTimeAsync(DEFAULT_WEATHER_REFRESH_MINUTES * MINUTE);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(2);
    expect(observer.getCurrentResult().data).toMatchObject({
      kind: 'ready',
      conditions: { feelslike: 10 },
    });
  });

  it('does not refresh while unconfigured', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    host.answer('bar_weather', ready({ provider: 'visualCrossing', apiKey: null }));
    observe(inputs());
    await vi.advanceTimersByTimeAsync(0);
    await flush(20);
    await vi.advanceTimersByTimeAsync(24 * 60 * MINUTE);
    await flush(20);
    expect(host.callsOf('bar_weather')).toHaveLength(1);
  });

  it('refetches on reconnect', async () => {
    host.answer('bar_weather', ready());
    observe(inputs());
    await settle();
    expect(host.callsOf('bar_weather')).toHaveLength(1);
    onlineManager.setOnline(false);
    onlineManager.setOnline(true);
    await vi.waitFor(() => expect(host.callsOf('bar_weather')).toHaveLength(2));
  });

  it('a new revision or epoch refetches and aborts the previous request', async () => {
    host.answer('bar_weather', ready());
    fetchMock.mockImplementation(hangingFetch);

    const observer = observe(inputs('1', '0'));

    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    observer.setOptions(weatherOptions(inputs('1', '1')));
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect((fetchMock.mock.calls[0]![1] as RequestInit).signal!.aborted).toBe(true);

    host.answer('bar_weather', ready({ configRevision: '2' }));
    observer.setOptions(weatherOptions(inputs('2', '1')));
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(3));
    expect((fetchMock.mock.calls[1]![1] as RequestInit).signal!.aborted).toBe(true);
    expect(host.callsOf('bar_weather')).toHaveLength(3);
  });

  it('never lets an older answer replace a newer one', async () => {
    const pending: Array<(response: Response) => void> = [];

    host.answer('bar_weather', ready());
    fetchMock.mockImplementation(
      (_url: string, init: RequestInit) =>
        new Promise<Response>((resolve, reject) => {
          pending.push(resolve);
          init.signal!.addEventListener('abort', () =>
            reject(new DOMException('aborted', 'AbortError')),
          );
        }),
    );

    const observer = observe(inputs('1', '0'));

    await vi.waitFor(() => expect(pending).toHaveLength(1));
    observer.setOptions(weatherOptions(inputs('1', '1')));
    await vi.waitFor(() => expect(pending).toHaveLength(2));
    pending[1]!(vcAnswer(22));
    await vi.waitFor(() =>
      expect(observer.getCurrentResult().data).toMatchObject({ conditions: { feelslike: 22 } }),
    );
    pending[0]!(vcAnswer(5));
    await flush(20);
    expect(observer.getCurrentResult().data).toMatchObject({ conditions: { feelslike: 22 } });
    expect(client.getQueryData(weatherOptions(inputs('1', '0')).queryKey)).toBeUndefined();
  });

  it('keeps the same revision reading while a new epoch loads', async () => {
    host.answer('bar_weather', ready());

    const observer = observe(inputs('1', '0'));

    await settle();
    fetchMock.mockImplementation(hangingFetch);
    observer.setOptions(weatherOptions(inputs('1', '1')));
    expect(observer.getCurrentResult().data).toMatchObject({ conditions: { feelslike: 10 } });
    observer.setOptions(weatherOptions(inputs('2', '1')));
    expect(observer.getCurrentResult().data).toBeUndefined();
  });
});
