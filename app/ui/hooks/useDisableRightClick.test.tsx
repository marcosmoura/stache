import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import {
  attachContextMenuListener,
  isDisableRightClickDevModeForTesting,
  resetDisableRightClickForTesting,
  setDisableRightClickDevModeForTesting,
  useDisableRightClick,
} from './useDisableRightClick';

let addEventListenerSpy: ReturnType<typeof vi.spyOn>;
let removeEventListenerSpy: ReturnType<typeof vi.spyOn>;
const originalDev = import.meta.env.DEV;

describe('useDisableRightClick', () => {
  beforeEach(() => {
    Object.defineProperty(import.meta.env, 'DEV', {
      configurable: true,
      writable: true,
      value: false,
    });

    const originalAddEventListener = document.addEventListener.bind(document);
    const originalRemoveEventListener = document.removeEventListener.bind(document);

    addEventListenerSpy = vi
      .spyOn(document, 'addEventListener')
      .mockImplementation(((
        type: string,
        listener: EventListenerOrEventListenerObject,
        options?: boolean | AddEventListenerOptions,
      ) => originalAddEventListener(type, listener, options)) as typeof document.addEventListener);
    removeEventListenerSpy = vi
      .spyOn(document, 'removeEventListener')
      .mockImplementation(((
        type: string,
        listener: EventListenerOrEventListenerObject,
        options?: boolean | EventListenerOptions,
      ) =>
        originalRemoveEventListener(
          type,
          listener,
          options,
        )) as typeof document.removeEventListener);

    resetDisableRightClickForTesting();
    setDisableRightClickDevModeForTesting(false);
  });

  afterEach(() => {
    addEventListenerSpy.mockRestore();
    removeEventListenerSpy.mockRestore();
    Object.defineProperty(import.meta.env, 'DEV', {
      configurable: true,
      writable: true,
      value: originalDev,
    });
    resetDisableRightClickForTesting();
    setDisableRightClickDevModeForTesting(null);
  });

  test('attaches and detaches the contextmenu listener outside dev', () => {
    expect(import.meta.env.DEV).toBe(false);
    expect(isDisableRightClickDevModeForTesting()).toBe(false);

    const cleanup = attachContextMenuListener();

    expect(addEventListenerSpy).toHaveBeenCalled();

    const contextMenuCall = addEventListenerSpy.mock.calls.find(
      ([event]: [string, ...unknown[]]) => event === 'contextmenu',
    );

    expect(contextMenuCall).toBeDefined();

    if (!contextMenuCall) {
      throw new Error('Context menu listener should be registered');
    }

    const handler = contextMenuCall[1] as EventListener;

    cleanup();

    expect(removeEventListenerSpy).toHaveBeenCalledWith('contextmenu', handler);
  });

  test('does not register the listener multiple times for re-renders', () => {
    expect(import.meta.env.DEV).toBe(false);
    expect(isDisableRightClickDevModeForTesting()).toBe(false);

    const cleanupFirst = attachContextMenuListener();
    const cleanupSecond = attachContextMenuListener();

    const contextMenuCalls = addEventListenerSpy.mock.calls.filter(
      ([event]: [string, ...unknown[]]) => event === 'contextmenu',
    );

    expect(contextMenuCalls).toHaveLength(1);

    cleanupFirst();
    cleanupSecond();
  });

  test('skips registering the listener while in dev mode', () => {
    addEventListenerSpy.mockClear();
    removeEventListenerSpy.mockClear();

    Object.defineProperty(import.meta.env, 'DEV', {
      configurable: true,
      writable: true,
      value: true,
    });

    resetDisableRightClickForTesting();
    setDisableRightClickDevModeForTesting(true);

    expect(import.meta.env.DEV).toBe(true);
    expect(isDisableRightClickDevModeForTesting()).toBe(true);

    const cleanup = attachContextMenuListener();

    const contextMenuCalls = addEventListenerSpy.mock.calls.filter(
      ([event]: [string, ...unknown[]]) => event === 'contextmenu',
    );

    expect(contextMenuCalls).toHaveLength(0);

    cleanup();
    expect(removeEventListenerSpy).not.toHaveBeenCalled();
  });

  test('cleanup function does nothing if listener is not attached', () => {
    expect(import.meta.env.DEV).toBe(false);
    expect(isDisableRightClickDevModeForTesting()).toBe(false);

    const cleanup = attachContextMenuListener();

    removeEventListenerSpy.mockClear();

    // First cleanup should remove the listener
    cleanup();
    expect(removeEventListenerSpy).toHaveBeenCalledTimes(1);

    removeEventListenerSpy.mockClear();

    // Second cleanup should do nothing since listener is already removed
    cleanup();
    expect(removeEventListenerSpy).not.toHaveBeenCalled();
  });

  test('resetDisableRightClickForTesting does nothing when listener is not attached', () => {
    removeEventListenerSpy.mockClear();

    // Call reset when nothing is attached
    resetDisableRightClickForTesting();

    expect(removeEventListenerSpy).not.toHaveBeenCalled();
  });

  test('resetDisableRightClickForTesting removes listener when attached', () => {
    expect(import.meta.env.DEV).toBe(false);

    attachContextMenuListener();

    removeEventListenerSpy.mockClear();

    resetDisableRightClickForTesting();

    expect(removeEventListenerSpy).toHaveBeenCalledWith('contextmenu', expect.any(Function));
  });

  test('useDisableRightClick hook attaches listener on mount', async () => {
    addEventListenerSpy.mockClear();

    const { unmount } = await renderHook(() => useDisableRightClick());

    const contextMenuCalls = addEventListenerSpy.mock.calls.filter(
      ([event]: [string, ...unknown[]]) => event === 'contextmenu',
    );

    expect(contextMenuCalls).toHaveLength(1);

    await unmount();
  });

  test('isDisableRightClickDevModeForTesting falls back to import.meta.env when override is null', () => {
    Object.defineProperty(import.meta.env, 'DEV', {
      configurable: true,
      writable: true,
      value: true,
    });

    setDisableRightClickDevModeForTesting(null);

    expect(isDisableRightClickDevModeForTesting()).toBe(true);
  });
});
