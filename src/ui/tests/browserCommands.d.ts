import 'vitest/browser';

declare module 'vitest/browser' {
  interface BrowserCommands {
    /**
     * Playwright `page.emulateMedia` for the test page (defined in
     * `vite.config.ts`). `null` restores the real value.
     */
    emulateMedia: (media: {
      colorScheme?: 'light' | 'dark' | 'no-preference' | null;
      reducedMotion?: 'reduce' | 'no-preference' | null;
    }) => Promise<void>;
  }
}
