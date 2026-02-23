/// <reference types="vitest/config" />

import path from 'node:path';

import react from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import wyw from '@wyw-in-js/vite';
import { defineConfig } from 'vite';

const host = process.env.TAURI_DEV_HOST;

const UI_DIR = './app/ui';
const SAFARI_VERSION = 18;
const BROWSER_TARGET = `safari${SAFARI_VERSION}`;
const BROWSER_TARGET_LIST = [BROWSER_TARGET];

const hmr = {
  host,
  protocol: 'ws',
  port: 1421,
};

export default defineConfig({
  root: UI_DIR,
  envDir: __dirname,
  envPrefix: ['VITE_', 'API_'],
  plugins: [
    wyw({
      include: [`${UI_DIR}/**/*.styles.ts`],
      babelOptions: {
        plugins: [
          [
            'module-resolver',
            {
              alias: {
                '@': path.resolve(__dirname, UI_DIR),
              },
              extensions: ['.ts', '.tsx'],
            },
          ],
        ],
      },
      importOverrides: {
        './app/ui/design-system/index.ts': { unknown: 'allow' },
        './app/ui/design-system/colors.ts': { unknown: 'allow' },
        './app/ui/design-system/motion.ts': { unknown: 'allow' },
        './app/ui/utils/media-query.ts': { unknown: 'allow' },
        './app/ui/renderer/widgets/components/Calendar/Calendar.constants.ts': { unknown: 'allow' },
      },
    }),
    react({
      babel: {
        plugins: ['babel-plugin-react-compiler'],
      },
    }),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, UI_DIR),
    },
    conditions: ['module', 'production'],
  },
  optimizeDeps: {
    include: [
      '@hugeicons/core-free-icons',
      '@hugeicons/react',
      '@icons-pack/react-simple-icons',
      '@tanstack/react-query',
      '@tauri-apps/api/webviewWindow',
      '@tauri-store/zustand',
      'react-dom',
      'react',
      'vitest-browser-react',
      'zustand',
      'zustand/middleware/immer',
    ],
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? hmr : undefined,
    watch: {
      ignored: ['**/app/native/**', '**/coverage/**'],
    },
  },
  build: {
    target: BROWSER_TARGET_LIST,
    cssTarget: BROWSER_TARGET,
    minify: 'oxc',
    cssMinify: 'lightningcss',
    assetsInlineLimit: 4096,
    sourcemap: 'hidden',
    modulePreload: { polyfill: false },
    reportCompressedSize: true,
    chunkSizeWarningLimit: 300,
    outDir: path.resolve(__dirname, `${UI_DIR}/dist`),
    rolldownOptions: {
      output: {
        advancedChunks: {
          groups: [
            {
              name: 'react',
              test: /[\\/]node_modules[\\/](react|react-dom)[\\/]/,
            },
            {
              name: 'vendor',
              test(id: string): boolean {
                if (!id.includes('node_modules')) {
                  return false;
                }

                if (/@hugeicons|@icons-pack|hugeicons|simple-icons/.test(id)) {
                  return false;
                }

                if (/[\\/](react|react-dom)[\\/]/.test(id)) {
                  return false;
                }

                return true;
              },
            },
          ],
        },
      },
    },
  },
  css: {
    transformer: 'lightningcss',
    lightningcss: {
      targets: {
        safari: SAFARI_VERSION,
      },
    },
  },
  test: {
    css: true,
    root: path.resolve(__dirname, UI_DIR),
    setupFiles: ['./tests/setup.ts'],
    include: ['./**/*.test.{ts,tsx}'],
    coverage: {
      provider: 'istanbul',
      reporter: ['text', 'json', 'html'],
      include: ['./**/*.{ts,tsx}'],
      exclude: [
        './**/*.test.{ts,tsx}',
        './**/*.styles.ts',
        './**/*.state.ts',
        './main.tsx',
        './test/**',
        './vite-env.d.ts',
      ],
      thresholds: {
        lines: 80,
        functions: 80,
        branches: 65,
        statements: 80,
      },
    },
    browser: {
      provider: playwright(),
      enabled: true,
      headless: true,
      isolate: true,
      instances: [{ browser: 'webkit' }],
      viewport: { width: 800, height: 600 },
      screenshotFailures: true,
      screenshotDirectory: 'test-results/screenshots',
    },
  },
});
