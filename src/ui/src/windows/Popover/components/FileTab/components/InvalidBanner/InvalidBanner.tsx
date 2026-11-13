import * as stylex from '@stylexjs/stylex';

import type { ConfigDiagnosticView_Serialize } from '@/ipc';

import { diagnosticSentence, diagnosticText } from '../../utils/diagnostic';

import { styles } from './InvalidBanner.style';

/**
 * The banner of an invalid file: a plain-language sentence about the first
 * problem (from the line and a small mapping of parser messages), the raw
 * parser text behind "Details", and how to make the form editable again.
 */
export function InvalidBanner({
  diagnostic,
}: {
  diagnostic: ConfigDiagnosticView_Serialize | null;
}) {
  return (
    <div {...stylex.props(styles.alert)} role="alert">
      <p {...stylex.props(styles.alertTitle)}>This file is invalid and read-only</p>
      {diagnostic ? (
        <>
          <p {...stylex.props(styles.list)}>{diagnosticSentence(diagnostic)}</p>
          <details {...stylex.props(styles.list, styles.details)}>
            <summary {...stylex.props(styles.summary)}>Details</summary>
            <code {...stylex.props(styles.raw)}>{diagnosticText(diagnostic)}</code>
          </details>
        </>
      ) : null}
      <p {...stylex.props(styles.list, styles.muted)}>
        Fix it in an editor; the form shows the defaults and becomes editable as soon as a valid
        save lands.
      </p>
    </div>
  );
}
