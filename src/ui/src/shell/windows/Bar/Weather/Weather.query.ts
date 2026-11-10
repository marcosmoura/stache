import { skipToken, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';

import { readBarWeather } from '../../../../sdk/client';

import {
  chooseProvider,
  fetchCurrentConditions,
  WeatherError,
  type CurrentConditions,
} from './Weather.api';
import type { WeatherReading } from './Weather.types';

/** Default cadence in minutes, used until the host reports the configured one. */
export const DEFAULT_WEATHER_REFRESH_MINUTES = 20;

const MS_PER_MINUTE = 60 * 1000;

/** The configured minutes, falling back to the default for an unusable value. */
const refreshIntervalMs = (minutes: number | undefined) =>
  (Number.isFinite(minutes) && minutes !== undefined && minutes > 0
    ? minutes
    : DEFAULT_WEATHER_REFRESH_MINUTES) * MS_PER_MINUTE;

const UNCONFIGURED: WeatherReading = { isConfigured: false, isLoading: false };

/**
 * Query keys carry only the non-secret accepted config revision and the
 * non-secret weather epoch. The key, the
 * coordinates and the host answer never reach a key, a cache entry, storage or
 * a log; they exist inside `fetchWeather` only.
 */
const weatherKey = (revision: string, epoch: string) =>
  ['bar', 'weather', revision, epoch] as const;
const weatherStateKey = (revision: string) => ['bar', 'weather', 'state', revision] as const;

type WeatherState = { configured: boolean; refreshIntervalMinutes?: number };
type WeatherResult =
  | { kind: 'unconfigured' }
  | { kind: 'ready'; conditions: CurrentConditions; refreshIntervalMinutes: number };

function parseRevision(value: string): bigint | null {
  return /^\d+$/.test(value) ? BigInt(value) : null;
}

/**
 * Reads the host inputs; `bar_weather` answers `stale` (or an older revision
 * than the feed's) when Bar restarted or was reconfigured mid-read, in which
 * case it is asked again once, promptly.
 */
async function readInputs(revision: string, signal: AbortSignal) {
  const wanted = parseRevision(revision);

  for (let attempt = 0; attempt < 2; attempt += 1) {
    const outcome = await readBarWeather();

    if (signal.aborted) {
      throw new WeatherError('Weather request cancelled');
    }

    if (outcome.kind === 'ready') {
      const got = parseRevision(outcome.weather.configRevision);

      if (wanted !== null && got !== null && got < wanted) {
        continue;
      }
    } else if (outcome.kind === 'stale') {
      continue;
    }

    return outcome;
  }

  throw new WeatherError('Weather inputs are not ready');
}

async function fetchWeather(
  client: QueryClient,
  revision: string,
  signal: AbortSignal,
): Promise<WeatherResult> {
  const publish = (configured: boolean, refreshIntervalMinutes?: number) => {
    if (!signal.aborted) {
      client.setQueryData<WeatherState>(weatherStateKey(revision), {
        configured,
        refreshIntervalMinutes,
      });
    }
  };
  const outcome = await readInputs(revision, signal);

  if (outcome.kind !== 'ready') {
    publish(false);

    return { kind: 'unconfigured' };
  }

  const { provider, location } = outcome.weather;
  const apiKey = outcome.weather.apiKey?.trim() ?? '';
  const choice = chooseProvider(provider, apiKey);

  if (!choice) {
    publish(false);

    return { kind: 'unconfigured' };
  }

  // Published before the provider fetch so a failed fetch keeps the configured cadence.
  publish(true, outcome.weather.refreshIntervalMinutes);

  const conditions = await fetchCurrentConditions(choice, { location, apiKey }, signal);

  if (signal.aborted) {
    throw new WeatherError('Weather request cancelled');
  }

  return {
    kind: 'ready',
    conditions,
    refreshIntervalMinutes: outcome.weather.refreshIntervalMinutes,
  };
}

/**
 * The Bar weather reading (`BarData.weather`). Runs only for a served Bar
 * (`revision` is its accepted config revision, `null` otherwise), refetches
 * every configured `refreshIntervalMinutes` (20 until the first answer) and on reconnect, and a new revision starts a new query
 * whose answer can never be replaced by an older revision's. A new `epoch` (a
 * location consent decided after the read timed out) is a new query key, so it
 * refetches exactly once at once; the previous reading of the same revision is
 * kept until the new answer arrives and the older in-flight request is cancelled.
 */
export function useBarWeather(revision: string | null, epoch = '0'): WeatherReading {
  const enabled = revision !== null;
  const key = weatherKey(revision ?? '', epoch);
  const queryClient = useQueryClient();
  const result = useQuery<WeatherResult>({
    queryKey: key,
    queryFn: enabled ? ({ signal, client }) => fetchWeather(client, revision, signal) : skipToken,
    staleTime: Infinity,
    retry: false,
    placeholderData: (previous, previousQuery) =>
      previousQuery?.queryKey[2] === revision ? previous : undefined,
    refetchInterval: (query) => {
      const data = query.state.data;

      if (data?.kind === 'unconfigured') {
        return false;
      }

      return refreshIntervalMs(
        data?.kind === 'ready'
          ? data.refreshIntervalMinutes
          : queryClient.getQueryData<WeatherState>(weatherStateKey(revision ?? ''))
              ?.refreshIntervalMinutes,
      );
    },
    refetchIntervalInBackground: true,
    refetchOnReconnect: 'always',
    refetchOnWindowFocus: false,
    refetchOnMount: false,
    gcTime: 0,
  });
  const state = useQuery<WeatherState>({
    queryKey: weatherStateKey(revision ?? ''),
    queryFn: skipToken,
    staleTime: Infinity,
    gcTime: 0,
  });

  if (!enabled || !state.data?.configured) {
    return UNCONFIGURED;
  }

  if (result.data?.kind === 'ready') {
    return { isConfigured: true, isLoading: false, currentConditions: result.data.conditions };
  }

  return { isConfigured: true, isLoading: result.isLoading };
}
