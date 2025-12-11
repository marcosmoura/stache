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

// Mock @hugeicons/react to avoid issues in test environment
vi.mock('@hugeicons/react', () => ({
  HugeiconsIcon: () => null,
}));

// Default mock implementations for Tauri invoke commands
const defaultInvokeMocks: Record<string, unknown> = {
  get_workspaces: [
    {
      name: 'coding',
      layout: 'tiling',
      screen: 'Main',
      isFocused: true,
      windowCount: 2,
      focusedApp: {
        name: 'Visual Studio Code',
        appId: 'com.microsoft.VSCode',
        windowCount: 1,
      },
    },
  ],
  get_current_media_info: {},
  get_battery_info: { percentage: 100, state: 'Full' },
  get_cpu_info: { usage: 25, temperature: 50 },
  is_system_awake: false,
  get_weather_config: {},
};

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockImplementation((cmd: string) => {
    if (cmd in defaultInvokeMocks) {
      return Promise.resolve(defaultInvokeMocks[cmd]);
    }
    return Promise.resolve(null);
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));
