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
  let callbackId = 0;
  const callbacks = new Map<number, (payload: unknown) => void>();

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'plugin:event|listen') {
        const eventId = typeof args?.handler === 'number' ? args.handler : 0;
        if (args?.event === 'stache://tiling/initialized') {
          setTimeout(() => callbacks.get(eventId)?.({ payload: { enabled: true } }), 0);
        }

        return Promise.resolve(eventId);
      }

      if (cmd === 'plugin:event|unlisten') {
        if (typeof args?.eventId === 'number') {
          callbacks.delete(args.eventId);
        }

        return Promise.resolve(null);
      }

      // Handle plugin commands (e.g., plugin:zustand|...)
      if (cmd.startsWith('plugin:')) {
        return Promise.resolve(null);
      }

      if (cmd in defaultInvokeMocks) {
        return Promise.resolve(defaultInvokeMocks[cmd]);
      }

      return Promise.resolve(null);
    },
    transformCallback: (callback: (payload: unknown) => void) => {
      callbackId += 1;
      callbacks.set(callbackId, callback);

      return callbackId;
    },
    convertFileSrc: (path: string) => path,
    metadata: { currentWindow: { label: 'bar' }, currentWebview: { label: 'bar' } },
  };

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event: string, eventId: number) => {
      callbacks.delete(eventId);
    },
  };
}

vi.doMock('@hugeicons/react', () => ({
  HugeiconsIcon: () => null,
}));

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
