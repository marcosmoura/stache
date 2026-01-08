import type { ReactNode } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

/**
 * Creates a QueryClient configured for testing.
 * - Disables retries to make tests deterministic
 * - Sets gcTime to 0 to prevent query caching between tests
 * - Disables refetchOnMount and refetchOnWindowFocus to prevent unwanted refetches
 * - Sets staleTime to Infinity to consider preloaded data as fresh
 */
export const createTestQueryClient = () =>
  new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        gcTime: 0,
        staleTime: Infinity,
        refetchOnMount: false,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
      },
    },
  });

/**
 * Creates a wrapper component that provides React Query context.
 * Use this with render() or renderHook() from vitest-browser-react.
 *
 * @example
 * const queryClient = createTestQueryClient();
 * const wrapper = createQueryClientWrapper(queryClient);
 * render(<MyComponent />, { wrapper });
 */
export const createQueryClientWrapper = (queryClient: QueryClient) => {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
};

/**
 * Route configuration for fetch mocking.
 */
export interface FetchRoute {
  /** URL pattern to match (string for includes, RegExp for pattern matching) */
  pattern: string | RegExp;
  /** Response to return */
  response: unknown;
  /** Whether the request should fail */
  shouldFail?: boolean;
  /** HTTP status code (defaults to 200) */
  status?: number;
}

/**
 * Creates a fetch mock that routes requests based on URL patterns.
 * This is useful for mocking multiple API endpoints efficiently.
 *
 * @example
 * const mockFetch = createFetchMock([
 *   { pattern: 'ipinfo.io', response: { city: 'Berlin' } },
 *   { pattern: 'visualcrossing', response: { currentConditions: { feelslike: 20 } } },
 * ]);
 * vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);
 */
export const createFetchMock = (routes: FetchRoute[]) => {
  return (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);

    for (const route of routes) {
      const matches =
        typeof route.pattern === 'string' ? url.includes(route.pattern) : route.pattern.test(url);

      if (matches) {
        if (route.shouldFail) {
          return Promise.reject(new Error(`Mocked failure for ${url}`));
        }

        return Promise.resolve({
          ok: route.status ? route.status >= 200 && route.status < 300 : true,
          status: route.status ?? 200,
          json: () => Promise.resolve(route.response),
        } as Response);
      }
    }

    // No route matched - fail immediately instead of hanging
    return Promise.reject(new Error(`No mock route for: ${url}`));
  };
};
