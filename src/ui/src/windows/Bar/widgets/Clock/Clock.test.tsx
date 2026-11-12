import { Profiler } from 'react';

import { afterEach, describe, expect, it, vi } from 'vitest';

import { ticker } from '@/stores';

import { mockHost } from '../../../../../tests/ipc';
import { render } from '../../../../../tests/render';
import type { WidgetOptions } from '../types';

import { Clock } from './Clock';
import { formatClock, minuteOf } from './Clock.format';

type Options = WidgetOptions<'clock'>;

const DEFAULTS: Options = { show_date: true, show_seconds: true, hour_cycle: 'h23' };
const at = (hours: number, minutes: number, seconds: number) =>
  new Date(2026, 10, 11, hours, minutes, seconds);

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('formatClock', () => {
  const table: Array<[Partial<Options>, string]> = [
    [{}, 'Wed Nov 11 13:04:05'],
    [{ show_seconds: false }, 'Wed Nov 11 13:04'],
    [{ hour_cycle: 'h12' }, 'Wed Nov 11 01:04:05 PM'],
    [{ show_seconds: false, hour_cycle: 'h12' }, 'Wed Nov 11 01:04 PM'],
    [{ show_date: false }, '13:04:05'],
    [{ show_date: false, show_seconds: false }, '13:04'],
    [{ show_date: false, hour_cycle: 'h12' }, '01:04:05 PM'],
    [{ show_date: false, show_seconds: false, hour_cycle: 'h12' }, '01:04 PM'],
  ];

  for (const [options, text] of table) {
    it(`formats ${JSON.stringify(options)} as "${text}"`, () => {
      expect(formatClock(at(13, 4, 5), { ...DEFAULTS, ...options })).toBe(text);
    });
  }

  it('shows midnight as 00 (h23) and 12 AM (h12), noon as 12 PM', () => {
    expect(formatClock(at(0, 0, 0), DEFAULTS)).toBe('Wed Nov 11 00:00:00');
    expect(formatClock(at(0, 0, 0), { ...DEFAULTS, hour_cycle: 'h12' })).toBe(
      'Wed Nov 11 12:00:00 AM',
    );
    expect(formatClock(at(12, 0, 0), { ...DEFAULTS, hour_cycle: 'h12' })).toBe(
      'Wed Nov 11 12:00:00 PM',
    );
  });

  it('formats in a given time zone and across a DST change', () => {
    // 2026-11-01 06:00 UTC: New York falls back from 01:59:59 EDT to 01:00:00 EST.
    const change = Date.UTC(2026, 10, 1, 6, 0, 0);

    expect(formatClock(new Date(change - 1000), DEFAULTS, 'America/New_York')).toBe(
      'Sun Nov 01 01:59:59',
    );
    expect(formatClock(new Date(change), DEFAULTS, 'America/New_York')).toBe('Sun Nov 01 01:00:00');
    expect(formatClock(new Date(change), DEFAULTS, 'UTC')).toBe('Sun Nov 01 06:00:00');
  });

  it('selects whole minutes from a Date', () => {
    const minute = minuteOf(at(13, 4, 59).getTime() + 999);

    expect(new Date(minute)).toEqual(at(13, 4, 0));
  });
});

describe('Clock', () => {
  it('shows the current time with the clock icon, ticking from the shared ticker', async () => {
    vi.useFakeTimers({ toFake: ['Date'], now: at(13, 4, 5) });
    const screen = await render(<Clock id="clock" zone="end" options={DEFAULTS} />);
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('Wed Nov 11 13:04:05');
    expect(button.element().querySelector('svg[aria-hidden="true"]')).not.toBeNull();

    vi.setSystemTime(at(13, 4, 6));
    ticker.setState({ now: Date.now() });
    await expect.element(button).toHaveTextContent('Wed Nov 11 13:04:06');
  });

  it('follows an options change on the same instance', async () => {
    vi.useFakeTimers({ toFake: ['Date'], now: at(13, 4, 5) });
    const screen = await render(<Clock id="clock" zone="end" options={DEFAULTS} />);

    await expect.element(screen.getByRole('button')).toHaveTextContent('Wed Nov 11 13:04:05');
    await screen.rerender(
      <Clock
        id="clock"
        zone="end"
        options={{ show_date: false, show_seconds: false, hour_cycle: 'h12' }}
      />,
    );
    await expect.element(screen.getByRole('button')).toHaveTextContent('01:04 PM');
  });

  it('re-renders once a minute without seconds', async () => {
    vi.useFakeTimers({ toFake: ['Date'], now: at(13, 4, 5) });
    let renders = 0;
    const screen = await render(
      <Profiler id="clock" onRender={() => (renders += 1)}>
        <Clock id="clock" zone="end" options={{ ...DEFAULTS, show_seconds: false }} />
      </Profiler>,
    );

    await expect.element(screen.getByRole('button')).toHaveTextContent('Wed Nov 11 13:04');
    const before = renders;

    for (const second of [6, 7, 30, 59]) {
      vi.setSystemTime(at(13, 4, second));
      ticker.setState({ now: Date.now() });
    }

    expect(renders).toBe(before);
    vi.setSystemTime(at(13, 5, 0));
    ticker.setState({ now: Date.now() });
    await expect.element(screen.getByRole('button')).toHaveTextContent('Wed Nov 11 13:05');
    expect(renders).toBe(before + 1);
  });

  it('two Clocks share one timer, which stops when both unmount', async () => {
    expect(ticker.running()).toBe(false);
    const screen = await render(
      <>
        <Clock id="a" zone="end" options={DEFAULTS} />
        <Clock id="b" zone="start" options={{ ...DEFAULTS, show_date: false }} />
      </>,
    );

    await expect.element(screen.getByRole('button').first()).toBeVisible();
    expect(ticker.subscriberCount()).toBe(2);
    expect(ticker.running()).toBe(true);

    await screen.unmount();
    expect(ticker.subscriberCount()).toBe(0);
    expect(ticker.running()).toBe(false);
  });

  it('sends bar_action("clock") on click', async () => {
    const host = mockHost().answer('bar_action', { kind: 'completed' });
    const screen = await render(<Clock id="clock" zone="end" options={DEFAULTS} />);

    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
    expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'clock' });
  });
});
