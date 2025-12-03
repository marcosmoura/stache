import { useState, useEffect } from 'react';

// Singleton registry to manage media query listeners
class MediaQueryRegistry {
  private queries = new Map<
    string,
    { mql: MediaQueryList; listeners: Set<(matches: boolean) => void> }
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

      entry = { mql, listeners };
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
        this.queries.delete(query);
      }
    };
  }
}

const registry = new MediaQueryRegistry();

export const useMediaQuery = (query: string) => {
  const [matches, setMatches] = useState(false);

  useEffect(() => registry.subscribe(query, setMatches), [query]);

  return matches;
};
