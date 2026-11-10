/// <reference types="vitest/config" />
import { mkdirSync, writeFileSync } from 'node:fs';
import { release, type } from 'node:os';
import path from 'node:path';

import babel from '@rolldown/plugin-babel';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import wyw from '@wyw-in-js/vite';
import { defineConfig } from 'vite';

import { readFontRecord } from './src/ui/tests/bar-parity/fontFile.node.ts';

const uiRoot = path.resolve(import.meta.dirname, 'src/ui');
const parityDirectory = path.resolve(uiRoot, 'tests/bar-parity');
const uiInclude = ['./src/**/*.test.{ts,tsx}', './tests/**/*.test.{ts,tsx}'];
const parityInclude = ['./tests/bar-parity/**/*.test.{ts,tsx}'];

export default defineConfig({
  root: path.resolve(import.meta.dirname, 'src/ui'),
  envDir: import.meta.dirname,
  plugins: [
    wyw({
      include: [path.resolve(import.meta.dirname, 'src/ui/src/**/*.styles.tsx')],
    }),
    react(),
    babel({ presets: [reactCompilerPreset()] }),
  ],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
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
    setupFiles: ['./tests/setup.ts'],
    exclude: ['**/_legacy/**', '**/.worktrees/**', '**/dist/**', '**/node_modules/**'],
    coverage: {
      provider: 'istanbul',
      reporter: ['text', 'json', 'html'],
      include: ['./src/**/*.{ts,tsx}'],
      exclude: [
        './**/*.test.{ts,tsx}',
        './**/*.styles.tsx',
        './main.tsx',
        './tests/**',
        './vite-env.d.ts',
        '**/_legacy/**',
        '**/dist/**',
      ],
      thresholds: { lines: 80, functions: 80, branches: 65, statements: 80 },
    },
    browser: {
      headless: true,
      commands: {
        // Bar visual-parity gate (src/ui/tests/bar-parity). Node side of the page-driven checks.
        parityFont: () => ({ ...readFontRecord(), os: `${type()} ${release()}` }),
        // Colour scheme and reduced motion emulation for the whole page (nested frames follow).
        parityEmulate: async (
          { page },
          colorScheme: 'dark' | 'light',
          reducedMotion: 'reduce' | 'no-preference',
        ) => {
          await page.emulateMedia({ colorScheme, reducedMotion });
        },
        // Exact screenshot of one element in the tester frame, as base64 PNG. Animations are
        // never altered here; the test pauses them at a known timeline position beforehand.
        parityScreenshot: async ({ frame }, selector: string) => {
          const buffer = await (
            await frame()
          )
            .locator(selector)
            .screenshot({ animations: 'allow', caret: 'hide', scale: 'device', type: 'png' });

          return buffer.toString('base64');
        },
        // Real pointer hover over the nth element matching `inner` inside the iframe `outer`.
        parityPointer: async (
          { page, frame },
          action: 'hover' | 'down' | 'up' | 'reset',
          outer: string,
          inner: string,
          index: number,
        ) => {
          if (action === 'reset') {
            await page.mouse.up();
            await page.mouse.move(0, page.viewportSize()?.height ?? 1);

            return;
          }

          if (action === 'hover') {
            await (
              await frame()
            )
              .locator(outer)
              .contentFrame()
              .locator(inner)
              .nth(index)
              .hover({ force: true });

            return;
          }

          await (action === 'down' ? page.mouse.down() : page.mouse.up());
        },
        // Persists the parity run record. Deterministic content, fixed directories.
        parityWriteRecord: (_context, name: string, content: string) => {
          if (!/^[\w.-]+\.json$/u.test(name)) {
            throw new Error(`Invalid parity record name: ${name}`);
          }

          // A normal run writes to the untracked `test-results/bar-parity/`; the committed
          // records change only on an explicit `STACHE_PARITY_WRITE_RECORD=1`.
          const directory =
            process.env.STACHE_PARITY_WRITE_RECORD === '1'
              ? parityDirectory
              : path.resolve(import.meta.dirname, 'test-results/bar-parity');

          mkdirSync(directory, { recursive: true });
          writeFileSync(path.join(directory, name), content);
        },
        // Lets a test emulate `prefers-reduced-motion` through Playwright.
        setReducedMotion: async ({ page }, reduce: boolean) => {
          await page.emulateMedia({ reducedMotion: reduce ? 'reduce' : 'no-preference' });
        },
      },
      screenshotFailures: true,
      screenshotDirectory: 'test-results/screenshots',
    },
    projects: [
      // The UI suite (device scale 1). The Bar parity matrix runs separately
      // (`bun run test:parity`) because it takes minutes.
      {
        extends: true,
        test: {
          name: 'ui',
          include: uiInclude,
          exclude: [...parityInclude],
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            instances: [{ browser: 'webkit' }],
          },
        },
      },
      // The Bar parity matrix at device scale 1.
      {
        extends: true,
        test: {
          name: 'bar-parity-1x',
          include: parityInclude,
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            instances: [{ browser: 'webkit' }],
          },
        },
      },
      // The Bar parity matrix again at device scale 2.
      {
        extends: true,
        test: {
          name: 'bar-parity-2x',
          include: parityInclude,
          browser: {
            enabled: true,
            provider: playwright({ contextOptions: { deviceScaleFactor: 2 } }),
            headless: true,
            instances: [{ browser: 'webkit' }],
          },
        },
      },
    ],
  },
});
