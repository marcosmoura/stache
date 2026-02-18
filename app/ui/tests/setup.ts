/// <reference types="vitest/browser" />
/// <reference types="@vitest/browser/matchers" />
/// <reference types="@vitest/browser-playwright" />

import 'vitest-browser-react';
import { vi } from 'vitest';

// Suppress React act() warnings in vitest-browser-react tests
// These warnings occur because of async state updates in useLayoutEffect
// which are expected and handled by vitest-browser-react's retry-ability
const originalError = console.error;
console.error = (...args: unknown[]) => {
  const message = typeof args[0] === 'string' ? args[0] : '';
  if (message.includes('not wrapped in act(')) {
    return;
  }
  originalError.apply(console, args);
};

// Default mock implementations for Tauri invoke commands
const defaultInvokeMocks: Record<string, unknown> = {
  get_current_media_info: {},
  get_battery_info: { percentage: 100, state: 'Full' },
  get_cpu_info: { usage: 25, temperature: 50 },
  is_system_awake: false,
  get_weather_config: {},
  get_tiling_workspaces: [
    {
      name: 'terminal',
      screenId: 1,
      screenName: 'Built-in Display',
      layout: 'dwindle',
      isVisible: true,
      isFocused: true,
      windowCount: 1,
      windowIds: [1],
    },
  ],
  get_tiling_focused_workspace: 'terminal',
  get_tiling_focused_window: {
    id: 1,
    pid: 123,
    appId: 'com.mitchellh.ghostty',
    appName: 'Ghostty',
    title: 'zsh',
    workspace: 'terminal',
    isFocused: true,
  },
  get_tiling_current_workspace_windows: [
    {
      id: 1,
      pid: 123,
      appId: 'com.mitchellh.ghostty',
      appName: 'Ghostty',
      title: 'zsh',
      workspace: 'terminal',
      isFocused: true,
    },
  ],
  focus_tiling_workspace: undefined,
  focus_tiling_window: undefined,
  is_tiling_enabled: true,
};

// Mock window.__TAURI_INTERNALS__ for @tauri-store/zustand and other plugins
// that directly access Tauri internals instead of using the API
if (typeof window !== 'undefined') {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).__TAURI_INTERNALS__ = {
    invoke: (cmd: string) => {
      // Handle plugin commands (e.g., plugin:zustand|...)
      if (cmd.startsWith('plugin:')) {
        return Promise.resolve(null);
      }
      if (cmd in defaultInvokeMocks) {
        return Promise.resolve(defaultInvokeMocks[cmd]);
      }
      return Promise.resolve(null);
    },
    transformCallback: () => 0,
    convertFileSrc: (path: string) => path,
    metadata: { currentWindow: { label: 'bar' }, currentWebview: { label: 'bar' } },
  };
}

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

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockImplementation((cmd: string) => {
    if (cmd in defaultInvokeMocks) {
      return Promise.resolve(defaultInvokeMocks[cmd]);
    }
    return Promise.resolve(null);
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation((event: string, callback: (payload: unknown) => void) => {
    // Immediately trigger the tiling initialized event so Renderer becomes ready
    if (event === 'stache://tiling/initialized') {
      setTimeout(() => callback({ payload: { enabled: true } }), 0);
    }
    return Promise.resolve(() => {});
  }),
  emitTo: vi.fn().mockResolvedValue(undefined),
}));
