import { afterEach, describe, expect, test, vi } from 'vitest';

import {
  GEO_HOST,
  hang,
  jsonResponse,
  OM_HOST,
  omBody,
  onHost,
  stubFetch,
  VC_HOST,
  vcBody,
  type FetchHarness,
} from '../../../../../tests/weatherFetch';

import {
  chooseProvider,
  fetchCurrentConditions,
  isProviderAvailable,
  parseCoordinates,
  WEATHER_REQUEST_TIMEOUT_MS,
} from './Weather.api';
import { translateOpenMeteoIcon, translateVisualCrossingIcon } from './Weather.codes';
import { weatherIconMap } from './Weather.icons';

let harness: FetchHarness;

afterEach(() => {
  harness?.restore();
  vi.useRealTimers();
});

const signal = () => new AbortController().signal;
const ok = (body: unknown) => () => jsonResponse(body);

describe('provider availability', () => {
  test.each([
    ['auto', '', true, 'openMeteo'],
    ['auto', 'k', true, 'visualCrossing'],
    ['visualCrossing', '', false, undefined],
    ['visualCrossing', 'k', true, 'visualCrossing'],
    ['openMeteo', '', true, 'openMeteo'],
    ['openMeteo', 'k', true, 'openMeteo'],
  ] as const)('%s with key %j', (provider, key, available, choice) => {
    expect(isProviderAvailable(provider, key)).toBe(available);
    expect(chooseProvider(provider, key)).toBe(choice);
  });
});

describe('coordinates', () => {
  test.each([
    ['51.5,-0.12', { lat: 51.5, lon: -0.12 }],
    [' 40 , -74 ', { lat: 40, lon: -74 }],
    ['91,0', undefined],
    ['0,181', undefined],
    ['London', undefined],
    ['London, UK', undefined],
    ['1,2,3', undefined],
  ])('%j', (text, expected) => {
    expect(parseCoordinates(text)).toEqual(expected);
  });
});

describe('icon maps', () => {
  test('Visual Crossing names map to icon keys and unknown names fall back', () => {
    const expected: Record<string, string> = {
      snow: 'snow',
      'snow-showers-day': 'snowShowersDay',
      'snow-showers-night': 'snowShowersNight',
      'thunder-rain': 'thunder',
      'thunder-showers-day': 'thunderShowersDay',
      'thunder-showers-night': 'thunderShowersNight',
      rain: 'rain',
      'showers-day': 'rainDay',
      'showers-night': 'rainNight',
      fog: 'fog',
      wind: 'windy',
      cloudy: 'cloudy',
      'partly-cloudy-day': 'partlyCloudyDay',
      'partly-cloudy-night': 'partlyCloudyNight',
      'clear-day': 'clearDay',
      'clear-night': 'clearNight',
    };

    for (const [name, key] of Object.entries(expected)) {
      expect(translateVisualCrossingIcon(name)).toBe(key);
      expect(weatherIconMap).toHaveProperty(key);
    }

    expect(translateVisualCrossingIcon('hail')).toBe('clearDay');
    expect(translateVisualCrossingIcon('constructor')).toBe('clearDay');
  });

  test.each([
    [0, 'clearDay', 'clearNight'],
    [1, 'clearDay', 'clearNight'],
    [2, 'partlyCloudyDay', 'partlyCloudyNight'],
    [3, 'cloudy', 'cloudy'],
    [45, 'fog', 'fog'],
    [48, 'fog', 'fog'],
    [53, 'rain', 'rain'],
    [67, 'rain', 'rain'],
    [75, 'snow', 'snow'],
    [77, 'snow', 'snow'],
    [81, 'rainDay', 'rainNight'],
    [86, 'snowShowersDay', 'snowShowersNight'],
    [95, 'thunder', 'thunderShowersNight'],
    [99, 'thunder', 'thunderShowersNight'],
    [1234, 'clearDay', 'clearNight'],
  ])('Open Meteo code %s', (code, day, night) => {
    expect(translateOpenMeteoIcon(code, true)).toBe(day);
    expect(translateOpenMeteoIcon(code, false)).toBe(night);
    expect(weatherIconMap).toHaveProperty(day);
    expect(weatherIconMap).toHaveProperty(night);
  });
});

describe('Visual Crossing', () => {
  test('requests the legacy URL with the key and returns feels-like and the mapped icon', async () => {
    harness = stubFetch([{ match: onHost(VC_HOST), respond: ok(vcBody('showers-night', 4.5)) }]);

    const conditions = await fetchCurrentConditions(
      'visualCrossing',
      { apiKey: 'KEY 1', location: { source: 'defaultLocation', query: 'São Paulo, BR' } },
      signal(),
    );

    expect(conditions).toEqual({ feelslike: 4.5, icon: 'rainNight' });
    expect(harness.urls()).toHaveLength(1);

    const url = harness.urls()[0];

    expect(url.origin + url.pathname).toBe(
      `https://${VC_HOST}/VisualCrossingWebServices/rest/services/timeline/S%C3%A3o%20Paulo%2C%20BR/next5days`,
    );
    expect(Object.fromEntries(url.searchParams)).toEqual({
      key: 'KEY 1',
      unitGroup: 'metric',
      elements:
        'name,address,resolvedAddress,temp,tempmax,tempmin,feelslike,humidity,windspeed,winddir,pressure,visibility,cloudcover,conditions,icon,moonphase,precip,precipprob,preciptype,snow,snowdepth,datetime',
      include: 'current,hours,days',
      iconSet: 'icons2',
      contentType: 'json',
    });
    expect(harness.unexpected()).toEqual([]);
  });

  test('core location is sent as lat,lon and a null feels-like stays null', async () => {
    harness = stubFetch([{ match: onHost(VC_HOST), respond: ok(vcBody('fog', null)) }]);

    const conditions = await fetchCurrentConditions(
      'visualCrossing',
      {
        apiKey: 'k',
        location: {
          source: 'coreLocation',
          latitude: 51.5,
          longitude: -0.12,
          reducedAccuracy: false,
        },
      },
      signal(),
    );

    expect(conditions).toEqual({ feelslike: null, icon: 'fog' });
    expect(harness.urls()[0].pathname).toContain('/timeline/51.5%2C-0.12/next5days');
  });

  test('default coordinates are passed through as typed', async () => {
    harness = stubFetch([{ match: onHost(VC_HOST), respond: ok(vcBody()) }]);

    await fetchCurrentConditions(
      'visualCrossing',
      { apiKey: 'k', location: { source: 'defaultLocation', query: '40.7,-74.0' } },
      signal(),
    );

    expect(harness.urls()[0].pathname).toContain('/timeline/40.7%2C-74.0/next5days');
  });
});

describe('Open Meteo', () => {
  const forecast = { match: onHost(OM_HOST, '/v1/forecast') };

  test('core location goes straight to the forecast with the legacy fields', async () => {
    harness = stubFetch([{ ...forecast, respond: ok(omBody(61, 0, 7.1)) }]);

    const conditions = await fetchCurrentConditions(
      'openMeteo',
      {
        apiKey: '',
        location: {
          source: 'coreLocation',
          latitude: 48.85,
          longitude: 2.35,
          reducedAccuracy: true,
        },
      },
      signal(),
    );

    expect(conditions).toEqual({ feelslike: 7.1, icon: 'rain' });
    expect(harness.urls()).toHaveLength(1);

    const url = harness.urls()[0];

    expect(url.origin + url.pathname).toBe(`https://${OM_HOST}/v1/forecast`);
    expect(Object.fromEntries(url.searchParams)).toEqual({
      latitude: '48.85',
      longitude: '2.35',
      current:
        'temperature_2m,relative_humidity_2m,apparent_temperature,is_day,precipitation,weather_code,wind_speed_10m,wind_direction_10m,wind_gusts_10m',
      hourly:
        'temperature_2m,relative_humidity_2m,precipitation_probability,precipitation,weather_code,cloud_cover,wind_speed_10m,wind_direction_10m,wind_gusts_10m,visibility,pressure_msl',
      daily:
        'weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max,sunrise,sunset',
      timezone: 'auto',
      forecast_days: '5',
    });
  });

  test('default coordinates are parsed, not geocoded', async () => {
    harness = stubFetch([{ ...forecast, respond: ok(omBody(0, 1, 10)) }]);

    await fetchCurrentConditions(
      'openMeteo',
      { apiKey: '', location: { source: 'defaultLocation', query: '40.7, -74' } },
      signal(),
    );

    expect(harness.urls()).toHaveLength(1);
    expect(harness.urls()[0].searchParams.get('latitude')).toBe('40.7');
    expect(harness.urls()[0].searchParams.get('longitude')).toBe('-74');
  });

  test('a default city is geocoded first (first result, count 1, language en)', async () => {
    harness = stubFetch([
      {
        match: onHost(GEO_HOST),
        respond: ok({
          results: [
            { latitude: 1.5, longitude: 2.5, name: 'Paris' },
            { latitude: 9, longitude: 9, name: 'Paris, TX' },
          ],
        }),
      },
      { ...forecast, respond: ok(omBody()) },
    ]);

    await fetchCurrentConditions(
      'openMeteo',
      { apiKey: '', location: { source: 'defaultLocation', query: 'Paris' } },
      signal(),
    );

    const [geo, weather] = harness.urls();

    expect(geo.origin + geo.pathname).toBe(`https://${GEO_HOST}/v1/search`);
    expect(Object.fromEntries(geo.searchParams)).toEqual({
      name: 'Paris',
      count: '1',
      language: 'en',
      format: 'json',
    });
    expect(weather.searchParams.get('latitude')).toBe('1.5');
    expect(weather.searchParams.get('longitude')).toBe('2.5');
  });

  test.each([
    ['empty results', () => jsonResponse({ results: [] })],
    ['no results field', () => jsonResponse({})],
    ['HTTP error', () => jsonResponse({}, 500)],
    ['malformed JSON', () => new Response('{nope')],
  ])('geocoding %s is an error and no forecast is requested', async (_name, respond) => {
    harness = stubFetch([
      { match: onHost(GEO_HOST), respond },
      { ...forecast, respond: ok(omBody()) },
    ]);

    await expect(
      fetchCurrentConditions(
        'openMeteo',
        { apiKey: '', location: { source: 'defaultLocation', query: 'Nowhere' } },
        signal(),
      ),
    ).rejects.toThrow('Failed to geocode location');
    expect(harness.urls()).toHaveLength(1);
  });

  test('the location "none" and an empty default make no request', async () => {
    harness = stubFetch([]);

    for (const location of [
      { source: 'none' } as const,
      { source: 'defaultLocation', query: '  ' } as const,
      { source: 'coreLocation', latitude: null, longitude: 1, reducedAccuracy: false } as const,
    ]) {
      await expect(
        fetchCurrentConditions('openMeteo', { apiKey: '', location }, signal()),
      ).rejects.toThrow();
    }

    expect(harness.urls()).toEqual([]);
  });
});

describe('failures', () => {
  const location = { source: 'defaultLocation', query: '1,2' } as const;
  const run = (choice: 'visualCrossing' | 'openMeteo') =>
    fetchCurrentConditions(choice, { apiKey: 'SECRET', location }, signal());

  test.each([
    ['HTTP error', () => jsonResponse({}, 429), 'Weather API error: 429'],
    ['malformed JSON', () => new Response('<html>'), 'Weather request failed'],
    ['wrong shape', () => jsonResponse({ current: 1, currentConditions: 1 }), 'invalid'],
    ['non-numeric feels-like', () => jsonResponse(vcBody('fog', '5' as never)), 'invalid'],
  ])('%s', async (_name, respond, message) => {
    harness = stubFetch([{ match: () => true, respond }]);

    for (const choice of ['visualCrossing', 'openMeteo'] as const) {
      await expect(run(choice)).rejects.toThrow(message);
    }
  });

  test('a network error carries no URL or key', async () => {
    harness = stubFetch([]);

    const error = await run('visualCrossing').then(
      () => new Error('resolved'),
      (caught: unknown) => caught as Error,
    );

    expect(error.message).toBe('Weather request failed');
    expect(error.message).not.toContain('SECRET');
  });

  test('a request that never answers times out', async () => {
    vi.useFakeTimers();
    harness = stubFetch([
      {
        match: () => true,
        respond: (_url, requestSignal) => hang(requestSignal),
      },
    ]);

    const result = run('openMeteo').then(
      () => new Error('resolved'),
      (caught: unknown) => caught as Error,
    );

    await vi.advanceTimersByTimeAsync(WEATHER_REQUEST_TIMEOUT_MS);

    expect((await result).message).toBe('Weather request timed out');
  });

  test('an aborted signal cancels the request', async () => {
    const controller = new AbortController();

    harness = stubFetch([
      {
        match: () => true,
        respond: (_url, requestSignal) => hang(requestSignal),
      },
    ]);

    const result = fetchCurrentConditions(
      'openMeteo',
      { apiKey: '', location },
      controller.signal,
    ).then(
      () => new Error('resolved'),
      (caught: unknown) => caught as Error,
    );

    await Promise.resolve();
    controller.abort();

    expect((await result).message).toBe('Weather request cancelled');
  });
});
