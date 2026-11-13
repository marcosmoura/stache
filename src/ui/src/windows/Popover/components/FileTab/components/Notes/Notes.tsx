import type { ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import { styles } from '../../styles/form.style';

/** A field's inline write message (an alert) and its disk conflict, if any. */
export function Notes({ message, conflict }: { message: string | null; conflict?: ReactNode }) {
  if (message === null && !conflict) {
    return null;
  }

  return (
    <>
      {message !== null ? (
        <p {...stylex.props(styles.message, styles.danger)} role="alert">
          {message}
        </p>
      ) : null}
      {conflict}
    </>
  );
}
