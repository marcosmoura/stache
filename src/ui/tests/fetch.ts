import { vi } from 'vitest';

type Fetch = typeof fetch;

/**
 * Replaces the global `fetch` with `mock` for requests to other origins only.
 * Requests to the test server itself keep the real `fetch`: the StyleX dev
 * runtime fetches its stylesheet on every hot update, and those calls must not
 * reach (or be counted by) a provider mock.
 */
export function stubRemoteFetch(mock: ReturnType<typeof vi.fn>) {
  const real = globalThis.fetch.bind(globalThis);
  const remote = mock as unknown as (...args: Parameters<Fetch>) => unknown;

  vi.stubGlobal('fetch', (...args: Parameters<Fetch>) => {
    const [input] = args;
    const href = input instanceof Request ? input.url : String(input);

    return new URL(href, location.href).origin === location.origin
      ? real(...args)
      : remote(...args);
  });
}
