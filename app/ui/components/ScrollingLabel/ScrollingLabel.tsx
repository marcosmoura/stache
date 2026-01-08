import { useEffect, useMemo, useRef, useState } from 'react';

import { cx } from '@linaria/core';

import * as styles from './ScrollingLabel.styles';
import type { ScrollingLabelProps } from './ScrollingLabel.types';

export const ScrollingLabel = ({
  children,
  className,
  scrollSpeed = 60,
  ...props
}: ScrollingLabelProps) => {
  const wrapperRef = useRef<HTMLDivElement>(null);
  const labelRef = useRef<HTMLSpanElement>(null);
  const [scrollDistance, setScrollDistance] = useState(0);

  useEffect(() => {
    const wrapper = wrapperRef.current;
    const label = labelRef.current;

    if (!wrapper || !label) {
      return;
    }

    const calculateScrollDistance = () => {
      const wrapperWidth = wrapper.offsetWidth;
      const labelWidth = label.scrollWidth;
      const overflow = labelWidth - wrapperWidth;

      setScrollDistance(overflow > 0 ? -overflow : 0);
    };

    calculateScrollDistance();

    const resizeObserver = new ResizeObserver(calculateScrollDistance);
    resizeObserver.observe(wrapper);
    resizeObserver.observe(label);

    return () => resizeObserver.disconnect();
  }, [children]);

  const isScrolling = scrollDistance < 0;
  const scrollStyles = useMemo(() => {
    // Calculate duration: base 1s + scrollSpeed px per second for readable scrolling
    const scrollDuration = Math.max(1, 1 + Math.abs(scrollDistance) / scrollSpeed);

    return {
      '--scroll-distance': `${scrollDistance}px`,
      '--scroll-duration': `${scrollDuration}s`,
    };
  }, [scrollDistance, scrollSpeed]);

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
};
