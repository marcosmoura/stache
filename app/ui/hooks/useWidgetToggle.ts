import { useCallback, useRef } from 'react';

import type { WidgetConfig, WidgetNames } from '@/renderer/widgets/Widgets.types';
import { WidgetsEvents } from '@/types';
import { emitTauriEvent } from '@/utils/emitTauriEvent';

/**
 * Hook for toggling widget visibility from status bar items.
 *
 * Encapsulates the common pattern of:
 * 1. Getting a ref to the trigger element
 * 2. Calculating bounding rect on click
 * 3. Emitting the widget toggle event
 *
 * @param widgetName - The name of the widget to toggle
 * @returns Object containing ref and onClick handler
 */
export const useWidgetToggle = <T extends HTMLElement = HTMLButtonElement>(
  widgetName: WidgetNames,
) => {
  const ref = useRef<T>(null);

  const onClick = useCallback(() => {
    if (!ref.current) {
      return;
    }

    const { x, y, width, height } = ref.current.getBoundingClientRect();

    emitTauriEvent<WidgetConfig>({
      eventName: WidgetsEvents.TOGGLE,
      target: 'widgets',
      payload: {
        name: widgetName,
        rect: { x, y, width, height },
      },
    });
  }, [widgetName]);

  return { ref, onClick };
};
