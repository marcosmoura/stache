import { useEffect, useLayoutEffect, useRef, useState } from 'react';

import * as stylex from '@stylexjs/stylex';
import type { StyleXStyles } from '@stylexjs/stylex';
import {
  animate,
  useMotionValue,
  useReducedMotion,
  type AnimationPlaybackControls,
} from 'motion/react';
import * as m from 'motion/react-m';

import { scrolling } from '@/theme/motion';

import { styles } from './ScrollingLabel.style';

/** The overrides a `ScrollingLabel` accepts: its width limits and text color. */
export type ScrollingLabelStyle = StyleXStyles<{
  color?: string;
  width?: number | string;
  minWidth?: number | string;
  maxWidth?: number | string;
}>;

export type ScrollingLabelProps = {
  /** The label; plain text, so the follow-up copy can repeat it. */
  children: string;
  /** Overrides, merged last; set the width limit here. */
  style?: ScrollingLabelStyle;
};

type Widths = { box: number; text: number };

/**
 * The box's and the text's layout widths (the translation does not change
 * them), both fractional, so a box sized to its text compares equal.
 */
const widthsOf = (box: HTMLElement, text: HTMLElement): Widths => ({
  box: box.getBoundingClientRect().width,
  text: text.getBoundingClientRect().width,
});

/** Sub-pixel differences are rounding, not overflow. */
const OVERFLOW_TOLERANCE = 0.5;

/**
 * Text in a width-limited box. When it fits it is static. When it overflows it
 * runs as a continuous marquee: the text enters from just past the right edge
 * and moves left at `scrolling.speed`; a second copy follows it
 * `scrolling.gap` of the box width behind, and the track loops seamlessly by
 * one text width plus that gap. Both edges fade while it scrolls. It
 * re-measures when the text or the box width changes. Under reduced motion it
 * never scrolls: the text is truncated with an ellipsis.
 */
export function ScrollingLabel({ children, style }: ScrollingLabelProps) {
  const box = useRef<HTMLSpanElement>(null);
  const text = useRef<HTMLSpanElement>(null);
  const [widths, setWidths] = useState<Widths>({ box: 0, text: 0 });
  const reduced = useReducedMotion() ?? false;
  const x = useMotionValue(0);

  useLayoutEffect(() => {
    const boxElement = box.current;
    const textElement = text.current;

    if (!boxElement || !textElement) {
      return;
    }

    const measure = () => {
      const next = widthsOf(boxElement, textElement);

      setWidths((current) =>
        current.box === next.box && current.text === next.text ? current : next,
      );
    };

    measure();

    const observer = new ResizeObserver(measure);

    observer.observe(boxElement);
    observer.observe(textElement);

    return () => observer.disconnect();
  }, [children, reduced]);

  const overflows = widths.text - widths.box > OVERFLOW_TOLERANCE;
  const state = overflows ? (reduced ? 'truncated' : 'scrolling') : 'static';
  const scrollingNow = state === 'scrolling';
  const gap = widths.box * scrolling.gap;

  useEffect(() => {
    if (!scrollingNow) {
      x.set(0);

      return;
    }

    // Intro: enter from the right edge to 0, then loop by one text + gap.
    const period = widths.text + gap;
    let current: AnimationPlaybackControls | undefined;
    let cancelled = false;

    x.set(widths.box);
    current = animate(x, 0, { duration: widths.box / scrolling.speed, ease: 'linear' });
    void current.finished.then(() => {
      if (cancelled) {
        return;
      }

      current = animate(x, [0, -period], {
        duration: period / scrolling.speed,
        ease: 'linear',
        repeat: Infinity,
        repeatType: 'loop',
      });
    });

    return () => {
      cancelled = true;
      current?.stop();
      x.set(0);
    };
  }, [scrollingNow, widths.box, widths.text, gap, x]);

  // The Motion value joins StyleX's inline style on the one moving element.
  const track = stylex.props(reduced ? styles.inline : styles.track);

  return (
    <span
      ref={box}
      data-state={state}
      {...stylex.props(
        styles.box,
        scrollingNow && styles.scrolling,
        reduced && styles.truncated,
        style,
      )}
    >
      <m.span data-track="" className={track.className} style={{ ...track.style, x }}>
        <span ref={text} {...stylex.props(reduced ? styles.inline : styles.text)}>
          {children}
        </span>
        {scrollingNow && (
          <span
            aria-hidden="true"
            data-clone=""
            data-text={children}
            {...stylex.props(styles.text, styles.clone(gap))}
          />
        )}
      </m.span>
    </span>
  );
}
