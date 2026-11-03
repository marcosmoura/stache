import type { HTMLAttributes, JSX, PropsWithChildren } from 'react';

export interface CalendarDay {
  day: number;
  isOutsideMonth: boolean;
}

export interface DayProps extends PropsWithChildren<
  HTMLAttributes<HTMLDivElement | HTMLButtonElement>
> {
  day: CalendarDay;
  isToday: boolean;
}

export type AnimationDirection = 'left' | 'right';

export interface SlideAnimationProps extends PropsWithChildren {
  animationKey: string;
  direction: AnimationDirection;
  element?: keyof JSX.IntrinsicElements;
  className?: string;
}
