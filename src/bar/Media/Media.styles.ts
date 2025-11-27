import { css } from '@linaria/core';

import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

const laptopMediaQuery = `@media ${LAPTOP_MEDIA_QUERY}`;

export const media = css`
  position: fixed;
  top: 0;
  bottom: 0;
  left: 50%;
  transform: translateX(-50%);

  display: grid;
  grid-auto-flow: column;
  row-gap: 4px;

  height: 100%;
  padding-left: 1px;
`;

export const labelWrapper = css`
  position: relative;

  overflow: hidden;

  max-width: 480px;

  ${laptopMediaQuery} {
    max-width: 300px;
  }
`;

export const label = css`
  display: inline-block;

  white-space: nowrap;
`;

export const scrollingLabel = css`
  animation: scroll-text var(--scroll-duration, 5s) linear infinite alternate;

  @keyframes scroll-text {
    0%,
    20% {
      transform: translateX(0);
    }

    80%,
    100% {
      transform: translateX(var(--scroll-distance, 0px));
    }
  }
`;

export const artwork = css`
  overflow: hidden;

  width: 24px;
  height: 24px;
  border-radius: 10px;

  object-fit: cover;
`;
