import { css } from '@linaria/core';

export const stack = css`
  display: grid;
  grid-auto-flow: column;
  column-gap: 4px;

  height: 100%;
`;

export const stackCompact = css`
  column-gap: 2px;
`;
