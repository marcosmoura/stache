import { css } from '@linaria/core';

import { colors, motion } from '@/design-system';

export const app = css`
  display: grid;
  grid-auto-flow: column;
  column-gap: 6px;
  align-items: center;

  height: 100%;
  padding: 0 10px;

  opacity: 0.68;

  transition: ${motion.duration} ${motion.easing};
  transition-property: background-color, opacity;
`;

export const appFocused = css`
  opacity: 1;
  background-color: ${colors.crust};
`;

export const appLabel = css`
  overflow: hidden;
  display: inline-block;

  white-space: nowrap;
`;
