/// <reference types="vitest/browser" />
/// <reference types="@vitest/browser/matchers" />
/// <reference types="@vitest/browser-playwright" />

import 'vitest-browser-react';
import { vi } from 'vitest';

// Polyfill crypto.getRandomValues for browser tests
if (typeof window !== 'undefined' && !window.crypto) {
  Object.defineProperty(window, 'crypto', {
    value: {
      getRandomValues: (buffer: Uint8Array) => {
        for (let i = 0; i < buffer.length; i++) {
          buffer[i] = Math.floor(Math.random() * 256);
        }
        return buffer;
      },
    },
    writable: true,
    configurable: true,
  });
}

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  getCurrentWebviewWindow: vi.fn().mockImplementation(async () => ({
    label: 'bar',
  })),
}));
