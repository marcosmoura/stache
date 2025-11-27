import { invoke } from '@tauri-apps/api/core';

export function getClockInfo(): string {
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

export const openClockApp = () => invoke('open_app', { name: 'Clock' });
