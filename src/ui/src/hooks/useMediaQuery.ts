import { useSyncExternalStore } from 'react';

import { media } from '@/theme/tokens.stylex';

type Store = {
  subscribe: (onChange: () => void) => () => void;
  getSnapshot: () => boolean;
};

const stores = new Map<string, Store>();

/** One stable store per query string, so `useSyncExternalStore` never resubscribes. */
function storeFor(query: string): Store {
  let store = stores.get(query);

  if (!store) {
    store = {
      subscribe: (onChange) => {
        const list = window.matchMedia(query);

        list.addEventListener('change', onChange);

        return () => list.removeEventListener('change', onChange);
      },
      getSnapshot: () => window.matchMedia(query).matches,
    };
    stores.set(query, store);
  }

  return store;
}

/** Strips the `@media ` prefix of a StyleX media const, so CSS and script share one source. */
export const mediaQueryOf = (atRule: string) => atRule.replace(/^@media\s+/, '');

/** The compact (laptop) Bar query, `(width <= 2036px)`, from the StyleX `media.compact` const. */
export const COMPACT_QUERY = mediaQueryOf(media.compact);

/** Whether `query` (a media query list, without `@media`) matches; re-renders on change. */
export function useMediaQuery(query: string): boolean {
  const { subscribe, getSnapshot } = storeFor(query);

  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Whether the compact (laptop) Bar rules apply. */
export const useCompact = () => useMediaQuery(COMPACT_QUERY);
