import { css } from '@linaria/core';

import { colors } from '@/design-system';

export const fallback = css`
  display: grid;
  grid-auto-flow: column;
  column-gap: 8px;
  align-items: center;

  height: 100%;
  padding: 0 10px;
`;

export const message = css`
  font-size: 12px;
  color: ${colors.overlay1};
  white-space: nowrap;
`;

export const retry = css`
  height: 24px;
  padding: 0 8px;
  border-radius: 8px;
`;
