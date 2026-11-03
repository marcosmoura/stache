import { css } from '@linaria/core';

import { colors } from '@/design-system';

export const widgets = css`
  user-select: none;

  overflow: hidden;

  width: fit-content;
  border-radius: 12px;
`;

export const widget = css`
  position: relative;

  display: flex;
  flex-direction: column;

  border-radius: 16px;

  &::after {
    pointer-events: none;
    content: '';

    position: absolute;
    z-index: 2;
    inset: 0;

    border-radius: 16px;

    box-shadow: inset 0 0 0 1px ${colors.surface0};
  }
`;
