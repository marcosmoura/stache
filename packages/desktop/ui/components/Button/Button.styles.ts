import { css } from '@linaria/core';

import { colors, motion } from '@/design-system';

export const button = css`
  --button-background-color: ${colors.crust};

  cursor: pointer;

  display: grid;
  grid-auto-flow: column;
  column-gap: 6px;
  align-items: center;

  width: auto;
  height: 100%;
  margin: 0;
  padding: 0 10px;
  border: none;
  border: 1px solid transparent;
  border-radius: inherit;

  font: inherit;
  color: inherit;

  appearance: none;
  background: none;
  background-color: var(--button-background-color, ${colors.crust});

  transition: ${motion.easing} ${motion.duration};
  transition-property: background-color, border-color;

  &:hover {
    --button-background-color: ${colors.surface1};

    border-color: ${colors.base};

    background-color: var(--button-background-color, ${colors.surface1});
  }
`;

export const buttonActive = css`
  --button-background-color: ${colors.surface1};

  border-color: ${colors.base};

  background-color: var(--button-background-color, ${colors.surface1});
`;
