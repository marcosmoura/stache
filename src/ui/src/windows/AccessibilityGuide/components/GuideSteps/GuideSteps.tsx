import type { ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import { PRODUCT_NAME } from '@/identity';

import { styles } from './GuideSteps.style';

function Strong({ children }: { children: ReactNode }) {
  return <strong {...stylex.props(styles.strong)}>{children}</strong>;
}

const STEPS: ReactNode[] = [
  <>
    Click <Strong>Request Access</Strong>, or <Strong>Open System Settings</Strong>.
  </>,
  <>
    In Privacy &amp; Security &gt; Accessibility, turn on <Strong>{PRODUCT_NAME}</Strong>.
  </>,
  <>Come back here; {PRODUCT_NAME} starts by itself.</>,
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
