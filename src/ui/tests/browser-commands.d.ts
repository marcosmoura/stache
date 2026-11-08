import 'vitest/browser';

declare module 'vitest/browser' {
  interface BrowserCommands {
    /** Emulates (or clears) `prefers-reduced-motion: reduce` in the test page. */
    setReducedMotion: (reduce: boolean) => Promise<void>;
  }
}
