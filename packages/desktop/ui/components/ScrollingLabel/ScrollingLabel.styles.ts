import { css } from '@linaria/core';

import { motion } from '@/design-system';

export const wrapper = css`
  position: relative;

  overflow: hidden;

  max-width: 100%;
`;

export const scrollingWrapper = css`
  margin: 0 -4px;

  &::before,
  &::after {
    pointer-events: none;
    content: '';

    position: absolute;
    z-index: 1;
    top: 0;
    bottom: 0;

    width: 10px;

    transition: background ${motion.easing} ${motion.duration};
  }

  &::after {
    right: 0;

    background: linear-gradient(to right, transparent, var(--button-background-color, #000));
  }

  &::before {
    left: 0;

    background: linear-gradient(to left, transparent, var(--button-background-color, #000));
  }
`;

export const label = css`
  display: inline-block;

  white-space: nowrap;
`;

export const scrollingLabel = css`
  display: inline-flex;
  align-items: center;

  padding-right: 8px;
  padding-left: 8px;

  animation: scroll-text var(--scroll-duration, 5s) linear infinite alternate;

  @keyframes scroll-text {
    0%,
    5% {
      transform: translateX(0);
    }

    95%,
    100% {
      transform: translateX(var(--scroll-distance, 0px));
    }
  }
`;
