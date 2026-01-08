import { describe, expect, test, vi, beforeEach } from 'vitest';
import { renderHook } from 'vitest-browser-react';

import type { WidgetNames } from '@/renderer/widgets/Widgets.types';
import { WidgetsEvents } from '@/types';
import { emitTauriEvent } from '@/utils/emitTauriEvent';

import { useWidgetToggle } from './useWidgetToggle';

// Mock emitTauriEvent
vi.mock('@/utils/emitTauriEvent', () => ({
  emitTauriEvent: vi.fn(),
}));

const mockEmitTauriEvent = vi.mocked(emitTauriEvent);

describe('useWidgetToggle', () => {
  beforeEach(() => {
    mockEmitTauriEvent.mockReset();
  });

  test('returns a ref and onClick handler', async () => {
    const { result } = await renderHook(() => useWidgetToggle('calendar'));

    expect(result.current.ref).toBeDefined();
    expect(result.current.ref.current).toBeNull();
    expect(typeof result.current.onClick).toBe('function');
  });

  test('does not emit event when ref is not attached', async () => {
    const { result } = await renderHook(() => useWidgetToggle('battery'));

    result.current.onClick();

    expect(mockEmitTauriEvent).not.toHaveBeenCalled();
  });

  test('emits toggle event with correct payload when clicked', async () => {
    const { result } = await renderHook(() => useWidgetToggle('weather'));

    // Create a mock element with getBoundingClientRect
    const mockElement = document.createElement('button');
    mockElement.getBoundingClientRect = vi.fn().mockReturnValue({
      x: 100,
      y: 50,
      width: 80,
      height: 30,
    });

    // Attach the ref to the mock element
    Object.defineProperty(result.current.ref, 'current', {
      value: mockElement,
      writable: true,
    });

    result.current.onClick();

    expect(mockEmitTauriEvent).toHaveBeenCalledTimes(1);
    expect(mockEmitTauriEvent).toHaveBeenCalledWith({
      eventName: WidgetsEvents.TOGGLE,
      target: 'widgets',
      payload: {
        name: 'weather',
        rect: { x: 100, y: 50, width: 80, height: 30 },
      },
    });
  });

  test('uses correct widget name for each widget type', async () => {
    const widgetNames: WidgetNames[] = ['calendar', 'battery', 'weather'];

    for (const widgetName of widgetNames) {
      mockEmitTauriEvent.mockReset();

      const { result } = await renderHook(() => useWidgetToggle(widgetName));

      const mockElement = document.createElement('button');
      mockElement.getBoundingClientRect = vi.fn().mockReturnValue({
        x: 0,
        y: 0,
        width: 50,
        height: 50,
      });

      Object.defineProperty(result.current.ref, 'current', {
        value: mockElement,
        writable: true,
      });

      result.current.onClick();

      expect(mockEmitTauriEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          payload: expect.objectContaining({
            name: widgetName,
          }),
        }),
      );
    }
  });

  test('onClick handler is stable across renders', async () => {
    const { result, rerender } = await renderHook(() => useWidgetToggle('calendar'));

    const firstOnClick = result.current.onClick;

    await rerender();

    expect(result.current.onClick).toBe(firstOnClick);
  });

  test('onClick handler updates when widget name changes', async () => {
    const { result, rerender } = await renderHook(
      (props?: { name: WidgetNames }) => useWidgetToggle(props!.name),
      { initialProps: { name: 'calendar' as WidgetNames } },
    );

    const mockElement = document.createElement('button');
    mockElement.getBoundingClientRect = vi.fn().mockReturnValue({
      x: 10,
      y: 20,
      width: 30,
      height: 40,
    });

    Object.defineProperty(result.current.ref, 'current', {
      value: mockElement,
      writable: true,
    });

    // Click with first widget name
    result.current.onClick();

    expect(mockEmitTauriEvent).toHaveBeenLastCalledWith(
      expect.objectContaining({
        payload: expect.objectContaining({ name: 'calendar' }),
      }),
    );

    // Change widget name
    await rerender({ name: 'battery' as WidgetNames });
    mockEmitTauriEvent.mockReset();

    // Reattach ref after rerender
    Object.defineProperty(result.current.ref, 'current', {
      value: mockElement,
      writable: true,
    });

    result.current.onClick();

    expect(mockEmitTauriEvent).toHaveBeenLastCalledWith(
      expect.objectContaining({
        payload: expect.objectContaining({ name: 'battery' }),
      }),
    );
  });

  test('works with custom element types', async () => {
    const { result } = await renderHook(() => useWidgetToggle<HTMLDivElement>('calendar'));

    const mockDiv = document.createElement('div');
    mockDiv.getBoundingClientRect = vi.fn().mockReturnValue({
      x: 200,
      y: 100,
      width: 150,
      height: 75,
    });

    Object.defineProperty(result.current.ref, 'current', {
      value: mockDiv,
      writable: true,
    });

    result.current.onClick();

    expect(mockEmitTauriEvent).toHaveBeenCalledWith({
      eventName: WidgetsEvents.TOGGLE,
      target: 'widgets',
      payload: {
        name: 'calendar',
        rect: { x: 200, y: 100, width: 150, height: 75 },
      },
    });
  });
});
