import { css } from '@linaria/core';

import { colors } from '@/design-system';

export const workspace = css`
  position: relative;
  z-index: 1;

  padding: 0 10px;

  background-color: transparent;
`;

export const workspaceIndicator = css`
  position: absolute;
  z-index: -1;
  inset: 0;

  border-radius: 12px;

  background-color: ${colors.surface1};
`;
