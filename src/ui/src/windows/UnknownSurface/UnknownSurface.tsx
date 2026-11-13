import * as stylex from '@stylexjs/stylex';

import type { FallbackProps } from '@/router';

import { styles } from './UnknownSurface.style';

/** Local fallback for an empty or unknown `?surface=` value. Calls no native command. */
export function UnknownSurface({ requested }: FallbackProps) {
  return (
    <main {...stylex.props(styles.root)} data-surface="unknown">
      <h1 {...stylex.props(styles.title)}>Unknown surface</h1>
      <p {...stylex.props(styles.detail)}>
        {requested === '' ? 'No surface was requested.' : `“${requested}” is not a surface.`}
      </p>
    </main>
  );
}
