import { useState, useEffect } from 'react';

// Singleton registry to manage media query listeners
class MediaQueryRegistry {
  private queries = new Map<
    string,
    { mql: MediaQueryList; listeners: Set<(matches: boolean) => void>; handler: () => void }
  >();

  subscribe(query: string, callback: (matches: boolean) => void): () => void {
    let entry = this.queries.get(query);

    if (!entry) {
      const mql = window.matchMedia(query);
      const listeners = new Set<(matches: boolean) => void>();

      const handler = () => {
        listeners.forEach((listener) => listener(mql.matches));
      };

      mql.addEventListener('change', handler);

      entry = { mql, listeners, handler };
      this.queries.set(query, entry);
    }

    entry.listeners.add(callback);

    // Return initial value immediately
    callback(entry.mql.matches);

    // Return unsubscribe function
    return () => {
      const entry = this.queries.get(query);

      if (!entry) {
        return;
      }

      entry.listeners.delete(callback);

      // Clean up if no more listeners
      if (entry.listeners.size === 0) {
        entry.mql.removeEventListener('change', entry.handler);
        this.queries.delete(query);
      }
    };
  }
}

const registry = new MediaQueryRegistry();

// Get initial value synchronously to avoid hydration mismatch and extra render
const getInitialValue = (query: string): boolean => {
  if (typeof window === 'undefined') {
    return false;
  }
  return window.matchMedia(query).matches;
};

export function useMediaQuery(query: string) {
  const [matches, setMatches] = useState(() => getInitialValue(query));

  useEffect(() => registry.subscribe(query, setMatches), [query]);

  return matches;
}
