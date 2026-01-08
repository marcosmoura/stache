import { css } from '@linaria/core';

import { colors } from '@/design-system';

import {
  DAY_COLUMN_GAP,
  DAY_HEIGHT,
  DAY_ROW_GAP,
  SECTION_GAP,
  WEEKDAY_HEIGHT,
} from './Calendar.constants';

export const calendar = css`
  display: flex;
  flex-direction: column;
  gap: 8px;

  width: 360px;
  padding: 24px 16px;
`;

export const header = css`
  display: flex;
  align-items: center;
  justify-content: space-between;
`;

export const monthYearContainer = css`
  position: relative;

  overflow: hidden;
  display: grid;

  height: 32px;
  border-radius: 8px;
`;

export const monthYear = css`
  display: grid;
  grid-area: 1 / 1;
  place-content: center;

  font-size: 16px;
  font-weight: 600;
  color: ${colors.text};
  white-space: nowrap;
`;

export const navButton = css`
  cursor: pointer;

  display: grid;
  place-content: center;

  width: 28px;
  height: 28px;
  padding: 0;
  border: none;
  border-radius: 6px;

  font: inherit;
  color: ${colors.subtext1};

  background-color: transparent;

  &:hover {
    background-color: ${colors.surface0};
  }

  &:active {
    background-color: ${colors.surface1};
  }
`;

export const monthContainer = css`
  position: relative;

  overflow: hidden;
`;

export const month = css`
  position: absolute;
  inset: 0;

  display: flex;
  flex-direction: column;
  row-gap: ${SECTION_GAP}px;

  padding: 0 6px;
`;

export const weekdays = css`
  display: grid;
  grid-template-columns: repeat(7, 1fr);
  column-gap: ${DAY_COLUMN_GAP}px;
`;

export const weekday = css`
  display: grid;
  place-content: center;

  min-height: ${WEEKDAY_HEIGHT}px;

  font-size: 11px;
  font-weight: 700;
  color: ${colors.overlay1};
  text-transform: uppercase;
  letter-spacing: 0.03em;
`;

export const days = css`
  display: grid;
  grid-template-columns: repeat(7, 1fr);
  row-gap: ${DAY_ROW_GAP}px;
  column-gap: ${DAY_COLUMN_GAP}px;
`;

export const day = css`
  cursor: pointer;

  display: grid;
  place-content: center;

  min-height: ${DAY_HEIGHT}px;
  border: none;
  border-radius: 8px;

  color: ${colors.text};
`;

export const dayOutsideMonth = css`
  color: ${colors.surface2};
`;

export const dayToday = css`
  font-weight: 900;
  color: ${colors.crust};

  background-color: ${colors.blue};
`;
