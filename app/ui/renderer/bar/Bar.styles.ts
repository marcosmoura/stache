import { css } from '@linaria/core';

import { motion } from '@/design-system';

export const bar = css`
  -webkit-user-select: none;

  overflow: hidden;
  display: grid;
  grid-auto-flow: column;
  align-items: center;
  justify-content: space-between;
  justify-items: center;

  width: 100%;
  height: 100%;

  transition: ${motion.easing} ${motion.duration};
  transition-property: transform, opacity;
  will-change: transform, opacity;
  -webkit-user-drag: none;
`;

export const barHidden = css`
  transform: translateY(100%) translateZ(0);

  opacity: 0;

  transition-duration: 0ms;
`;
