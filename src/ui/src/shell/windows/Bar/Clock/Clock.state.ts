import { useEffect, useState } from 'react';

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

function findDatePart(parts: Intl.DateTimeFormatPart[], part: string): string {
  return parts.find((p) => p.type === part)?.value ?? '';
}

/** Formats a moment the way legacy did, in the system time zone. */
export function formatClock(time: Date): string {
  const parts = formatter.formatToParts(time);

  const weekday = findDatePart(parts, 'weekday');
  const month = findDatePart(parts, 'month');
  const day = findDatePart(parts, 'day');
  const hour = findDatePart(parts, 'hour');
  const minute = findDatePart(parts, 'minute');
  const second = findDatePart(parts, 'second');

  return `${weekday} ${month} ${day} ${hour}:${minute}:${second}`;
}

const REFRESH_INTERVAL = 1000;

/**
 * The clock is a UI-local value (no native reading): it renders the current
 * time and re-reads it every second while mounted. The timer stops on unmount.
 */
export function useClock(): ClockState {
  const [clock, setClock] = useState(() => formatClock(new Date()));

  useEffect(() => {
    const timer = setInterval(() => setClock(formatClock(new Date())), REFRESH_INTERVAL);

    return () => clearInterval(timer);
  }, []);

  return { clock };
}
