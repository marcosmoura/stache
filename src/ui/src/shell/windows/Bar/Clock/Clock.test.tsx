import { afterEach, describe, expect, test, vi } from 'vitest';

import { renderWithBarData } from '../../../../../tests/renderBar';

import { Clock } from './Clock';
import { formatClock } from './Clock.state';

afterEach(() => {
  vi.useRealTimers();
});

describe('Clock', () => {
  test('formats weekday, month, two-digit day and 24 hour time with seconds', () => {
    expect(formatClock(new Date(2026, 0, 5, 9, 4, 3))).toBe('Mon Jan 05 09:04:03');
    expect(formatClock(new Date(2026, 10, 10, 23, 59, 59))).toBe('Tue Nov 10 23:59:59');
  });

  test('renders the clock icon and the time, and ticks every second', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    vi.setSystemTime(new Date(2026, 0, 5, 9, 4, 3));

    const { screen } = await renderWithBarData(<Clock />);

    expect(screen.container.querySelector('button svg')).not.toBeNull();
    expect(screen.container.querySelector('button span')?.textContent).toBe('Mon Jan 05 09:04:03');

    await vi.advanceTimersByTimeAsync(1000);

    await vi.waitFor(() =>
      expect(screen.container.querySelector('button span')?.textContent).toBe(
        'Mon Jan 05 09:04:04',
      ),
    );
  });

  test('stops its timer on unmount', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });

    const { screen } = await renderWithBarData(<Clock />);

    expect(vi.getTimerCount()).toBe(1);

    await screen.unmount();

    expect(vi.getTimerCount()).toBe(0);
  });
});
