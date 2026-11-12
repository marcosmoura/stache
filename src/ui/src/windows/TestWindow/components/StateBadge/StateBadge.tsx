import * as stylex from '@stylexjs/stylex';

import { badgeTone, type BadgeState } from '../../TestWindow.labels';

import { styles } from './StateBadge.style';

export type StateBadgeProps = { state: BadgeState; children: string };

/** A module or file state, tinted by its tone; `data-state` carries the raw state. */
export function StateBadge({ state, children }: StateBadgeProps) {
  return (
    <span {...stylex.props(styles.root, styles[badgeTone(state)])} data-state={state}>
      {children}
    </span>
  );
}
