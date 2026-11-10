import 'vitest/browser';

declare module 'vitest/browser' {
  interface BrowserCommands {
    /** Emulates (or clears) `prefers-reduced-motion: reduce` in the test page. */
    setReducedMotion: (reduce: boolean) => Promise<void>;
    /** Bar parity: the installed Maple Mono NF medium file record plus the OS (node side). */
    parityFont: () => Promise<
      import('./bar-parity/fontFile.node').FontRecord & {
        os: string;
      }
    >;
    /** Bar parity: emulates colour scheme and reduced motion on the page. */
    parityEmulate: (
      colorScheme: 'dark' | 'light',
      reducedMotion: 'reduce' | 'no-preference',
    ) => Promise<void>;
    /** Bar parity: PNG screenshot (base64) of the first element matching a selector. */
    parityScreenshot: (selector: string) => Promise<string>;
    /** Bar parity: real pointer actions against an element inside a nested iframe. */
    parityPointer: (
      action: 'hover' | 'down' | 'up' | 'reset',
      outer: string,
      inner: string,
      index: number,
    ) => Promise<void>;
    /** Bar parity: writes a JSON run record next to the parity tests. */
    parityWriteRecord: (name: string, content: string) => Promise<void>;
  }
}
