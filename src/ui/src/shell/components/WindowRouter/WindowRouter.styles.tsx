import { css } from '@linaria/core';

import { colors, fonts, space } from '../../../sdk/tokens';

export const page = css`
  box-sizing: border-box;
  min-height: 100vh;
  padding: ${space.xxl} ${space.xl};

  font-family: ${fonts.ui};
  font-size: 13px;
  line-height: 1.5;
  color: ${colors.text};

  background: ${colors.base};
`;

export const content = css`
  display: flex;
  flex-direction: column;
  gap: ${space.sm};

  max-width: 40rem;
  margin: 0 auto;
`;

export const title = css`
  margin: 0;

  font-size: 18px;
  font-weight: 600;
  color: ${colors.text};
`;

export const body = css`
  margin: 0;

  color: ${colors.subtext0};
`;
