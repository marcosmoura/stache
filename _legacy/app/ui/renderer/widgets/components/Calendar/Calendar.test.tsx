import { describe, expect, test, vi } from 'vitest';
import { render, renderHook } from 'vitest-browser-react';

import { Calendar } from './Calendar';
import { DAY_HEIGHT, DAY_ROW_GAP } from './Calendar.constants';
import { calculateMonthHeight, useCalendar } from './Calendar.state';

const monthLabelFor = (date: Date) =>
  date.toLocaleDateString('en-US', { month: 'long', year: 'numeric' });

const monthOffsetLabel = (months: number) => {
  const date = new Date();
  date.setMonth(date.getMonth() + months);
  return monthLabelFor(date);
};

describe('Calendar', () => {
  test('renders the current month with weekday headers', async () => {
    const screen = await render(<Calendar />);

    for (const weekday of ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']) {
      await expect.element(screen.getByText(weekday, { exact: true })).toBeVisible();
    }

    await expect.element(screen.getByText(monthLabelFor(new Date()))).toBeVisible();
  });

  test('navigates to the next month', async () => {
    const screen = await render(<Calendar />);

    await screen.getByRole('button', { name: 'Next month' }).click();

    await expect.element(screen.getByText(monthOffsetLabel(1))).toBeVisible();
  });

  test('navigates to the previous month', async () => {
    const screen = await render(<Calendar />);

    await screen.getByRole('button', { name: 'Previous month' }).click();

    await expect.element(screen.getByText(monthOffsetLabel(-1))).toBeVisible();
  });

  test('goToToday returns to the current month', async () => {
    const screen = await render(<Calendar />);

    await screen.getByRole('button', { name: 'Previous month' }).click();
    await expect.element(screen.getByText(monthOffsetLabel(-1))).toBeVisible();

    await screen.getByText(monthOffsetLabel(-1)).click();
    await expect.element(screen.getByText(monthLabelFor(new Date()))).toBeVisible();
  });

  test('calculateMonthHeight accounts for day height and row gaps', () => {
    expect(calculateMonthHeight(4)).toBe(4 * DAY_HEIGHT + 3 * DAY_ROW_GAP);
    expect(calculateMonthHeight(5)).toBe(5 * DAY_HEIGHT + 4 * DAY_ROW_GAP);
    expect(calculateMonthHeight(6)).toBe(6 * DAY_HEIGHT + 5 * DAY_ROW_GAP);
  });
});

describe('useCalendar', () => {
  test('refreshes the today highlight across midnight', async () => {
    vi.useFakeTimers();

    try {
      vi.setSystemTime(new Date(2026, 7, 18, 23, 59, 50));

      const { result, unmount } = await renderHook(() => useCalendar());

      expect(result.current.isToday({ day: 18, isOutsideMonth: false })).toBe(true);

      // Cross midnight while the window stays open.
      await vi.advanceTimersByTimeAsync(15_000);

      await vi.waitFor(() => {
        expect(result.current.isToday({ day: 18, isOutsideMonth: false })).toBe(false);
      });
      expect(result.current.isToday({ day: 19, isOutsideMonth: false })).toBe(true);

      await unmount();
    } finally {
      vi.useRealTimers();
    }
  });
});
