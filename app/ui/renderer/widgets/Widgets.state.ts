import { useCallback, useRef, useState } from 'react';

import { useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import type { EventCallback } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalPosition, LogicalSize } from '@tauri-apps/api/window';
import { useResizeObserver, useDebounceCallback } from 'usehooks-ts';

import { motionRaw } from '@/design-system';
import { useTauriEvent } from '@/hooks';
import { WidgetsEvents } from '@/types';

import type { Rect, WidgetConfig, WidgetNames, WindowFrame } from './Widgets.types';

const fetchBarPosition = async () => invoke<WindowFrame>('get_bar_window_frame');

const transition = {
  type: 'spring',
  bounce: 0,
  duration: motionRaw.durationSlow,
} as const;

/**
 * Widget window state management hook.
 *
 * Flow:
 * 1. `toggle_widgets` event received with widget name and trigger element rect
 * 2. If closed: set widget to render, store trigger rect, show window
 * 3. ResizeObserver detects content size and repositions/resizes window
 * 4. If open: animate out, then hide window and clear state
 */
export const useWidgets = () => {
  // The widget currently being rendered (null when closed)
  const [activeWidget, setActiveWidget] = useState<WidgetNames | null>(null);
  // The rect of the element that triggered the widget (for positioning)
  const [triggerRect, setTriggerRect] = useState<Rect | null>(null);
  // Controls the enter/exit animation state
  const [isAnimatingIn, setIsAnimatingIn] = useState(false);

  const contentRef = useRef<HTMLDivElement>(null);

  const { data: barPosition } = useSuspenseQuery({
    queryKey: ['widgets-frame'],
    queryFn: fetchBarPosition,
  });

  /**
   * Updates window size and position based on content dimensions and trigger rect.
   * Clamps horizontal position to keep window within screen bounds.
   */
  const updateWindowFrame = useCallback(
    async (contentSize: { width: number; height: number }) => {
      if (!barPosition || !triggerRect) {
        return;
      }

      const window = getCurrentWindow();
      const { width, height } = contentSize;

      if (width === 0 || height === 0) {
        return;
      }

      // Clamp x position to keep window within screen bounds
      const minX = 0;
      const maxX = barPosition.width - width;
      const clampedX = Math.max(minX, Math.min(triggerRect.x, maxX));

      // Position window below the bar, aligned with trigger element
      const windowX = clampedX + barPosition.x;
      const windowY = barPosition.y + barPosition.height + 4;

      await window.setSize(new LogicalSize(width, height));
      await window.setPosition(new LogicalPosition(windowX, windowY));
    },
    [barPosition, triggerRect],
  );

  /**
   * Opens the widget window with the specified configuration.
   */
  const openWidget = useCallback(
    async (config: WidgetConfig) => {
      if (!barPosition) {
        return;
      }

      const window = getCurrentWindow();

      // Set state to render the widget
      setActiveWidget(config.name);
      setTriggerRect(config.rect);

      // Show window (ResizeObserver will handle proper sizing)
      await window.show();

      // Trigger enter animation
      setIsAnimatingIn(true);
    },
    [barPosition],
  );

  /**
   * Closes the widget window with exit animation.
   */
  const closeWidget = useCallback(async () => {
    if (!barPosition) {
      return;
    }

    const window = getCurrentWindow();

    // Trigger exit animation
    setIsAnimatingIn(false);

    // Wait for animation to complete before hiding
    await new Promise((resolve) => setTimeout(resolve, transition.duration * 1000));

    updateWindowFrame({ width: 0, height: 0 });

    // Hide window and reset state
    await window.hide();
    setActiveWidget(null);
    setTriggerRect(null);
  }, [barPosition, updateWindowFrame]);

  /**
   * Handles the toggle_widgets event from the bar.
   */
  const handleToggle = useCallback<EventCallback<WidgetConfig>>(
    ({ payload }) => {
      if (activeWidget) {
        closeWidget();
      } else {
        openWidget(payload);
      }
    },
    [activeWidget, closeWidget, openWidget],
  );

  /**
   * ResizeObserver callback - updates window frame when content size changes.
   */
  const handleContentResize = useDebounceCallback((size: { width?: number; height?: number }) => {
    if (size.width && size.height) {
      updateWindowFrame({ width: size.width, height: size.height });
    }
  }, 4.17);

  useResizeObserver({
    ref: contentRef as React.RefObject<HTMLElement>,
    box: 'border-box',
    onResize: handleContentResize,
  });

  useTauriEvent<WidgetConfig>(WidgetsEvents.TOGGLE, handleToggle, 'widgets');
  useTauriEvent(WidgetsEvents.CLICK_OUTSIDE, closeWidget, 'widgets');

  return {
    /** Whether a widget is currently active (for conditional rendering) */
    isOpen: activeWidget !== null,
    /** Whether the widget should animate in (for motion) */
    isAnimatingIn,
    /** Animation transition config */
    transition,
    /** Ref to attach to the content container for size observation */
    contentRef,
    /** The widget name to render */
    activeWidget,
  };
};
