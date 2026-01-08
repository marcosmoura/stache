import { useCallback, useMemo, useState } from 'react';

import { DAY_HEIGHT, DAY_ROW_GAP } from './Calendar.constants';
import type { AnimationDirection, CalendarDay } from './Calendar.types';

export const calculateMonthHeight = (weeks: number) =>
  weeks * DAY_HEIGHT + (weeks - 1) * DAY_ROW_GAP;

export function useCalendar() {
  const [currentDate, setCurrentDate] = useState(() => new Date());
  const [animationDirection, setAnimationDirection] = useState<AnimationDirection>('right');
  const today = useMemo(() => new Date(), []);

  const monthYearLabel = useMemo(() => {
    return currentDate.toLocaleDateString('en-US', {
      month: 'long',
      year: 'numeric',
    });
  }, [currentDate]);

  const days = useMemo(() => {
    const year = currentDate.getFullYear();
    const month = currentDate.getMonth();

    // First day of the month (0 = Sunday, 1 = Monday, etc.)
    const firstDayOfMonth = new Date(year, month, 1).getDay();

    // Number of days in the current month
    const daysInMonth = new Date(year, month + 1, 0).getDate();

    // Number of days in the previous month
    const daysInPrevMonth = new Date(year, month, 0).getDate();

    const daysArray: CalendarDay[] = [];

    // Add days from previous month
    for (let i = firstDayOfMonth - 1; i >= 0; i--) {
      daysArray.push({
        day: daysInPrevMonth - i,
        isOutsideMonth: true,
      });
    }

    // Add all days of the current month
    for (let day = 1; day <= daysInMonth; day++) {
      daysArray.push({
        day,
        isOutsideMonth: false,
      });
    }

    // Fill remaining slots with next month days
    const remainingSlots = 7 - (daysArray.length % 7);
    if (remainingSlots < 7) {
      for (let i = 1; i <= remainingSlots; i++) {
        daysArray.push({
          day: i,
          isOutsideMonth: true,
        });
      }
    }

    return daysArray;
  }, [currentDate]);

  const weekCount = useMemo(() => Math.ceil(days.length / 7), [days]);

  const goToPreviousMonth = useCallback(() => {
    setAnimationDirection('left');
    setCurrentDate((prev) => {
      const newDate = new Date(prev);
      newDate.setMonth(prev.getMonth() - 1);
      return newDate;
    });
  }, []);

  const goToNextMonth = useCallback(() => {
    setAnimationDirection('right');
    setCurrentDate((prev) => {
      const newDate = new Date(prev);
      newDate.setMonth(prev.getMonth() + 1);
      return newDate;
    });
  }, []);

  const goToToday = useCallback(() => {
    const todayDate = new Date();
    const currentYear = currentDate.getFullYear();
    const currentMonth = currentDate.getMonth();
    const todayYear = todayDate.getFullYear();
    const todayMonth = todayDate.getMonth();

    if (todayYear > currentYear || (todayYear === currentYear && todayMonth > currentMonth)) {
      setAnimationDirection('right');
    } else {
      setAnimationDirection('left');
    }

    setCurrentDate(todayDate);
  }, [currentDate]);

  const isToday = useCallback(
    (calendarDay: CalendarDay) => {
      if (calendarDay.isOutsideMonth) {
        return false;
      }

      return (
        calendarDay.day === today.getDate() &&
        currentDate.getMonth() === today.getMonth() &&
        currentDate.getFullYear() === today.getFullYear()
      );
    },
    [currentDate, today],
  );

  const getKeyForDay = useCallback(
    (calendarDay: CalendarDay, index: number) => {
      const prefix = calendarDay.isOutsideMonth ? 'outside' : 'current';
      return `${currentDate.getFullYear()}-${currentDate.getMonth()}-${prefix}-${calendarDay.day}-${index}`;
    },
    [currentDate],
  );

  return {
    days,
    weekCount,
    monthYearLabel,
    animationDirection,
    goToPreviousMonth,
    goToNextMonth,
    isToday,
    getKeyForDay,
    goToToday,
  };
}
