import { StrictMode, type ReactNode } from 'react';

import { QueryClient, QueryClientProvider, onlineManager } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

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
import type { BarWeather, BarWeatherOutcome } from '../../../../sdk/bindings.gen';

import { useBarWeather } from './Weather.query';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const MINUTE = 60 * 1000;
const WEATHER_REFETCH_INTERVAL_MS = 20 * MINUTE;
const KEY = 'SENTINEL-KEY-4f9d2a';
const LAT = 51.123456;
const LON = -0.654321;

type Inputs = Partial<Omit<BarWeather, 'location'>> & { location?: BarWeather['location'] };

const ready = (inputs: Inputs = {}): BarWeatherOutcome => ({
  kind: 'ready',
  weather: {
    configRevision: '1',
    provider: 'auto',
    apiKey: KEY,
    refreshIntervalMinutes: 20,
    location: { source: 'coreLocation', latitude: LAT, longitude: LON, reducedAccuracy: false },
    ...inputs,
  } as BarWeather,
});

let harness: FetchHarness;
let clients: QueryClient[] = [];

function hostAnswers(...answers: BarWeatherOutcome[]) {
  const queue = [...answers];

  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command !== 'bar_weather') {
      throw new Error(`Unexpected command: ${command}`);
    }

    return queue.length > 1 ? queue.shift() : queue[0];
  }) as never);
}

const vcOk = (icon = 'clear-day', feelslike: number | null = 20.2) => ({
  match: onHost(VC_HOST),
  respond: () => jsonResponse(vcBody(icon, feelslike)),
});
const omOk = { match: onHost(OM_HOST), respond: () => jsonResponse(omBody()) };

async function mount(initial: string | null, { strict = false } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });

  clients.push(client);

  const wrapper = ({ children }: { children: ReactNode }) => {
    const tree = <QueryClientProvider client={client}>{children}</QueryClientProvider>;

    return strict ? <StrictMode>{tree}</StrictMode> : tree;
  };

  const hook = await renderHook(
    (props?: { revision: string | null; epoch?: string }) =>
      useBarWeather(props?.revision ?? null, props?.epoch ?? '0'),
    { initialProps: { revision: initial, epoch: '0' }, wrapper },
  );

  return { client, ...hook };
}

async function settle() {
  await vi.advanceTimersByTimeAsync(0);
  await vi.advanceTimersByTimeAsync(0);
}

const bridgeCalls = () =>
  vi.mocked(invoke).mock.calls.filter(([command]) => command === 'bar_weather').length;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
  onlineManager.setOnline(true);
});

afterEach(() => {
  harness?.restore();
  vi.useRealTimers();
  vi.restoreAllMocks();
  clients = [];
});

describe('useBarWeather outcomes', () => {
  test('is hidden and calls nothing while Bar serves nothing', async () => {
    harness = stubFetch([]);
    hostAnswers(ready());

    const { result } = await mount(null);

    await settle();

    expect(result.current).toEqual({ isConfigured: false, isLoading: false });
    expect(bridgeCalls()).toBe(0);
    expect(harness.urls()).toEqual([]);
  });

  test('ready: loads, then renders conditions from Visual Crossing', async () => {
    harness = stubFetch([vcOk('rain', 3.3)]);
    hostAnswers(ready());

    const { result } = await mount('1');

    await vi.waitFor(() => expect(result.current.isConfigured).toBe(true));
    await settle();

    expect(result.current).toEqual({
      isConfigured: true,
      isLoading: false,
      currentConditions: { feelslike: 3.3, icon: 'rain' },
    });
    expect(harness.unexpected()).toEqual([]);
  });

  test('shows the loading branch while the provider request is in flight', async () => {
    harness = stubFetch([{ match: () => true, respond: (_url, signal) => hang(signal) }]);
    hostAnswers(ready({ provider: 'openMeteo', apiKey: null }));

    const { result } = await mount('1');

    await settle();

    expect(result.current).toEqual({ isConfigured: true, isLoading: true });
  });

  test.each([
    ['unavailable', { kind: 'unavailable', reason: 'disabled', detail: null } as const],
    ['unauthorized', { kind: 'unauthorized' } as const],
  ])('%s is the unconfigured branch and makes no request', async (_name, outcome) => {
    harness = stubFetch([]);
    hostAnswers(outcome);

    const { result } = await mount('1');

    await settle();

    expect(result.current).toEqual({ isConfigured: false, isLoading: false });
    expect(harness.urls()).toEqual([]);

    await vi.advanceTimersByTimeAsync(WEATHER_REFETCH_INTERVAL_MS * 2);

    expect(bridgeCalls()).toBe(1);
  });

  test('stale is asked again once, promptly', async () => {
    harness = stubFetch([vcOk()]);
    hostAnswers({ kind: 'stale' }, ready());

    const { result } = await mount('1');

    await settle();

    expect(bridgeCalls()).toBe(2);
    expect(result.current.currentConditions?.icon).toBe('clearDay');
  });

  test('stale twice ends in the loading branch without a third call', async () => {
    harness = stubFetch([]);
    hostAnswers({ kind: 'stale' });

    const { result } = await mount('1');

    await settle();

    expect(bridgeCalls()).toBe(2);
    expect(result.current.currentConditions).toBeUndefined();
    expect(result.current.isLoading).toBe(false);
  });

  test('an answer older than the feed revision is read again, never applied', async () => {
    harness = stubFetch([vcOk('snow')]);
    hostAnswers(
      ready({ configRevision: '1', provider: 'openMeteo' }),
      ready({ configRevision: '2' }),
    );

    const { result } = await mount('2');

    await settle();

    expect(bridgeCalls()).toBe(2);
    expect(harness.urls().map((url) => url.host)).toEqual([VC_HOST]);
    expect(result.current.currentConditions?.icon).toBe('snow');
  });

  test('a fetch failure shows the legacy loading label state and keeps no data', async () => {
    harness = stubFetch([{ match: () => true, respond: () => jsonResponse({}, 500) }]);
    hostAnswers(ready());

    const { result } = await mount('1');

    await settle();

    expect(result.current).toEqual({ isConfigured: true, isLoading: false });
  });
});

describe('provider modes', () => {
  test.each([
    ['auto', 'KEY', VC_HOST],
    ['auto', '', OM_HOST],
    ['auto', null, OM_HOST],
    ['auto', '   ', OM_HOST],
    ['openMeteo', 'KEY', OM_HOST],
    ['openMeteo', null, OM_HOST],
    ['visualCrossing', 'KEY', VC_HOST],
  ] as const)('%s with key %j uses %s', async (provider, apiKey, host) => {
    harness = stubFetch([vcOk(), omOk]);
    hostAnswers(ready({ provider, apiKey }));

    const { result } = await mount('1');

    await settle();

    expect(result.current.isConfigured).toBe(true);
    expect(harness.urls().map((url) => url.host)).toEqual([host]);
  });

  test.each([null, '', '  '])('visualCrossing with key %j is unconfigured', async (apiKey) => {
    harness = stubFetch([]);
    hostAnswers(ready({ provider: 'visualCrossing', apiKey }));

    const { result } = await mount('1');

    await settle();

    expect(result.current).toEqual({ isConfigured: false, isLoading: false });
    expect(harness.urls()).toEqual([]);
  });
});

describe('location chain', () => {
  test('core location is used directly with no geocoding', async () => {
    harness = stubFetch([omOk]);
    hostAnswers(ready({ provider: 'openMeteo' }));
    await mount('1');
    await settle();

    expect(harness.urls()).toHaveLength(1);
    expect(harness.urls()[0].searchParams.get('latitude')).toBe(String(LAT));
  });

  test('a default city is geocoded; an empty geocode result is the loading branch', async () => {
    harness = stubFetch([
      { match: onHost(GEO_HOST), respond: () => jsonResponse({ results: [] }) },
      omOk,
    ]);
    hostAnswers(
      ready({ provider: 'openMeteo', location: { source: 'defaultLocation', query: 'Atlantis' } }),
    );

    const { result } = await mount('1');

    await settle();

    expect(harness.urls().map((url) => url.host)).toEqual([GEO_HOST]);
    expect(result.current).toEqual({ isConfigured: true, isLoading: false });
  });

  test('location none makes no provider request', async () => {
    harness = stubFetch([]);
    hostAnswers(ready({ location: { source: 'none' } }));

    const { result } = await mount('1');

    await settle();

    expect(harness.urls()).toEqual([]);
    expect(result.current).toEqual({ isConfigured: true, isLoading: false });
  });
});

describe('cadence', () => {
  test('refreshes every 20 minutes, on reconnect and at no other time', async () => {
    harness = stubFetch([vcOk()]);
    hostAnswers(ready());
    await mount('1');
    await settle();

    expect(harness.urls()).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(WEATHER_REFETCH_INTERVAL_MS - 1000);
    expect(harness.urls()).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(1000);
    expect(harness.urls()).toHaveLength(2);
    expect(bridgeCalls()).toBe(2);

    onlineManager.setOnline(false);
    onlineManager.setOnline(true);
    await settle();
    expect(harness.urls()).toHaveLength(3);

    window.dispatchEvent(new Event('focus'));
    await settle();
    expect(harness.urls()).toHaveLength(3);

    await vi.advanceTimersByTimeAsync(WEATHER_REFETCH_INTERVAL_MS);
    expect(harness.urls()).toHaveLength(4);
  });

  test('no later call after a success when nothing happens', async () => {
    harness = stubFetch([vcOk()]);
    hostAnswers(ready());
    await mount('1');
    await settle();
    await vi.advanceTimersByTimeAsync(WEATHER_REFETCH_INTERVAL_MS - 1);

    expect(harness.urls()).toHaveLength(1);
    expect(bridgeCalls()).toBe(1);
  });
});

describe('configured refresh interval', () => {
  test('polls at the configured minutes: nothing at N-1, one call at N', async () => {
    harness = stubFetch([vcOk()]);
    hostAnswers(ready({ refreshIntervalMinutes: 45 }));
    await mount('1');
    await settle();
    expect(bridgeCalls()).toBe(1);

    await vi.advanceTimersByTimeAsync(44 * MINUTE);
    expect(bridgeCalls()).toBe(1);

    await vi.advanceTimersByTimeAsync(MINUTE);
    expect(bridgeCalls()).toBe(2);
    expect(harness.urls()).toHaveLength(2);
  });

  test('keeps the configured interval when the provider request fails', async () => {
    harness = stubFetch([{ match: () => true, respond: () => jsonResponse({}, 500) }]);
    hostAnswers(ready({ refreshIntervalMinutes: 5 }));
    await mount('1');
    await settle();
    expect(bridgeCalls()).toBe(1);

    // The host answered with 5 minutes before the provider failed.
    await vi.advanceTimersByTimeAsync(4 * MINUTE);
    expect(bridgeCalls()).toBe(1);

    await vi.advanceTimersByTimeAsync(MINUTE);
    expect(bridgeCalls()).toBe(2);
  });

  test('uses 20 minutes while the host has not answered', async () => {
    harness = stubFetch([]);
    hostAnswers({ kind: 'stale' }, { kind: 'stale' });
    await mount('1');
    await settle();
    const first = bridgeCalls();

    await vi.advanceTimersByTimeAsync(19 * MINUTE);
    expect(bridgeCalls()).toBe(first);

    await vi.advanceTimersByTimeAsync(MINUTE);
    expect(bridgeCalls()).toBeGreaterThan(first);
  });

  test('a new config revision with a new interval switches the cadence', async () => {
    harness = stubFetch([vcOk()]);
    hostAnswers(
      ready({ configRevision: '1', refreshIntervalMinutes: 30 }),
      ready({ configRevision: '2', refreshIntervalMinutes: 5 }),
    );

    const { rerender } = await mount('1');

    await settle();
    await rerender({ revision: '2', epoch: '0' });
    await settle();
    expect(bridgeCalls()).toBe(2);

    await vi.advanceTimersByTimeAsync(4 * MINUTE);
    expect(bridgeCalls()).toBe(2);

    await vi.advanceTimersByTimeAsync(MINUTE);
    expect(bridgeCalls()).toBe(3);

    await vi.advanceTimersByTimeAsync(5 * MINUTE);
    expect(bridgeCalls()).toBe(4);
  });

  test('unconfigured disables polling whatever the interval', async () => {
    harness = stubFetch([]);
    hostAnswers({ kind: 'unauthorized' });
    await mount('1');
    await settle();
    await vi.advanceTimersByTimeAsync(60 * MINUTE);
    expect(bridgeCalls()).toBe(1);
  });
});

describe('revisions, cancellation and disposal', () => {
  test('a new revision refetches and the old response can never replace the new one', async () => {
    let releaseOld: (response: Response) => void = () => {};
    let call = 0;

    harness = stubFetch([
      {
        match: onHost(VC_HOST),
        respond: () => {
          call += 1;

          return call === 1
            ? new Promise<Response>((resolve) => {
                releaseOld = resolve;
              })
            : jsonResponse(vcBody('snow'));
        },
      },
    ]);
    hostAnswers(ready({ configRevision: '1' }), ready({ configRevision: '2' }));

    const { result, rerender } = await mount('1');

    await settle();
    await rerender({ revision: '2', epoch: '0' });
    await settle();

    expect(result.current.currentConditions?.icon).toBe('snow');

    releaseOld(jsonResponse(vcBody('rain')));
    await settle();

    expect(result.current.currentConditions?.icon).toBe('snow');
    expect(harness.urls()).toHaveLength(2);
  });

  test('a changed weather epoch refetches exactly once and an unchanged one never does', async () => {
    let call = 0;

    harness = stubFetch([
      {
        match: onHost(VC_HOST),
        respond: () => {
          call += 1;

          return jsonResponse(vcBody(call === 1 ? 'rain' : 'snow'));
        },
      },
    ]);
    hostAnswers(ready());

    const { result, rerender } = await mount('1');

    await settle();
    expect(result.current.currentConditions?.icon).toBe('rain');
    expect(bridgeCalls()).toBe(1);

    await rerender({ revision: '1', epoch: '0' });
    await rerender({ revision: '1', epoch: '0' });
    await settle();
    expect(bridgeCalls()).toBe(1);

    await rerender({ revision: '1', epoch: '1' });
    await settle();
    expect(bridgeCalls()).toBe(2);
    expect(result.current.currentConditions?.icon).toBe('snow');

    await rerender({ revision: '1', epoch: '1' });
    await settle();
    expect(bridgeCalls()).toBe(2);
    expect(harness.urls()).toHaveLength(2);
  });

  test('the old in-flight answer cannot replace the newer epoch answer', async () => {
    let releaseOld: (response: Response) => void = () => {};
    let call = 0;

    harness = stubFetch([
      {
        match: onHost(VC_HOST),
        respond: () => {
          call += 1;

          return call === 1
            ? new Promise<Response>((resolve) => {
                releaseOld = resolve;
              })
            : jsonResponse(vcBody('snow'));
        },
      },
    ]);
    hostAnswers(ready());

    const { result, rerender } = await mount('1');

    await settle();
    await rerender({ revision: '1', epoch: '1' });
    await settle();
    expect(result.current.currentConditions?.icon).toBe('snow');

    releaseOld(jsonResponse(vcBody('rain')));
    await settle();

    expect(result.current.currentConditions?.icon).toBe('snow');
  });

  test('the previous reading stays on screen while a new epoch loads', async () => {
    let call = 0;
    let releaseNew: (response: Response) => void = () => {};

    harness = stubFetch([
      {
        match: onHost(VC_HOST),
        respond: () => {
          call += 1;

          return call === 1
            ? jsonResponse(vcBody('rain'))
            : new Promise<Response>((resolve) => {
                releaseNew = resolve;
              });
        },
      },
    ]);
    hostAnswers(ready());

    const { result, rerender } = await mount('1');

    await settle();
    await rerender({ revision: '1', epoch: '1' });
    await settle();
    expect(result.current.currentConditions?.icon).toBe('rain');

    releaseNew(jsonResponse(vcBody('snow')));
    await settle();
    expect(result.current.currentConditions?.icon).toBe('snow');
  });

  test('unmounting aborts the in-flight request', async () => {
    let seen: AbortSignal | undefined;

    harness = stubFetch([
      {
        match: () => true,
        respond: (_url, signal) => {
          seen = signal;

          return hang(signal);
        },
      },
    ]);
    hostAnswers(ready());

    const { unmount } = await mount('1');

    await settle();
    expect(seen?.aborted).toBe(false);

    await unmount();
    await settle();

    expect(seen?.aborted).toBe(true);
  });

  test('StrictMode disposal leaves one live query and the right answer', async () => {
    harness = stubFetch([vcOk('fog')]);
    hostAnswers(ready());

    const { result, client } = await mount('1', { strict: true });

    await settle();

    expect(result.current.currentConditions?.icon).toBe('fog');
    expect(harness.unexpected()).toEqual([]);

    const calls = harness.urls().length;

    await vi.advanceTimersByTimeAsync(WEATHER_REFETCH_INTERVAL_MS);

    expect(harness.urls().length).toBe(calls + 1);
    expect(
      client
        .getQueryCache()
        .getAll()
        .filter((query) => query.getObserversCount() > 0),
    ).toHaveLength(2);
  });
});

describe('secret and coordinate boundary', () => {
  test('the key and the coordinates are in no query key, cache entry, storage, log or error text', async () => {
    const logs: string[] = [];

    for (const method of ['log', 'debug', 'info', 'warn', 'error'] as const) {
      vi.spyOn(console, method).mockImplementation((...args: unknown[]) => {
        logs.push(JSON.stringify(args));
      });
    }

    harness = stubFetch([{ match: onHost(VC_HOST), respond: () => jsonResponse({}, 500) }, omOk]);
    hostAnswers(ready());

    const first = await mount('1');

    await settle();

    harness.restore();
    harness = stubFetch([vcOk('fog')]);
    await vi.advanceTimersByTimeAsync(WEATHER_REFETCH_INTERVAL_MS);

    const second = await mount('2');

    await settle();

    for (const { client } of [first, second]) {
      const dump = JSON.stringify(
        client
          .getQueryCache()
          .getAll()
          .map((query) => ({
            key: query.queryKey,
            data: query.state.data,
            error: (query.state.error as Error | null)?.message,
          })),
      );

      expect(dump).not.toContain(KEY);
      expect(dump).not.toContain(String(LAT));
      expect(dump).not.toContain(String(LON));
    }

    const everything = [
      JSON.stringify({ ...localStorage }),
      JSON.stringify({ ...sessionStorage }),
      JSON.stringify(first.result.current),
      JSON.stringify(second.result.current),
      ...logs,
    ].join('\n');

    expect(everything).not.toContain(KEY);
    expect(everything).not.toContain(String(LAT));
    expect(everything).not.toContain(String(LON));
    expect(localStorage.length).toBe(0);
    expect(sessionStorage.length).toBe(0);
  });
});
