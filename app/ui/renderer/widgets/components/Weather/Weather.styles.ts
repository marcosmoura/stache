import { css } from '@linaria/core';

import { colors } from '@/design-system';

export const weather = css`
  display: flex;
  flex-direction: column;
  gap: 20px;

  width: 520px;
  padding: 24px;
`;

export const header = css`
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
`;

export const headerContent = css`
  display: flex;
  flex-direction: column;
  gap: 4px;
`;

export const headerTitle = css`
  font-size: 22px;
  font-weight: 600;
  color: ${colors.text};
`;

export const headerSubtitle = css`
  display: flex;
  gap: 6px;
  align-items: center;

  font-size: 13px;
  color: ${colors.subtext0};
`;

export const settingsButton = css`
  cursor: pointer;

  display: grid;
  place-content: center;

  width: 36px;
  height: 36px;
  border: none;
  border-radius: 10px;

  color: ${colors.subtext0};

  background-color: ${colors.surface0};

  transition:
    color 0.15s ease,
    background-color 0.15s ease;

  &:hover {
    color: ${colors.text};

    background-color: ${colors.surface1};
  }
`;

/* Main display section */
export const mainDisplay = css`
  display: flex;
  gap: 20px;
  align-items: center;
`;

export const temperatureDisplay = css`
  display: flex;
  flex-direction: column;
  flex-shrink: 0;
  align-items: center;
  justify-content: center;

  min-width: 100px;
`;

export const temperatureValue = css`
  font-size: 48px;
  font-weight: 700;
  line-height: 1.15;
  color: ${colors.text};
`;

export const temperatureFeelsLike = css`
  font-size: 13px;
  color: ${colors.subtext0};
`;

export const circularProgress = css`
  position: absolute;
  inset: 0;
  transform: rotate(-90deg);
`;

export const circularBackground = css`
  fill: none;
  stroke: ${colors.surface1};
  stroke-width: 6;
`;

export const circularFill = css`
  fill: none;
  stroke-linecap: round;
  stroke-width: 6;

  transition: stroke-dashoffset 0.5s ease;
`;

export const mainInfo = css`
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 8px;
`;

export const statusLabel = css`
  font-size: 20px;
  font-weight: 600;
  color: ${colors.text};
`;

export const statusDescription = css`
  font-size: 13px;
  line-height: 1.5;
  color: ${colors.subtext0};
`;

/* Chart section */
export const chartSection = css`
  display: flex;
  flex-direction: column;
  gap: 12px;
`;

export const chartContainer = css`
  position: relative;

  height: 80px;
  padding: 8px 0;
`;

export const chartSvg = css`
  overflow: visible;

  width: 100%;
  height: 100%;
`;

export const chartLine = css`
  fill: none;
  stroke-linecap: round;
  stroke-linejoin: round;
  stroke-width: 2;
`;

export const chartGradient = css`
  opacity: 0.2;
`;

export const chartPoint = css`
  filter: drop-shadow(0 0 4px currentColor);
`;

export const chartTimeLabels = css`
  display: flex;
  justify-content: space-between;

  padding: 0 4px;

  font-size: 11px;
  color: ${colors.overlay1};
`;

/* Toggle section */
export const toggleSection = css`
  cursor: pointer;

  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: flex-end;

  font-size: 13px;
  color: ${colors.subtext0};

  transition: color 0.15s ease;

  &:hover {
    color: ${colors.text};
  }
`;

export const toggleIcon = css`
  transition: transform 0.2s ease;

  &[data-expanded='true'] {
    transform: rotate(180deg);
  }
`;

/* Stats grid */
export const statsGrid = css`
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
`;

export const statCard = css`
  display: flex;
  gap: 12px;
  align-items: center;

  padding: 14px;
  border-radius: 12px;

  background-color: ${colors.surface0};
`;

export const statIndicator = css`
  position: relative;

  display: grid;
  flex-shrink: 0;
  place-content: center;

  width: 48px;
  height: 48px;
`;

export const statCircleBackground = css`
  fill: none;
  stroke: ${colors.surface1};
  stroke-width: 4;
`;

export const statCircleFill = css`
  fill: none;
  stroke-linecap: round;
  stroke-width: 4;

  transition: stroke-dashoffset 0.5s ease;
`;

export const statValueInCircle = css`
  font-size: 14px;
  font-weight: 700;
  color: ${colors.text};
`;

export const statInfo = css`
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 2px;
`;

export const statLabel = css`
  font-size: 11px;
  color: ${colors.overlay1};
`;

export const statStatus = css`
  display: flex;
  gap: 6px;
  align-items: center;

  font-size: 14px;
  font-weight: 600;
`;

export const statusDot = css`
  width: 6px;
  height: 6px;
  border-radius: 50%;
`;

export const statDetail = css`
  font-size: 11px;
  color: ${colors.overlay1};
`;

/* Footer */
export const footer = css`
  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: center;

  padding-top: 4px;

  font-size: 11px;
  color: ${colors.overlay1};
`;

/* Rain Forecast Section */
export const rainForecastSection = css`
  display: flex;
  flex-direction: column;
  gap: 12px;

  padding: 16px;
  border-radius: 12px;

  background-color: ${colors.surface0};
`;

export const rainForecastHeader = css`
  display: flex;
  gap: 8px;
  align-items: center;

  font-size: 14px;
  font-weight: 600;
  color: ${colors.text};
`;

export const rainForecastHeaderIcon = css`
  color: ${colors.sapphire};
`;

export const rainForecastChart = css`
  display: flex;
  gap: 4px;
  align-items: flex-end;

  height: 80px;
`;

export const rainForecastBar = css`
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 4px;
  align-items: center;
`;

export const rainBar = css`
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: flex-end;

  width: 100%;
  height: 60px;
  border-radius: 4px 4px 0 0;

  background-color: ${colors.surface1};
`;

export const rainBarFill = css`
  width: 100%;
  border-radius: 4px 4px 0 0;
`;

export const rainBarLabel = css`
  font-size: 9px;
  color: ${colors.overlay1};
`;

export const rainBarValue = css`
  font-size: 10px;
  font-weight: 600;
  color: ${colors.text};
`;

export const rainForecastLegend = css`
  display: flex;
  gap: 16px;
  align-items: center;
  justify-content: center;

  padding-top: 8px;
  border-top: 1px solid ${colors.surface1};

  font-size: 11px;
  color: ${colors.subtext0};
`;

export const legendItem = css`
  display: flex;
  gap: 4px;
  align-items: center;
`;

export const legendDot = css`
  width: 8px;
  height: 8px;
  border-radius: 2px;
`;

export const noRainMessage = css`
  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: center;

  padding: 12px;

  font-size: 13px;
  color: ${colors.subtext0};
`;

export const noRainIcon = css`
  color: ${colors.green};
`;

/* Next Precipitation Card */
export const nextPrecipCard = css`
  display: flex;
  gap: 12px;
  align-items: center;

  padding: 8px 0;
`;

export const nextPrecipIcon = css`
  display: grid;
  flex-shrink: 0;
  place-content: center;

  width: 40px;
  height: 40px;
  border-radius: 10px;
`;

export const nextPrecipInfo = css`
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 2px;
`;

export const nextPrecipTitle = css`
  display: flex;
  gap: 6px;
  align-items: center;

  font-size: 13px;
  font-weight: 600;
  color: ${colors.text};
`;

export const nextPrecipDetails = css`
  font-size: 12px;
  color: ${colors.subtext0};
`;

export const nextPrecipProb = css`
  font-size: 16px;
  font-weight: 700;
  color: ${colors.text};
`;

export const clearSkiesCard = css`
  display: flex;
  gap: 12px;
  align-items: center;

  padding: 8px 0;
`;

export const clearSkiesIcon = css`
  display: grid;
  flex-shrink: 0;
  place-content: center;

  width: 40px;
  height: 40px;
  border-radius: 10px;

  color: ${colors.yellow};

  background-color: rgba(249, 226, 175, 0.15);
`;

export const clearSkiesInfo = css`
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 2px;
`;

export const clearSkiesTitle = css`
  font-size: 13px;
  font-weight: 600;
  color: ${colors.text};
`;

export const clearSkiesDetails = css`
  font-size: 12px;
  color: ${colors.subtext0};
`;
