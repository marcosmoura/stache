import { css } from '@linaria/core';

import { colors } from '@/design-system';

export const battery = css`
  display: flex;
  flex-direction: column;
  gap: 16px;

  width: 320px;
  padding: 20px;
`;

export const header = css`
  display: flex;
  gap: 12px;
  align-items: center;
`;

export const headerIcon = css`
  display: grid;
  place-content: center;

  width: 40px;
  height: 40px;
  border-radius: 10px;

  background-color: ${colors.surface0};
`;

export const headerContent = css`
  display: flex;
  flex-direction: column;
  gap: 2px;
`;

export const headerTitle = css`
  font-size: 14px;
  font-weight: 600;
  color: ${colors.text};
`;

export const headerSubtitle = css`
  font-size: 12px;
  color: ${colors.subtext0};
`;

export const progressContainer = css`
  display: flex;
  flex-direction: column;
  gap: 8px;
`;

export const progressBar = css`
  position: relative;

  overflow: hidden;

  height: 8px;
  border-radius: 4px;

  background-color: ${colors.surface0};
`;

export const progressFill = css`
  position: absolute;
  top: 0;
  left: 0;
  transform-origin: left;

  width: 100%;
  height: 100%;
  border-radius: 4px;
`;

export const progressLabel = css`
  display: flex;
  align-items: center;
  justify-content: space-between;

  font-size: 11px;
  color: ${colors.subtext0};
`;

export const stats = css`
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
`;

export const stat = css`
  display: flex;
  flex-direction: column;
  gap: 4px;

  padding: 12px;
  border-radius: 8px;

  background-color: ${colors.surface0};
`;

export const statLabel = css`
  font-size: 11px;
  font-weight: 500;
  color: ${colors.subtext0};
  text-transform: uppercase;
  letter-spacing: 0.03em;
`;

export const statValue = css`
  font-size: 16px;
  font-weight: 600;
  color: ${colors.text};
`;

export const timeRemaining = css`
  display: flex;
  gap: 8px;
  align-items: center;

  padding: 12px;
  border-radius: 8px;

  font-size: 13px;
  color: ${colors.subtext1};

  background-color: ${colors.surface0};
`;

export const timeIcon = css`
  flex-shrink: 0;
`;
