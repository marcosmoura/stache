import type { ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import { styles } from './Marker.style';

/** A small tertiary note after a label: "default" or a count. */
export function Marker({ children }: { children: ReactNode }) {
  return <span {...stylex.props(styles.marker)}>{children}</span>;
}
