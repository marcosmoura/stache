import { queryOptions, skipToken, type QueryClient } from '@tanstack/react-query';

import { commands, type BarSnapshotOutcome, type BarWeatherOutcome_Serialize } from '@/ipc';

import { resolveLocation } from './Weather.location';
import {
  chooseProvider,
  fetchOpenMeteo,
  fetchVisualCrossing,
  WeatherError,
  type CurrentConditions,
} from './Weather.providers';

/** Minutes between refreshes until a `ready` answer reports the configured value. */
export const DEFAULT_WEATHER_REFRESH_MINUTES = 20;

const MS_PER_MINUTE = 60_000;

/** The non-secret inputs of the weather query, read from an `available` snapshot. */
export type WeatherKeyInputs = { configRevision: string; weatherEpoch: string };

/** The revision and epoch while the Bar is available, otherwise `null`. */
export function selectWeatherKey(outcome: BarSnapshotOutcome): WeatherKeyInputs | null {
  if (outcome.kind !== 'available') {
    return null;
  }

  const { configRevision, weatherEpoch } = outcome.view;

  return { configRevision, weatherEpoch };
}

/** What the weather query caches: never the key, the location or the host answer. */
export type WeatherResult =
  | { kind: 'unconfigured' }
  | { kind: 'ready'; conditions: CurrentConditions; refreshIntervalMinutes: number };

/**
 * Whether a revision's provider is configured, published by the `queryFn`
 * before the provider fetch so a failed fetch still renders `Loading...` and
 * keeps the configured cadence.
 */
export type WeatherState = { configured: boolean; refreshIntervalMinutes: number | null };

export function weatherStateOptions(configRevision: string | null) {
  return queryOptions<WeatherState>({
    queryKey: ['bar', 'weather', 'state', configRevision ?? ''],
    queryFn: skipToken,
  });
}

function revisionNumber(value: string): bigint | null {
  return /^\d+$/.test(value) ? BigInt(value) : null;
}

/**
 * Reads `bar_weather`. A `stale` answer, or one older than the snapshot's
 * revision, is asked again once.
 */
async function readInputs(
  configRevision: string,
  signal: AbortSignal,
): Promise<BarWeatherOutcome_Serialize> {
  const wanted = revisionNumber(configRevision);

  for (let attempt = 0; attempt < 2; attempt += 1) {
    const outcome = await commands.barWeather();

    if (signal.aborted) {
      throw new WeatherError('Weather request cancelled');
    }

    if (outcome.kind === 'stale') {
      continue;
    }

    if (outcome.kind === 'ready') {
      const got = revisionNumber(outcome.weather.configRevision);

      if (wanted !== null && got !== null && got < wanted) {
        continue;
      }
    }

    return outcome;
  }

  throw new WeatherError('Weather inputs are not ready');
}

async function fetchWeather(
  client: QueryClient,
  configRevision: string,
  signal: AbortSignal,
): Promise<WeatherResult> {
  const publish = (state: WeatherState) => {
    if (!signal.aborted) {
      client.setQueryData(weatherStateOptions(configRevision).queryKey, state);
    }
  };
  const outcome = await readInputs(configRevision, signal);

  if (outcome.kind !== 'ready') {
    // `unavailable` / `unauthorized` are host failures, not configuration: keep
    // the last known state (or assume configured, so `Loading...` shows) and
    // throw, so the interval refetch asks again. The text is fixed.
    const previous = client.getQueryData(weatherStateOptions(configRevision).queryKey);
    const state = previous ?? { configured: true, refreshIntervalMinutes: null };
    const error = new WeatherError('Weather inputs are unavailable');

    publish(state);
    error.refreshIntervalMinutes = state.refreshIntervalMinutes;

    throw error;
  }

  const { provider, location, refreshIntervalMinutes } = outcome.weather;
  const apiKey = outcome.weather.apiKey?.trim() ?? '';
  const choice = chooseProvider(provider, apiKey);

  if (!choice) {
    publish({ configured: false, refreshIntervalMinutes: null });

    return { kind: 'unconfigured' };
  }

  publish({ configured: true, refreshIntervalMinutes });

  try {
    const resolved = resolveLocation(location);

    if (!resolved) {
      throw new WeatherError('Weather location is not configured');
    }

    const conditions =
      choice === 'visualCrossing'
        ? await fetchVisualCrossing(resolved, apiKey, signal)
        : await fetchOpenMeteo(resolved, signal);

    if (signal.aborted) {
      throw new WeatherError('Weather request cancelled');
    }

    return { kind: 'ready', conditions, refreshIntervalMinutes };
  } catch (error) {
    // A failed fetch keeps the configured cadence (the error carries no secret).
    if (error instanceof WeatherError) {
      error.refreshIntervalMinutes = refreshIntervalMinutes;
    }

    throw error;
  }
}

function intervalMs(minutes: number | null | undefined) {
  return (
    (typeof minutes === 'number' && Number.isFinite(minutes) && minutes > 0
      ? minutes
      : DEFAULT_WEATHER_REFRESH_MINUTES) * MS_PER_MINUTE
  );
}

/**
 * The weather query: keyed only by the accepted `configRevision` and the
 * snapshot's `weatherEpoch` (`inputs` is `null` while the Bar is not
 * available, which disables it). The `queryFn` calls `bar_weather` and the
 * chosen provider with the query `signal`, so a superseded or unmounted
 * request is aborted; the key and coordinates exist only inside it.
 * Overrides the client defaults as a remote query: refetch every configured
 * `refresh_interval` (20 minutes until known, never once unconfigured; an
 * `unavailable` or `unauthorized` answer is a failure that refetches), in
 * the background too and on reconnect; no retry.
 */
export function weatherOptions(inputs: WeatherKeyInputs | null) {
  const configRevision = inputs?.configRevision ?? null;

  return queryOptions<WeatherResult>({
    queryKey: ['bar', 'weather', configRevision ?? '', inputs?.weatherEpoch ?? ''],
    queryFn:
      configRevision === null
        ? skipToken
        : ({ client, signal }) => fetchWeather(client, configRevision, signal),
    staleTime: Infinity,
    retry: false,
    // A new epoch keeps showing the same revision's reading until the new answer.
    placeholderData: (previous, previousQuery) =>
      previousQuery?.queryKey[2] === configRevision ? previous : undefined,
    refetchInterval: (query) => {
      const data = query.state.data;

      if (data?.kind === 'unconfigured') {
        return false;
      }

      if (data?.kind === 'ready') {
        return intervalMs(data.refreshIntervalMinutes);
      }

      const { error } = query.state;

      return intervalMs(error instanceof WeatherError ? error.refreshIntervalMinutes : null);
    },
    refetchIntervalInBackground: true,
    refetchOnReconnect: 'always',
  });
}
