import * as stylex from '@stylexjs/stylex';

import type { WindowProps } from '@/router';

import { styles } from './Placeholder.style';

/** The `shell` surface: a development fallback that names the surface and calls no command. */
export function Placeholder({ surface }: WindowProps) {
  return (
    <main {...stylex.props(styles.root)} data-surface={surface}>
      {surface}
    </main>
  );
}
