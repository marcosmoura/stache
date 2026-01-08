import { css } from '@linaria/core';

import { colors, motion } from '@/design-system';

export const button = css`
  --button-background-color: ${colors.crust};
  --button-border-color: transparent;

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
  border: 1px solid var(--button-border-color);
  border-radius: 12px;

  font: inherit;
  color: inherit;

  appearance: none;
  background: none;
  background-color: var(--button-background-color);

  transition: ${motion.easing} ${motion.duration};
  transition-property: background-color, border-color;

  &:hover {
    --button-background-color: ${colors.surface1};
    --button-border-color: ${colors.base};

    border-color: ${colors.base};

    background-color: var(--button-background-color, ${colors.surface1});
  }
`;

export const buttonActive = css`
  --button-background-color: ${colors.surface1};
  --button-border-color: ${colors.base};

  border-color: ${colors.base};

  background-color: var(--button-background-color, ${colors.surface1});
`;
