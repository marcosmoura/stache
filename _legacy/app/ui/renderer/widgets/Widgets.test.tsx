import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { WidgetsEvents } from '@/types';

import { Widgets } from './Widgets';
import type { Rect, WidgetConfig } from './Widgets.types';

const hoisted = vi.hoisted(() => {
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  const windowMock = {
    label: 'widgets',
    show: vi.fn(() => Promise.resolve()),
    hide: vi.fn(() => Promise.resolve()),
    setSize: vi.fn(() => Promise.resolve()),
    setPosition: vi.fn(() => Promise.resolve()),
  };

  return { listeners, windowMock };
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tauri-apps/api/event')>();
  return {
    ...actual,
    listen: vi.fn((eventName: string, callback: (event: { payload: unknown }) => void) => {
      hoisted.listeners.set(eventName, callback);
      return Promise.resolve(() => {
        hoisted.listeners.delete(eventName);
      });
    }),
    emitTo: vi.fn(() => Promise.resolve()),
  };
});

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => hoisted.windowMock,
  LogicalSize: class LogicalSize {
    constructor(
      public width: number,
      public height: number,
    ) {}
  },
  LogicalPosition: class LogicalPosition {
    constructor(
      public x: number,
      public y: number,
    ) {}
  },
}));

const FRAME = { x: 0, y: 0, width: 800, height: 28, screenWidth: 1920, screenHeight: 1080 };

const rect: Rect = { x: 100, y: 0, width: 50, height: 28 };

const emitToggle = (config: WidgetConfig) => {
  const callback = hoisted.listeners.get(WidgetsEvents.TOGGLE);
  callback?.({ payload: config });
};

const installDefaultInvokeMock = () => {
  vi.mocked(invoke).mockImplementation(async (command: string) => {
    if (command === 'get_bar_window_frame') {
      return FRAME;
    }
    if (command === 'get_weather_config') {
      return {
        provider: 'auto',
        visualCrossingApiKey: '',
        defaultLocation: 'Berlin, Germany',
      };
    }
    return null;
  });
};

describe('Widgets', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    hoisted.listeners.clear();
    installDefaultInvokeMock();
  });

  test('renders a retry fallback on transient query error and recovers', async () => {
    vi.mocked(invoke).mockImplementation(async (command: string) => {
      if (command === 'get_bar_window_frame') {
        throw new Error('transient frame error');
      }
      return null;
    });

    const screen = await render(<Widgets />);

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();

    // Transient failure clears up — retry must recover without remounting the window.
    installDefaultInvokeMock();

    await screen.getByRole('button', { name: 'Retry' }).click();

    await vi.waitFor(async () => {
      await expect.element(screen.getByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
    });

    await expect.element(screen.getByText('Something went wrong')).not.toBeInTheDocument();
  });

  test('renders the calendar widget when opened', async () => {
    const screen = await render(<Widgets />);

    await vi.waitFor(() => {
      expect(hoisted.listeners.has(WidgetsEvents.TOGGLE)).toBe(true);
    });

    emitToggle({ name: 'calendar', rect });

    await expect.element(screen.getByText('Sun')).toBeVisible();
  });

  test('renders the battery widget when opened', async () => {
    const screen = await render(<Widgets />);

    await vi.waitFor(() => {
      expect(hoisted.listeners.has(WidgetsEvents.TOGGLE)).toBe(true);
    });

    emitToggle({ name: 'battery', rect });

    await expect.element(screen.getByText('Battery', { exact: true })).toBeVisible();
  });

  test('renders the weather widget when opened', async () => {
    const screen = await render(<Widgets />);

    await vi.waitFor(() => {
      expect(hoisted.listeners.has(WidgetsEvents.TOGGLE)).toBe(true);
    });

    emitToggle({ name: 'weather', rect });

    await expect.element(screen.getByText("Today's weather")).toBeVisible();
  });
});
