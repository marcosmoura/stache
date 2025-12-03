import { css } from '@linaria/core';

import { motion } from '@/design-system';

export const bar = css`
  position: fixed;
  inset: 0;

  overflow: hidden;

  width: 100%;
  height: 100%;

  transition: ${motion.easing} ${motion.duration};
  transition-property: transform, opacity;
  will-change: transform, opacity;
`;

export const barHidden = css`
  transform: translateY(100%) translateZ(0);

  opacity: 0;

  transition-duration: 0ms;
`;
