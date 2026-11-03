import { css } from '@linaria/core';

import { colors } from '@/design-system';

export const surface = css`
  overflow: hidden;

  height: 100%;
  border-radius: 12px;

  color: ${colors.text};

  background: ${colors.crust};
`;
