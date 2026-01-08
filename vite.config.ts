/// <reference types="vitest/config" />

import path from 'node:path';

import react from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import wyw from '@wyw-in-js/vite';
import { defineConfig } from 'vite';

const host = process.env.TAURI_DEV_HOST;

// WebKit target configuration
const WEBKIT_SAFARI_VERSION = 18; // Targets Safari 18 to cover the latest two WebKit releases
const WEBKIT_TARGET = `safari${WEBKIT_SAFARI_VERSION}`;
const WEBKIT_TARGET_LIST = [WEBKIT_TARGET];

const hmr = {
  host,
  protocol: 'ws',
  port: 1421,
};

// Path to the UI source directory
const UI_DIR = './app/ui';

export default defineConfig({
  root: UI_DIR,
  envDir: __dirname,
  envPrefix: ['VITE_', 'API_'],
  plugins: [
    wyw({
      include: [`${UI_DIR}/**/*.styles.ts`],
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
    esbuildOptions: {
      target: WEBKIT_TARGET_LIST,
    },
    include: [
      '@hugeicons/core-free-icons',
      '@hugeicons/react',
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
  experimental: {
    enableNativePlugin: true,
  },
  build: {
    target: WEBKIT_TARGET_LIST,
    cssTarget: WEBKIT_TARGET,
    minify: 'oxc',
    cssMinify: 'lightningcss',
    assetsInlineLimit: 0,
    sourcemap: 'hidden',
    modulePreload: { polyfill: false },
    reportCompressedSize: true,
    chunkSizeWarningLimit: 300,
    outDir: path.resolve(__dirname, `${UI_DIR}/dist`),
    rolldownOptions: {
      output: {
        advancedChunks: {
          groups: [
            // React and React DOM in a separate chunk
            {
              name: 'react',
              test: /[\\/]node_modules[\\/](react|react-dom)[\\/]/,
            },
            // Other vendor dependencies (icons stay in main bundle)
            {
              name: 'vendor',
              test(id: string) {
                // Ignore non-node_modules
                if (!id.includes('node_modules')) {
                  return false;
                }

                // Exclude icon libraries - they stay in main bundle
                if (/@hugeicons|@icons-pack|hugeicons|simple-icons/.test(id)) {
                  return false;
                }

                // Exclude react (handled above)
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
        safari: WEBKIT_SAFARI_VERSION,
      },
    },
  },
  ssr: {
    external: [],
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
