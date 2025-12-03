import { css } from '@linaria/core';

import { CSS_LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

export const label = css`
  max-width: 200px;

  ${CSS_LAPTOP_MEDIA_QUERY} {
    max-width: 150px;
  }
`;
