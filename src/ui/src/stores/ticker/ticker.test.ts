import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { createTicker } from './ticker';

/** The start of the minute containing `now`, from a `Date`. */
function minuteOf(now: number) {
  const date = new Date(now);

  date.setSeconds(0, 0);

  return date.getTime();
}

beforeEach(() => {
  vi.useFakeTimers({ now: new Date(2026, 10, 11, 13, 4, 5, 400) });
});

afterEach(() => {
  vi.useRealTimers();
});

describe('ticker', () => {
  it('has no timer without subscribers and stops with the last one', () => {
    const ticker = createTicker();

    expect(ticker.running()).toBe(false);
    expect(vi.getTimerCount()).toBe(0);

    const first = ticker.subscribe(() => {});
    const second = ticker.subscribe(() => {});

    expect(ticker.subscriberCount()).toBe(2);
    expect(vi.getTimerCount()).toBe(1);

    first();
    first();
    expect(ticker.subscriberCount()).toBe(1);
    expect(ticker.running()).toBe(true);

    second();
    expect(ticker.subscriberCount()).toBe(0);
    expect(ticker.running()).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('refreshes on the first subscription and ticks on each whole second', () => {
    const ticker = createTicker();

    vi.advanceTimersByTime(10_000);
    const seen: number[] = [];
    const stop = ticker.subscribe(({ now }) => seen.push(now));

    expect(ticker.getState().now).toBe(Date.now());
    seen.length = 0;

    vi.advanceTimersByTime(599);
    expect(seen).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(seen).toEqual([new Date(2026, 10, 11, 13, 4, 16).getTime()]);
    vi.advanceTimersByTime(2000);
    expect(seen.map((now) => new Date(now).getSeconds())).toEqual([16, 17, 18]);
    stop();
  });

  it('re-aligns to the whole second after a late tick', () => {
    const ticker = createTicker();
    const seen: number[] = [];
    const stop = ticker.subscribe(({ now }) => seen.push(now % 1000));

    seen.length = 0;
    vi.advanceTimersByTime(600);
    vi.setSystemTime(Date.now() + 300);
    vi.advanceTimersByTime(1000);
    expect(seen).toEqual([0, 300]);
    vi.advanceTimersByTime(700);
    expect(seen).toEqual([0, 300, 0]);
    stop();
  });

  it('one timer serves every subscriber', () => {
    const ticker = createTicker();
    const a = vi.fn();
    const b = vi.fn();
    const stopA = ticker.subscribe(a);
    const stopB = ticker.subscribe(b);

    a.mockClear();
    b.mockClear();
    vi.advanceTimersByTime(3600);
    expect(a).toHaveBeenCalledTimes(4);
    expect(b).toHaveBeenCalledTimes(4);
    expect(vi.getTimerCount()).toBe(1);
    stopA();
    stopB();
  });

  it('a minute selection changes once a minute', () => {
    const ticker = createTicker();
    const minutes = new Set<number>();
    const stop = ticker.subscribe(({ now }) => minutes.add(minuteOf(now)));

    // The first subscription publishes 13:04; three minutes add 13:05 to 13:07.
    vi.advanceTimersByTime(3 * 60_000);
    expect(minutes.size).toBe(4);
    expect([...minutes].every((minute) => new Date(minute).getSeconds() === 0)).toBe(true);
    stop();
  });
});
