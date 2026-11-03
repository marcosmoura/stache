import { afterAll, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import { useMediaQuery } from './useMediaQuery';

type MockMediaQueryList = MediaQueryList & {
  trigger: (matches: boolean) => void;
};

let mediaQueries: Map<string, MockMediaQueryList>;
const currentInitialMatches = new Map<string, boolean>();
let matchMediaMock: ReturnType<typeof vi.fn>;
const originalMatchMedia = window.matchMedia;

const createMock = (query: string): MockMediaQueryList => {
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  let currentMatches = currentInitialMatches.get(query) ?? false;

  const mql = {
    media: query,
    onchange: null as ((this: MediaQueryList, ev: MediaQueryListEvent) => void) | null,
    addEventListener: (_event: 'change', listener: (event: MediaQueryListEvent) => void) => {
      listeners.add(listener);
    },
    removeEventListener: (_event: 'change', listener: (event: MediaQueryListEvent) => void) => {
      listeners.delete(listener);
    },
    addListener: (listener: (this: MediaQueryList, ev: MediaQueryListEvent) => void) => {
      void listener;
    },
    removeListener: (listener: (this: MediaQueryList, ev: MediaQueryListEvent) => void) => {
      void listener;
    },
    dispatchEvent: () => true,
    trigger: (matches: boolean) => {
      currentMatches = matches;
      const event = { matches, media: query } as MediaQueryListEvent;
      listeners.forEach((listener) => listener(event));
      mql.onchange?.call(mql as unknown as MediaQueryList, event);
    },
  } as const;

  return new Proxy(mql, {
    get(target, property, receiver) {
      if (property === 'matches') {
        return currentMatches;
      }

      return Reflect.get(target, property, receiver);
    },
  }) as unknown as MockMediaQueryList;
};

describe('useMediaQuery', () => {
  beforeAll(() => {
    matchMediaMock = vi.fn((query: string) => {
      let mql = mediaQueries.get(query);

      if (!mql) {
        mql = createMock(query);
        mediaQueries.set(query, mql);
      }

      return mql;
    });

    vi.stubGlobal('matchMedia', matchMediaMock);
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: matchMediaMock,
    });
  });

  beforeEach(() => {
    mediaQueries = new Map();
    currentInitialMatches.clear();
    matchMediaMock.mockClear();
  });

  afterAll(() => {
    vi.unstubAllGlobals();
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: originalMatchMedia,
    });
  });

  test('returns the current match state from matchMedia', async () => {
    const query = '(min-width: 100px)';
    currentInitialMatches.set(query, true);

    const { result } = await renderHook(() => useMediaQuery(query));

    await vi.waitFor(() => {
      expect(result.current).toBe(true);
    });
  });

  test('updates when the media query result changes', async () => {
    const query = '(prefers-reduced-motion: reduce)';
    const { result } = await renderHook(() => useMediaQuery(query));

    const mql = mediaQueries.get(query);
    expect(mql).toBeDefined();

    mql?.trigger(true);

    await vi.waitFor(() => {
      expect(result.current).toBe(true);
    });

    mql?.trigger(false);

    await vi.waitFor(() => {
      expect(result.current).toBe(false);
    });
  });

  test('shares the same media query list across subscribers', async () => {
    const query = '(min-width: 200px)';

    const first = await renderHook(() => useMediaQuery(query));
    const second = await renderHook(() => useMediaQuery(query));

    // Each hook call performs an initial synchronous match plus one shared subscription.
    expect(matchMediaMock).toHaveBeenCalledTimes(3);

    const mql = mediaQueries.get(query)!;

    mql.trigger(true);

    await vi.waitFor(() => {
      expect(first.result.current).toBe(true);
      expect(second.result.current).toBe(true);
    });

    await first.unmount();
    await second.unmount();
  });

  test('handles unsubscribe when query entry no longer exists', async () => {
    const query = '(min-width: 300px)';

    const { unmount } = await renderHook(() => useMediaQuery(query));

    // Manually delete the entry to simulate edge case
    mediaQueries.delete(query);

    // Should not throw when unsubscribing with no entry
    await unmount();

    expect(true).toBe(true); // Test passes if no error is thrown
  });
});
