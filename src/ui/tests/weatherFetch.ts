import { vi, type Mock } from 'vitest';

/** A scripted HTTP host: the first matching route answers, anything else fails the test. */
export type FetchRoute = {
  /** Matches the full request URL. */
  match: (url: URL) => boolean;
  /** Answers; a thrown error is a network failure. */
  respond: (url: URL, signal: AbortSignal | undefined) => Response | Promise<Response>;
};

export type FetchHarness = {
  fetch: Mock;
  /** Every requested URL, in order. */
  urls: () => URL[];
  /** URLs no route matched (each also rejected, so the code under test fails). */
  unexpected: () => string[];
  restore: () => void;
};

export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

/**
 * Replaces `fetch` with a router. No test reaches the network: an unmatched
 * request (ipapi.co and ipinfo.io included) is recorded and rejected.
 */
export function stubFetch(routes: FetchRoute[]): FetchHarness {
  const requested: URL[] = [];
  const unexpected: string[] = [];
  const mock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(typeof input === 'string' || input instanceof URL ? input : input.url);

    requested.push(url);

    const route = routes.find((candidate) => candidate.match(url));

    if (!route) {
      unexpected.push(url.origin + url.pathname);

      return Promise.reject(new TypeError(`unexpected request to ${url.origin}`));
    }

    return Promise.resolve(route.respond(url, init?.signal ?? undefined));
  });

  vi.stubGlobal('fetch', mock);

  return {
    fetch: mock,
    urls: () => requested,
    unexpected: () => unexpected,
    restore: () => vi.unstubAllGlobals(),
  };
}

export const onHost =
  (host: string, path = '') =>
  (url: URL) =>
    url.host === host && url.pathname.startsWith(path);

/** A response that never settles until the request is aborted. */
export function hang(signal: AbortSignal | undefined): Promise<Response> {
  return new Promise((_resolve, reject) => {
    signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
  });
}

export const VC_HOST = 'weather.visualcrossing.com';
export const OM_HOST = 'api.open-meteo.com';
export const GEO_HOST = 'geocoding-api.open-meteo.com';

export const vcBody = (icon = 'partly-cloudy-day', feelslike: number | null = 20.2) => ({
  currentConditions: { feelslike, icon },
});

export const omBody = (code = 3, isDay = 1, apparent: number | null = 18.4) => ({
  current: { apparent_temperature: apparent, weather_code: code, is_day: isDay },
});
