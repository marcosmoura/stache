import type { WidgetOptions } from '../types';

export type ClockOptions = WidgetOptions<'clock'>;

const formatters = new Map<string, Intl.DateTimeFormat>();

/** One cached `en-US` formatter per option set and time zone. */
function formatterFor({ show_seconds, hour_cycle }: ClockOptions, timeZone?: string) {
  const key = `${String(show_seconds)}|${hour_cycle}|${timeZone ?? ''}`;
  let formatter = formatters.get(key);

  if (!formatter) {
    formatter = new Intl.DateTimeFormat('en-US', {
      weekday: 'short',
      month: 'short',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: show_seconds ? '2-digit' : undefined,
      hourCycle: hour_cycle,
      timeZone,
    });
    formatters.set(key, formatter);
  }

  return formatter;
}

/**
 * The Clock text for `date`: `Wed Nov 11 13:04:05` by default. `show_date`
 * drops the date, `show_seconds` the seconds, `hour_cycle: 'h12'` gives a
 * 12-hour time with AM/PM (`Wed Nov 11 01:04:05 PM`). Always formatted from
 * the `Date` by `Intl.DateTimeFormat` in the system time zone (or `timeZone`).
 */
export function formatClock(date: Date, options: ClockOptions, timeZone?: string): string {
  const parts = formatterFor(options, timeZone).formatToParts(date);
  const part = (type: Intl.DateTimeFormatPartTypes) =>
    parts.find((entry) => entry.type === type)?.value ?? '';

  const time = [part('hour'), part('minute'), ...(options.show_seconds ? [part('second')] : [])]
    .join(':')
    .concat(options.hour_cycle === 'h12' ? ` ${part('dayPeriod')}` : '');

  if (!options.show_date) {
    return time;
  }

  return `${part('weekday')} ${part('month')} ${part('day')} ${time}`;
}

/** The start of the minute containing `now`, from a `Date` (never epoch arithmetic). */
export function minuteOf(now: number): number {
  const date = new Date(now);

  date.setSeconds(0, 0);

  return date.getTime();
}

/** The start of the second containing `now`. */
export function secondOf(now: number): number {
  const date = new Date(now);

  date.setMilliseconds(0);

  return date.getTime();
}
