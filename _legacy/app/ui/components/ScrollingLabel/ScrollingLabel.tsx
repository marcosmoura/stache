import { useEffect, useMemo, useRef, useState } from 'react';

import { cx } from '@linaria/core';

import * as styles from './ScrollingLabel.styles';
import type { ScrollingLabelProps, ScrollState } from './ScrollingLabel.types';

export function ScrollingLabel({
  children,
  className,
  scrollSpeed = 60,
  ...props
}: ScrollingLabelProps) {
  const wrapperRef = useRef<HTMLDivElement>(null);
  const labelRef = useRef<HTMLSpanElement>(null);
  const [scrollState, setScrollState] = useState<ScrollState>({ start: 0, end: 0, distance: 0 });

  useEffect(() => {
    const wrapper = wrapperRef.current;
    const label = labelRef.current;

    if (!wrapper || !label) {
      return;
    }

    const calculateScrollState = () => {
      const wrapperWidth = wrapper.offsetWidth;
      const labelWidth = label.scrollWidth;
      const overflow = labelWidth - wrapperWidth;

      if (overflow > 0) {
        setScrollState({
          start: wrapperWidth,
          end: -labelWidth,
          distance: wrapperWidth + labelWidth,
        });
      } else {
        setScrollState({ start: 0, end: 0, distance: 0 });
      }
    };

    calculateScrollState();

    const resizeObserver = new ResizeObserver(calculateScrollState);
    resizeObserver.observe(wrapper);
    resizeObserver.observe(label);

    return () => resizeObserver.disconnect();
  }, [children]);

  const isScrolling = scrollState.distance > 0;
  const scrollStyles = useMemo(() => {
    const scrollDuration = Math.max(1, 1 + scrollState.distance / scrollSpeed);

    return {
      '--scroll-start': `${scrollState.start}px`,
      '--scroll-end': `${scrollState.end}px`,
      '--scroll-duration': `${scrollDuration}s`,
    };
  }, [scrollState, scrollSpeed]);

  return (
    <div
      ref={wrapperRef}
      className={cx(styles.wrapper, isScrolling && styles.scrollingWrapper, className)}
      {...props}
    >
      <span
        ref={labelRef}
        className={cx(styles.label, isScrolling && styles.scrollingLabel)}
        style={scrollStyles}
      >
        {children}
      </span>
    </div>
  );
}
