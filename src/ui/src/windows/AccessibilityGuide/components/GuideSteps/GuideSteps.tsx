import type { ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import { styles } from './GuideSteps.style';

function Strong({ children }: { children: ReactNode }) {
  return <strong {...stylex.props(styles.strong)}>{children}</strong>;
}

const STEPS: ReactNode[] = [
  <>
    Click <Strong>Request Access</Strong>, or <Strong>Open System Settings</Strong>.
  </>,
  <>
    In Privacy &amp; Security &gt; Accessibility, turn on <Strong>Stache Next</Strong>.
  </>,
  <>Come back here; Stache Next starts by itself.</>,
];

/** The three numbered steps to grant Accessibility. */
export function GuideSteps() {
  return (
    <ol {...stylex.props(styles.list)}>
      {STEPS.map((step, index) => (
        // The steps are a fixed list.
        // oxlint-disable-next-line react/no-array-index-key
        <li key={index} {...stylex.props(styles.step)}>
          <span {...stylex.props(styles.number)} aria-hidden="true">
            {index + 1}
          </span>
          <span>{step}</span>
        </li>
      ))}
    </ol>
  );
}
