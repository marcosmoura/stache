import type { ReactNode } from 'react';
import { createContext, use, useState } from 'react';

import {
  createStore,
  useStore as useZustandStore,
  type StateCreator,
  type StoreApi,
} from 'zustand';

/**
 * A zustand store scoped to a React subtree. Each mounted `Provider` creates
 * its own vanilla store; `useStore(selector)` reads it and throws outside a
 * provider; `useStoreApi()` is the imperative escape hatch.
 */
export function createStoreContext<TState>(createState: StateCreator<TState>, name = 'Store') {
  const Context = createContext<StoreApi<TState> | null>(null);

  function Provider({ children }: { children: ReactNode }) {
    const [store] = useState(() => createStore<TState>()(createState));

    return <Context value={store}>{children}</Context>;
  }

  function useStoreApi(): StoreApi<TState> {
    const store = use(Context);

    if (!store) {
      throw new Error(`${name}: useStore/useStoreApi must be used inside its <Provider>.`);
    }

    return store;
  }

  function useStore<TSlice>(selector: (state: TState) => TSlice): TSlice {
    return useZustandStore(useStoreApi(), selector);
  }

  return { Provider, useStore, useStoreApi };
}
