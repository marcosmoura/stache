import * as stylex from '@stylexjs/stylex';

import { styles } from './Warnings.style';

/** The file's warnings (unknown keys in an otherwise valid file), at the top of the tab. */
export function Warnings({ warnings }: { warnings: readonly string[] }) {
  if (warnings.length === 0) {
    return null;
  }

  return (
    <section {...stylex.props(styles.alert)} aria-label="Warnings">
      <p {...stylex.props(styles.warningTitle)}>Warnings</p>
      <ul {...stylex.props(styles.list)}>
        {warnings.map((warning, index) => (
          <li key={index}>{warning}</li>
        ))}
      </ul>
    </section>
  );
}
