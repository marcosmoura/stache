/// <reference types="vitest/config" />
import path from 'node:path';

import babel from '@rolldown/plugin-babel';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import wyw from '@wyw-in-js/vite';
import { defineConfig } from 'vite';

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
    root: path.resolve(import.meta.dirname, 'src/ui'),
    setupFiles: ['./tests/setup.ts'],
    include: ['./src/**/*.test.{ts,tsx}'],
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
      enabled: true,
      provider: playwright(),
      headless: true,
      instances: [{ browser: 'webkit' }],
      screenshotFailures: true,
      screenshotDirectory: 'test-results/screenshots',
    },
  },
});
