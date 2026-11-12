import * as stylex from '@stylexjs/stylex';

import { Icon, type IconSource } from '@/components/Icon';

import type { GuideTone } from '../../AccessibilityGuide.state';

import { styles } from './HeroBadge.style';

export type HeroBadgeProps = {
  tone: GuideTone;
  icon: IconSource;
  /** The centered, larger badge (checking and open). */
  large?: boolean;
};

/** The round badge of the guide's hero, tinted by its tone; the neutral one pulses and spins. */
export function HeroBadge({ tone, icon, large = false }: HeroBadgeProps) {
  const glyph = <Icon icon={icon} size={large ? 28 : 22} strokeWidth={1.75} />;

  return (
    <span {...stylex.props(styles.root, large && styles.large, styles[tone])} data-tone={tone}>
      {tone === 'neutral' ? <span {...stylex.props(styles.spinner)}>{glyph}</span> : glyph}
    </span>
  );
}
