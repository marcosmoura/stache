import { invoke } from '@tauri-apps/api/core';
import { useResizeObserver } from 'usehooks-ts';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';
import { WidgetsEvents } from '@/types';

import { useWidgets } from './Widgets.state';
import type { Rect, WidgetConfig } from './Widgets.types';

const hoisted = vi.hoisted(() => {
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  const showController: { current: (() => void) | null } = { current: null };
  const windowMock = {
    label: 'widgets',
    show: vi.fn(() => Promise.resolve()),
    hide: vi.fn(() => Promise.resolve()),
    setSize: vi.fn(() => Promise.resolve()),
    setPosition: vi.fn(() => Promise.resolve()),
  };

  return { listeners, showController, windowMock };
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

vi.mock('usehooks-ts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('usehooks-ts')>();
  return {
    ...actual,
    useResizeObserver: vi.fn(),
  };
});

const FRAME = { x: 0, y: 0, width: 800, height: 28, screenWidth: 1920, screenHeight: 1080 };

const rect: Rect = { x: 100, y: 0, width: 50, height: 28 };
const calendarConfig: WidgetConfig = { name: 'calendar', rect };
const batteryConfig: WidgetConfig = { name: 'battery', rect };

const emitEvent = (eventName: string, payload: unknown) => {
  const callback = hoisted.listeners.get(eventName);
  callback?.({ payload });
};

const openWidget = (config: WidgetConfig) => emitEvent(WidgetsEvents.TOGGLE, config);
const clickOutside = () => emitEvent(WidgetsEvents.CLICK_OUTSIDE, undefined);

let resizeObserverCalls: Array<((size: { width?: number; height?: number }) => void) | undefined>;

describe('useWidgets', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    hoisted.listeners.clear();
    hoisted.showController.current = null;
    resizeObserverCalls = [];

    vi.mocked(invoke).mockImplementation(async (command: string) => {
      if (command === 'get_bar_window_frame') {
        return FRAME;
      }
      return null;
    });

    vi.mocked(useResizeObserver).mockImplementation((options) => {
      resizeObserverCalls.push(options.onResize);
      return { width: 0, height: 0 };
    });
  });

  const renderWidgets = async () => {
    const queryClient = createTestQueryClient();
    const hook = await renderHook(() => useWidgets(), {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(hoisted.listeners.has(WidgetsEvents.TOGGLE)).toBe(true);
      expect(hoisted.listeners.has(WidgetsEvents.CLICK_OUTSIDE)).toBe(true);
    });

    return { ...hook, queryClient };
  };

  test('opens a widget and shows the window on toggle', async () => {
    const { result, queryClient, unmount } = await renderWidgets();

    openWidget(calendarConfig);

    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('calendar');
    });

    expect(result.current.isOpen).toBe(true);
    expect(result.current.isAnimatingIn).toBe(true);
    expect(hoisted.windowMock.show).toHaveBeenCalledTimes(1);

    await unmount();
    queryClient.clear();
  });

  test('closes the widget after the exit animation', async () => {
    const { result, queryClient, unmount } = await renderWidgets();

    openWidget(calendarConfig);
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('calendar');
    });

    clickOutside();

    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBeNull();
    });
    await vi.waitFor(() => {
      expect(result.current.isAnimatingIn).toBe(false);
    });

    expect(hoisted.windowMock.hide).toHaveBeenCalledTimes(1);

    await unmount();
    queryClient.clear();
  });

  test('rapid double close collapses into a single hide', async () => {
    const { result, queryClient, unmount } = await renderWidgets();

    openWidget(calendarConfig);
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('calendar');
    });

    clickOutside();
    clickOutside();

    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBeNull();
    });

    expect(hoisted.windowMock.hide).toHaveBeenCalledTimes(1);

    await unmount();
    queryClient.clear();
  });

  test('reopens a widget after a full close cycle', async () => {
    const { result, queryClient, unmount } = await renderWidgets();

    openWidget(calendarConfig);
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('calendar');
    });

    clickOutside();
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBeNull();
    });

    openWidget(batteryConfig);
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('battery');
    });

    expect(result.current.isAnimatingIn).toBe(true);
    expect(hoisted.windowMock.show).toHaveBeenCalledTimes(2);

    await unmount();
    queryClient.clear();
  });

  test('a close superseding a pending open does not trigger the enter animation', async () => {
    const { result, queryClient, unmount } = await renderWidgets();

    // Hold the show promise open so the close can supersede the open.
    hoisted.windowMock.show.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          hoisted.showController.current = resolve;
        }),
    );

    openWidget(calendarConfig);
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('calendar');
    });

    // Toggle while the open is still in flight — this starts a close.
    openWidget(batteryConfig);

    hoisted.showController.current?.();

    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBeNull();
    });

    expect(result.current.isAnimatingIn).toBe(false);
    expect(hoisted.windowMock.hide).toHaveBeenCalledTimes(1);

    await unmount();
    queryClient.clear();
  });

  test('passes a stable resize callback across re-renders', async () => {
    const { result, queryClient, unmount } = await renderWidgets();

    const firstCallback = resizeObserverCalls[resizeObserverCalls.length - 1];

    openWidget(calendarConfig);
    await vi.waitFor(() => {
      expect(result.current.activeWidget).toBe('calendar');
    });

    // Opening triggers re-renders; the onResize passed to the observer must not change.
    expect(resizeObserverCalls.length).toBeGreaterThan(1);
    const lastCallback = resizeObserverCalls[resizeObserverCalls.length - 1];
    expect(lastCallback).toBe(firstCallback);

    await unmount();
    queryClient.clear();
  });
});
