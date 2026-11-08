import { css } from '@linaria/core';

import { colors, fonts, radii, space } from '../../../sdk/tokens';

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
  gap: ${space.xl};

  max-width: 54rem;
  margin: 0 auto;
`;

export const header = css`
  display: flex;
  flex-wrap: wrap;
  gap: ${space.lg};
  align-items: baseline;
  justify-content: space-between;

  padding-bottom: ${space.lg};
  border-bottom: 1px solid ${colors.surface0};
`;

export const title = css`
  margin: 0;

  font-size: 20px;
  font-weight: 600;
  color: ${colors.text};
`;

export const subtitle = css`
  margin: ${space.xs} 0 0;

  font-size: 12px;
  color: ${colors.subtext0};
`;

export const identity = css`
  font-family: ${fonts.mono};
  font-size: 11px;
  color: ${colors.overlay2};
`;

export const panel = css`
  padding: ${space.lg};
  border: 1px solid ${colors.surface0};
  border-radius: ${radii.md};

  background: ${colors.mantle};
`;

export const sectionTitle = css`
  margin: 0 0 ${space.md};

  font-size: 13px;
  font-weight: 600;
  color: ${colors.subtext1};
`;

export const readiness = css`
  margin: 0;

  font-size: 14px;
  font-weight: 600;
  color: ${colors.yellow};

  &[data-ready='true'] {
    color: ${colors.green};
  }
`;

export const summary = css`
  display: flex;
  flex-direction: column;
  gap: ${space.sm};
`;

export const alert = css`
  margin: ${space.md} 0 0;
  padding: ${space.sm} ${space.md};
  border-left: 3px solid ${colors.red};
  border-radius: ${radii.sm};

  color: ${colors.text};

  background: ${colors.surface0};
`;

export const alertTitle = css`
  margin: 0 0 ${space.xs};

  font-weight: 600;
  color: ${colors.red};
`;

export const note = css`
  margin: ${space.md} 0 0;

  font-size: 12px;
  color: ${colors.subtext0};
`;

export const empty = css`
  margin: 0;

  font-size: 12px;
  color: ${colors.subtext0};
`;

export const list = css`
  display: flex;
  flex-direction: column;
  gap: ${space.md};

  margin: 0;
  padding: 0;

  list-style: none;
`;

export const card = css`
  padding: ${space.md} ${space.lg};
  border: 1px solid ${colors.surface0};
  border-radius: ${radii.md};

  background: ${colors.base};
`;

export const cardHeading = css`
  display: flex;
  gap: ${space.sm};
  align-items: center;
  justify-content: space-between;
`;

export const cardTitle = css`
  margin: 0;

  font-family: ${fonts.mono};
  font-size: 12px;
  font-weight: 600;
  color: ${colors.text};
`;

export const stateBadge = css`
  display: inline-flex;
  flex: none;
  align-items: center;

  padding: ${space.xs} ${space.sm};
  border-radius: ${radii.sm};

  font-family: ${fonts.mono};
  font-size: 11px;
  color: ${colors.subtext1};

  background: ${colors.surface0};

  &[data-state='running'] {
    color: ${colors.green};
  }

  &[data-state='failed'],
  &[data-state='invalid'] {
    color: ${colors.red};
  }

  &[data-state='waitingForPermission'],
  &[data-state='missing'] {
    color: ${colors.yellow};
  }

  &[data-state='paused'],
  &[data-state='unavailable'] {
    color: ${colors.peach};
  }
`;

export const path = css`
  margin: ${space.sm} 0 0;

  font-family: ${fonts.mono};
  font-size: 11px;
  color: ${colors.subtext0};
  overflow-wrap: anywhere;
`;

export const muted = css`
  margin: ${space.sm} 0 0;

  font-size: 12px;
  color: ${colors.subtext0};
`;

export const meta = css`
  display: flex;
  flex-direction: column;
  gap: ${space.xs};

  margin: ${space.md} 0 0;
`;

export const metaRow = css`
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: ${space.md};
`;

export const metaLabel = css`
  font-size: 11px;
  color: ${colors.subtext0};
`;

export const metaValue = css`
  margin: 0;

  font-size: 12px;
  color: ${colors.subtext1};
`;

export const diagnostic = css`
  margin: ${space.md} 0 0;
  padding: ${space.sm} ${space.md};
  border-left: 3px solid ${colors.peach};
  border-radius: ${radii.sm};

  color: ${colors.subtext1};

  background: ${colors.surface0};
`;

export const warnings = css`
  margin: ${space.md} 0 0;
  padding: ${space.sm} ${space.md};
  border-left: 3px solid ${colors.yellow};
  border-radius: ${radii.sm};

  color: ${colors.subtext1};

  background: ${colors.surface0};
`;

export const label = css`
  margin: 0 0 ${space.xs};

  font-size: 11px;
  font-weight: 600;
  color: ${colors.subtext0};
`;
