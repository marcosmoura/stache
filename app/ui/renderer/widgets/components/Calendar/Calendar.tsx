import { ChevronLeft, ChevronRight } from '@hugeicons/core-free-icons';
import { cx } from '@linaria/core';
import { AnimatePresence, motion } from 'motion/react';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { motionRaw } from '@/design-system';

import { calculateMonthHeight, useCalendar } from './Calendar.state';
import * as styles from './Calendar.styles';
import type { AnimationDirection, DayProps, SlideAnimationProps } from './Calendar.types';

const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

const Day = ({ day, isToday }: DayProps) => (
  <div
    className={cx(
      styles.day,
      isToday && styles.dayToday,
      day.isOutsideMonth && styles.dayOutsideMonth,
    )}
  >
    {day.day}
  </div>
);

const SLIDE_DISTANCE = 60;

const slideVariants = {
  initial: (dir: AnimationDirection) => ({
    x: dir === 'right' ? -SLIDE_DISTANCE : SLIDE_DISTANCE,
    opacity: 0,
  }),
  animate: {
    x: 0,
    opacity: 1,
  },
  exit: (dir: AnimationDirection) => ({
    x: dir === 'right' ? SLIDE_DISTANCE : -SLIDE_DISTANCE,
    opacity: 0,
  }),
};

const springTransition = {
  type: 'spring',
  bounce: 0,
  duration: motionRaw.durationSlower,
} as const;

const SlideAnimation = ({
  element = 'div',
  direction,
  animationKey,
  ...rest
}: SlideAnimationProps) => {
  const MotionElement = element === 'span' ? motion.span : motion.div;

  return (
    <AnimatePresence mode="popLayout" custom={direction} initial={false}>
      <MotionElement
        key={animationKey}
        custom={direction}
        variants={slideVariants}
        initial="initial"
        animate="animate"
        exit="exit"
        transition={springTransition}
        {...rest}
      />
    </AnimatePresence>
  );
};

export const Calendar = () => {
  const {
    getKeyForDay,
    days,
    weekCount,
    monthYearLabel,
    animationDirection,
    goToPreviousMonth,
    goToNextMonth,
    goToToday,
    isToday,
  } = useCalendar();

  return (
    <Surface className={styles.calendar}>
      <header className={styles.header}>
        <button
          type="button"
          className={styles.navButton}
          onClick={goToPreviousMonth}
          aria-label="Previous month"
        >
          <Icon icon={ChevronLeft} size={18} />
        </button>

        <SlideAnimation
          element="span"
          animationKey={monthYearLabel}
          direction={animationDirection}
          className={styles.monthYear}
        >
          <Button className={styles.monthYearContainer} onClick={goToToday}>
            {monthYearLabel}
          </Button>
        </SlideAnimation>

        <button
          type="button"
          className={styles.navButton}
          onClick={goToNextMonth}
          aria-label="Next month"
        >
          <Icon icon={ChevronRight} size={18} />
        </button>
      </header>

      <div className={styles.weekdays}>
        {WEEKDAYS.map((day) => (
          <div key={day} className={styles.weekday}>
            {day}
          </div>
        ))}
      </div>

      <motion.div
        className={styles.monthContainer}
        animate={{ height: calculateMonthHeight(weekCount) }}
        transition={springTransition}
      >
        <SlideAnimation
          animationKey={monthYearLabel}
          direction={animationDirection}
          className={styles.month}
        >
          <div className={styles.days}>
            {days.map((day, index) => (
              <Day day={day} key={getKeyForDay(day, index)} isToday={isToday(day)} />
            ))}
          </div>
        </SlideAnimation>
      </motion.div>
    </Surface>
  );
};
