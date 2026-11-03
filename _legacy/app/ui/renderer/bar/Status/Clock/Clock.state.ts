import { useSuspenseQuery } from '@tanstack/react-query';

import { useWidgetToggle } from '@/hooks';

import type { ClockState } from './Clock.types';

const formatter = new Intl.DateTimeFormat('en-US', {
  hour12: false,
  weekday: 'short',
  month: 'short',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
});

function getClock(): string {
  function findDatePart(parts: Intl.DateTimeFormatPart[], part: string): string {
    return parts.find((p) => p.type === part)?.value ?? '';
  }

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

export function useClock(): ClockState {
  const { data: clock } = useSuspenseQuery({
    queryKey: ['clock'],
    queryFn: getClock,
    refetchInterval: 1000,
    refetchOnMount: true,
  });

  const { ref, onClick } = useWidgetToggle('calendar');

  return { clock, ref, onClick };
}
