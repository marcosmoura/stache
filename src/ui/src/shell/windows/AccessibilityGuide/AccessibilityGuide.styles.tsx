import { css } from '@linaria/core';

import { colors, fonts, motion, radii, space } from '../../../sdk/tokens';

/** Every animation and transition is off when the user asks for reduced motion. */
const still = '@media (prefers-reduced-motion: reduce) { animation: none; transition: none; }';

/** The window is transparent, so only the access surface clears the document. */
export const surface = css`
  /* stylelint-disable-next-line selector-pseudo-class-no-unknown */
  :global() {
    html[data-surface='access'],
    html[data-surface='access'] body,
    html[data-surface='access'] #root {
      height: 100%;

      background: transparent;
    }
  }
`;

export const panel = css`
  --ring: inset 0 0 0 1px ${colors.surface0};

  user-select: none;

  position: relative;

  display: grid;
  grid-template-rows: auto minmax(0, 1fr) auto auto;

  box-sizing: border-box;
  width: 100%;
  height: 100vh;
  border-radius: ${radii.panel};

  font-family: ${fonts.bar};
  font-size: 13px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
  line-height: 1.6em;
  color: ${colors.text};

  background: ${colors.crust};

  animation: guide-panel-in ${motion.durationSlow} ${motion.easing} both;

  /* The ring sits above the content so scrolled text never covers it. */
  &::after {
    pointer-events: none;
    content: '';

    position: absolute;
    z-index: 2;
    inset: 0;

    border-radius: ${radii.panel};

    box-shadow: var(--ring);
  }

  @keyframes guide-panel-in {
    from {
      transform: scale(0.97);

      opacity: 0;
    }

    to {
      transform: scale(1);

      opacity: 1;
    }
  }

  ${still}
`;

export const header = css`
  display: flex;
  gap: ${space.sm};
  align-items: center;

  height: 48px;
  padding: 0 ${space.md} 0 ${space.xl};
`;

export const dragRegion = css`
  display: flex;
  flex: 1;
  gap: ${space.sm};
  align-items: center;

  height: 100%;

  /* Children must not swallow the click: the drag script reads the clicked element. */
  > * {
    pointer-events: none;
  }
`;

export const glyph = css`
  display: grid;
  place-items: center;

  width: 22px;
  height: 22px;
  border-radius: ${radii.md};

  color: ${colors.mauve};

  background: color-mix(in srgb, ${colors.mauve} 16%, ${colors.crust});
`;

export const appName = css`
  font-size: 12px;
  color: ${colors.subtext0};
`;

export const close = css`
  cursor: pointer;

  display: grid;
  place-items: center;

  width: 28px;
  height: 28px;
  padding: 0;
  border: 1px solid transparent;
  border-radius: ${radii.control};

  color: ${colors.subtext0};

  background: transparent;

  transition: ${motion.easing} ${motion.duration};
  transition-property: background-color, border-color, color;

  &:hover {
    border-color: ${colors.base};

    color: ${colors.text};

    background-color: ${colors.surface1};
  }

  &:focus-visible {
    outline: 2px solid ${colors.lavender};
    outline-offset: 1px;
  }

  ${still}
`;

export const scroll = css`
  scrollbar-color: ${colors.surface1} transparent;
  scrollbar-width: thin;

  overflow: hidden auto;
  overscroll-behavior: contain;
  display: flex;
  flex-direction: column;

  min-height: 0;
  padding: ${space.xs} ${space.xl} ${space.lg};
`;

export const fade = css`
  display: flex;
  flex-direction: column;
  gap: 14px;

  margin-block: 0;

  animation: guide-fade ${motion.duration} ${motion.easing} both;

  /* Short states center their hero; margin auto keeps overflow scrollable. */
  [data-layout='centered'] > & {
    margin-block: auto;
  }

  @keyframes guide-fade {
    from {
      opacity: 0;
    }

    to {
      opacity: 1;
    }
  }

  ${still}
`;

export const hero = css`
  display: flex;
  gap: ${space.md};
  align-items: flex-start;

  text-align: left;

  &[data-centered='true'] {
    flex-direction: column;
    gap: ${space.md};
    align-items: center;

    text-align: center;
  }
`;

export const heroText = css`
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: ${space.xs};

  min-width: 0;
`;

export const badge = css`
  --tone: ${colors.overlay1};

  display: grid;
  flex: none;
  place-items: center;

  width: 44px;
  height: 44px;
  border-radius: 50%;

  color: var(--tone);

  background: color-mix(in srgb, var(--tone) 14%, ${colors.crust});
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--tone) 28%, transparent);

  [data-centered='true'] > & {
    width: 64px;
    height: 64px;
  }

  &[data-tone='mauve'] {
    --tone: ${colors.mauve};
  }

  &[data-tone='peach'] {
    --tone: ${colors.peach};
  }

  &[data-tone='green'] {
    --tone: ${colors.green};
  }

  &[data-tone='overlay'] {
    animation: guide-pulse 1600ms ${motion.easing} infinite;

    svg {
      animation: guide-spin 1200ms linear infinite;
    }
  }

  @keyframes guide-pulse {
    0%,
    100% {
      opacity: 1;
    }

    50% {
      opacity: 0.55;
    }
  }

  @keyframes guide-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    &[data-tone='overlay'],
    &[data-tone='overlay'] svg {
      animation: none;
    }
  }
`;

export const title = css`
  margin: 0;

  font-size: 17px;
  font-weight: 600;
  line-height: 1.3;
  color: ${colors.text};
`;

export const body = css`
  margin: 0;

  color: ${colors.subtext0};
`;

export const steps = css`
  counter-reset: step;

  display: flex;
  flex-direction: column;
  gap: ${space.sm};

  margin: 0;
  padding: 0;

  list-style: none;

  li {
    counter-increment: step;

    display: flex;
    gap: ${space.md};
    align-items: flex-start;

    font-size: 12.5px;
    line-height: 22px;
    color: ${colors.subtext1};
  }

  li::before {
    content: counter(step);

    display: grid;
    flex: none;
    place-items: center;

    width: 22px;
    height: 22px;
    border-radius: 50%;

    font-size: 12px;
    color: ${colors.mauve};

    background: ${colors.surface0};
  }

  strong {
    font-weight: 600;
    color: ${colors.text};
  }
`;

export const identity = css`
  display: flex;
  flex-direction: column;
  gap: ${space.sm};

  padding: ${space.sm} ${space.md};
  border-radius: ${radii.control};

  background: ${colors.mantle};
  box-shadow: var(--ring);
`;

export const fields = css`
  user-select: text;

  display: flex;
  flex-direction: column;
  gap: ${space.xs};

  margin: 0;
`;

export const field = css`
  display: grid;
  grid-template-columns: 68px minmax(0, 1fr);
  gap: ${space.md};

  font-size: 12px;

  dt {
    color: ${colors.overlay1};
  }

  dd {
    margin: 0;

    color: ${colors.text};
    overflow-wrap: anywhere;
  }
`;

export const hint = css`
  display: flex;
  gap: ${space.sm};
  align-items: flex-start;

  margin: 0;
  padding-top: ${space.sm};
  border-top: 1px solid ${colors.surface0};

  font-size: 12px;
  color: ${colors.yellow};

  svg {
    flex: none;

    margin-top: 3px;
  }
`;

export const chevron = css`
  display: grid;
  place-items: center;

  transition: transform ${motion.duration} ${motion.easing};

  ${still}
`;

export const troubleshooting = css`
  font-size: 12px;
  color: ${colors.subtext0};

  summary {
    cursor: pointer;

    display: flex;
    gap: ${space.xs};
    align-items: center;

    width: fit-content;
    border-radius: ${radii.md};

    color: ${colors.subtext1};
    list-style: none;

    &::-webkit-details-marker {
      display: none;
    }

    &:focus-visible {
      outline: 2px solid ${colors.lavender};
      outline-offset: 2px;
    }
  }

  &[open] summary > span:first-child {
    transform: rotate(90deg);
  }

  p {
    user-select: text;

    margin: ${space.sm} 0 0;
    padding-left: 18px;
  }
`;

export const alert = css`
  display: flex;
  gap: ${space.sm};
  align-items: flex-start;

  margin: ${space.sm} ${space.xl};
  padding: ${space.sm} ${space.md};
  border-radius: ${radii.control};

  font-size: 12px;
  color: ${colors.red};
  overflow-wrap: anywhere;

  background: color-mix(in srgb, ${colors.red} 12%, ${colors.crust});
  box-shadow: inset 0 0 0 1px color-mix(in srgb, ${colors.red} 24%, transparent);

  svg {
    flex: none;

    margin-top: 2px;
  }
`;

export const actions = css`
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: ${space.sm};

  padding: ${space.md} ${space.xl} ${space.lg};
  border-top: 1px solid ${colors.surface0};

  background: ${colors.mantle};

  > :first-child {
    grid-column: 1 / -1;
  }
`;

export const spinner = css`
  display: grid;
  place-items: center;

  animation: guide-spin 900ms linear infinite;

  @keyframes guide-spin {
    to {
      transform: rotate(360deg);
    }
  }

  ${still}
`;

export const button = css`
  --button-bg: ${colors.surface0};
  --button-border: transparent;

  cursor: pointer;

  display: flex;
  gap: ${space.sm};
  align-items: center;
  justify-content: center;

  height: 36px;
  padding: 0 ${space.lg};
  border: 1px solid var(--button-border);
  border-radius: ${radii.control};

  font: inherit;
  color: ${colors.text};
  white-space: nowrap;

  appearance: none;
  background-color: var(--button-bg);

  transition: ${motion.easing} ${motion.duration};
  transition-property: background-color, border-color;

  &:focus-visible {
    outline: 2px solid ${colors.lavender};
    outline-offset: 2px;
  }

  &:disabled {
    cursor: default;

    color: ${colors.subtext0};
  }

  &[data-primary='true'] {
    --button-bg: ${colors.mauve};

    color: ${colors.crust};
  }

  &:hover:not(:disabled) {
    --button-bg: ${colors.surface1};
    --button-border: ${colors.base};
  }

  &[data-primary='true']:disabled {
    color: ${colors.crust};

    opacity: 0.7;
  }

  &[data-primary='true']:hover:not(:disabled) {
    --button-bg: color-mix(in srgb, ${colors.mauve} 82%, ${colors.text});
  }

  ${still}
`;
