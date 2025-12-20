import { css } from '@linaria/core';

import { colors, motion } from '@/design-system';

export const spaces = css`
  position: fixed;
  top: 0;
  bottom: 0;
  left: 0;
`;

export const workspaces = css`
  display: grid;
  grid-auto-flow: column;
  align-items: center;
`;

export const workspace = css`
  padding: 0 8px;

  transition: ${motion.easing} ${motion.duration};
  /* stylelint-disable-next-line plugin/no-low-performance-animation-properties */
  transition-property: padding;
`;

export const workspaceActive = css`
  padding: 0 12px;
`;

export const app = css`
  display: grid;
  grid-auto-flow: column;
  column-gap: 6px;
  align-items: center;

  height: 100%;
  padding: 0 10px;
`;

export const appFocused = css`
  background-color: ${colors.surface1};
`;
