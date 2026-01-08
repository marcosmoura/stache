import { useCallback, useRef } from 'react';

import { useSuspenseQuery } from '@tanstack/react-query';

import type { WidgetConfig } from '@/renderer/widgets/Widgets.types';
import { WidgetsEvents } from '@/types';
import { emitTauriEvent } from '@/utils/emitTauriEvent';

function getClock(): string {
  const findDatePart = (parts: Intl.DateTimeFormatPart[], part: string) =>
    parts.find((p) => p.type === part)?.value || '';

  const options: Intl.DateTimeFormatOptions = {
    hour12: false,
    weekday: 'short',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  };

  const formatter = new Intl.DateTimeFormat('en-US', options);
  const time = new Date();
  const parts = formatter.formatToParts(time);

  const weekday = findDatePart(parts, 'weekday');
  const month = findDatePart(parts, 'month');
  const day = findDatePart(parts, 'day');
  const hour = findDatePart(parts, 'hour');
  const minute = findDatePart(parts, 'minute');
  const second = findDatePart(parts, 'second');

  return `${weekday} ${month} ${day} ${hour}:${minute}:${second}`;
}

export const useClock = () => {
  const { data: clock } = useSuspenseQuery({
    queryKey: ['clock'],
    queryFn: getClock,
    refetchInterval: 1000,
    refetchOnMount: true,
  });

  const ref = useRef<HTMLButtonElement>(null);

  const onClick = useCallback(() => {
    if (!ref.current) {
      return;
    }

    const { x, y, width, height } = ref.current.getBoundingClientRect();

    emitTauriEvent<WidgetConfig>({
      eventName: WidgetsEvents.TOGGLE,
      target: 'widgets',
      payload: {
        name: 'calendar',
        rect: {
          x,
          y,
          width,
          height,
        },
      },
    });
  }, []);

  return { clock, ref, onClick };
};
