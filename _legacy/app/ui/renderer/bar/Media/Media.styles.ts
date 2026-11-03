import { css } from '@linaria/core';

import { CSS_LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

export const media = css`
  position: fixed;
  top: 0;
  bottom: 0;
  left: 50%;
  transform: translateX(-50%);

  display: grid;
  grid-auto-flow: column;

  height: 100%;
  padding-left: 1px;
`;

export const label = css`
  max-width: 560px;

  ${CSS_LAPTOP_MEDIA_QUERY} {
    max-width: 400px;
  }
`;

export const artwork = css`
  overflow: hidden;

  width: 24px;
  height: 24px;
  border-radius: 10px;

  object-fit: cover;
`;
