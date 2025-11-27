/// <reference types="vitest/config" />

import path from 'node:path';

import react from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import wyw from '@wyw-in-js/vite';
import { defineConfig } from 'vite';

const host = process.env.TAURI_DEV_HOST;
const WEBKIT_SAFARI_VERSION = 18; // Targets Safari 18 to cover the latest two WebKit releases
const WEBKIT_TARGET = `safari${WEBKIT_SAFARI_VERSION}`;
const WEBKIT_TARGET_LIST = [WEBKIT_TARGET];

const hmr = {
  host,
  protocol: 'ws',
  port: 1421,
};

export default defineConfig({
  envPrefix: ['VITE_', 'API_'],
  plugins: [
    wyw({
      include: ['./src/**/*.styles.ts'],
    }),
    react({
      babel: {
        plugins: ['babel-plugin-react-compiler'],
      },
    }),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
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
      'react-dom',
      'react',
      'vitest-browser-react',
    ],
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? hmr : undefined,
    watch: {
      ignored: ['**/src-tauri/**', '**/coverage/**'],
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
    chunkSizeWarningLimit: 200,
    rolldownOptions: {
      output: {
        advancedChunks: {
          groups: [{ name: 'react', test: /[\\/]node_modules[\\/](react|react-dom)[\\/]/ }],
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
    setupFiles: ['./src/tests/setup.ts'],
    include: ['./src/**/*.test.{ts,tsx}'],
    coverage: {
      provider: 'istanbul',
      reporter: ['text', 'json', 'html'],
      include: ['src/**/*.{ts,tsx}'],
      exclude: [
        'src/**/*.test.{ts,tsx}',
        'src/**/*.styles.ts',
        'src/main.tsx',
        'src/test/**',
        'src/vite-env.d.ts',
      ],
      thresholds: {
        lines: 80,
        functions: 80,
        branches: 80,
        statements: 80,
      },
    },
    browser: {
      provider: playwright(),
      enabled: true,
      headless: true,
      isolate: true,
      instances: [{ browser: 'webkit' }, { browser: 'chromium' }],
      viewport: { width: 800, height: 600 },
      screenshotFailures: true,
      screenshotDirectory: 'test-results/screenshots',
    },
  },
});
