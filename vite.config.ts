/// <reference types="vitest/config" />
import { readFileSync } from 'node:fs';
import path from 'node:path';

import babel from '@rolldown/plugin-babel';
import stylex from '@stylexjs/unplugin';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import { defineConfig } from 'vite';
import type { BrowserCommand } from 'vitest/node';

/** Media features a browser test may emulate (`commands.emulateMedia`). */
type EmulatedMedia = {
  colorScheme?: 'light' | 'dark' | 'no-preference' | null;
  reducedMotion?: 'reduce' | 'no-preference' | null;
};

/** Vitest browser command: Playwright `page.emulateMedia` for the test page. */
const emulateMedia: BrowserCommand<[EmulatedMedia]> = async (context, media) => {
  await context.page.emulateMedia(media);
};

const repositoryRoot = import.meta.dirname;
const uiRoot = path.resolve(repositoryRoot, 'src/ui');

/**
 * The `@/` alias is defined once, in `tsconfig.app.json`. Vite reads it through
 * `resolve.tsconfigPaths`; StyleX receives the same table as absolute paths so
 * `defineVars` imports resolve identically in dev, build and tests.
 */
const tsconfigPaths = (
  JSON.parse(readFileSync(path.join(repositoryRoot, 'tsconfig.app.json'), 'utf8')) as {
    compilerOptions: { paths: Record<string, string[]> };
  }
).compilerOptions.paths;

const stylexAliases = Object.fromEntries(
  Object.entries(tsconfigPaths).map(([alias, targets]) => [
    alias,
    targets.map((target) => path.resolve(repositoryRoot, target)),
  ]),
);

export default defineConfig({
  root: uiRoot,
  envDir: repositoryRoot,
  plugins: [
    // StyleX first (keeps Fast Refresh), then React, then the React Compiler.
    // In dev the plugin injects `/virtual:stylex.css` and `virtual:stylex:runtime`
    // into `index.html` itself (`devMode: 'full'`).
    stylex.vite({
      useCSSLayers: { before: ['reset'], prefix: 'stylex' },
      aliases: stylexAliases,
      unstable_moduleResolution: { type: 'commonJS', rootDir: repositoryRoot },
      devMode: 'full',
      // StyleX post-processes its CSS with lightningcss. Without explicit
      // targets it uses the default browserslist and lowers `light-dark()` to a
      // polyfill that needs `color-scheme` in the same stylesheet; the webview is
      // Safari 26, which supports it natively. (Major version in the high 16 bits.)
      lightningcssOptions: { targets: { safari: 26 << 16 } },
    }),
    react(),
    babel({ presets: [reactCompilerPreset()] }),
  ],
  resolve: {
    tsconfigPaths: true,
  },
  // Pre-bundled up front so a first dev or test run never re-optimizes and
  // reloads mid-run.
  optimizeDeps: {
    include: [
      '@hugeicons/core-free-icons',
      '@hugeicons/react',
      '@tanstack/react-query',
      '@tauri-apps/api/window',
      'motion/react',
      'motion/react-m',
      'zustand',
      'zustand/react/shallow',
    ],
    // Commit the scanned dependencies at once. Holding them until the crawl
    // ends deadlocks a cold cache: the warmed-up `*.stylex.ts` transforms wait
    // for the optimized `@stylexjs/stylex`, and the optimizer waits for them.
    holdUntilCrawlEnd: false,
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // The dev stylesheet (`/virtual:stylex.css`) is compiled from the modules
    // transformed so far. A `defineConsts` media query used as a style key is a
    // placeholder until its defining file is transformed; if the stylesheet is
    // compiled before that, lightningcss rejects it ("Invalid empty selector").
    // Transforming the token files at server start closes that window (dev and
    // Vitest; the build transforms every module first).
    warmup: { clientFiles: ['./src/theme/*.stylex.ts'] },
  },
  build: {
    target: 'safari26',
    cssTarget: 'safari26',
    cssMinify: 'lightningcss',
    outDir: 'dist',
  },
  test: {
    css: true,
    root: uiRoot,
    setupFiles: ['./tests/processShim.ts', './tests/setup.ts'],
    exclude: ['**/.worktrees/**', '**/dist/**', '**/node_modules/**', 'e2e/**'],
    coverage: {
      provider: 'istanbul',
      reporter: ['text', 'json', 'html'],
      include: ['./src/**/*.{ts,tsx}'],
      exclude: [
        './**/*.test.{ts,tsx}',
        './**/*.style.ts',
        './**/*.stylex.ts',
        './src/main.tsx',
        './src/ipc/bindings.gen.ts',
        './tests/**',
        './e2e/**',
        '**/dist/**',
      ],
      thresholds: { lines: 80, functions: 80, branches: 65, statements: 80 },
    },
    browser: {
      headless: true,
      screenshotFailures: true,
      screenshotDirectory: 'test-results/screenshots',
      commands: { emulateMedia },
    },
    projects: [
      {
        extends: true,
        test: {
          name: 'ui',
          include: ['./src/**/*.test.{ts,tsx}', './tests/**/*.test.{ts,tsx}'],
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            instances: [{ browser: 'webkit' }],
          },
        },
      },
    ],
  },
});
