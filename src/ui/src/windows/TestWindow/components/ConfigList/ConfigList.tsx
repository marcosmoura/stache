import * as stylex from '@stylexjs/stylex';

import type { ConfigFileView } from '@/ipc';

import { card } from '../../styles/card.style';
import { CONFIG_STATE_LABELS } from '../../TestWindow.labels';
import { StateBadge } from '../StateBadge';

import { styles } from './ConfigList.style';

function ConfigCard({ file }: { file: ConfigFileView }) {
  return (
    <li {...stylex.props(card.card)}>
      <div {...stylex.props(card.heading)}>
        <h3 {...stylex.props(card.title)}>{file.file}</h3>
        <StateBadge state={file.state}>{CONFIG_STATE_LABELS[file.state]}</StateBadge>
      </div>
      <p {...stylex.props(styles.path)}>
        <code>{file.path}</code>
      </p>
      <p {...stylex.props(card.muted)}>
        {file.active ? 'Active configuration' : 'Not part of the active configuration'}
      </p>
      {file.diagnostic ? (
        <div {...stylex.props(card.callout, card.diagnostic)} data-kind="diagnostic">
          <h4 {...stylex.props(card.calloutLabel)}>File diagnostic</h4>
          <p {...stylex.props(card.calloutText)}>{file.diagnostic}</p>
        </div>
      ) : null}
      {file.warnings.length > 0 ? (
        <div {...stylex.props(card.callout, card.warning)} data-kind="warnings">
          <h4 {...stylex.props(card.calloutLabel)}>Warnings</h4>
          <ul {...stylex.props(card.calloutList)}>
            {file.warnings.map((warning) => (
              <li key={warning}>{warning}</li>
            ))}
          </ul>
        </div>
      ) : null}
    </li>
  );
}

/** Every catalog file: validity, whether it is active, its diagnostic and warnings. */
export function ConfigList({ files }: { files: readonly ConfigFileView[] }) {
  return (
    <section {...stylex.props(card.section)} aria-labelledby="config-heading">
      <h2 {...stylex.props(card.sectionTitle)} id="config-heading">
        Configuration
      </h2>
      {files.length === 0 ? (
        <p {...stylex.props(card.empty)}>No configuration files.</p>
      ) : (
        <ul {...stylex.props(card.list)}>
          {files.map((file) => (
            <ConfigCard key={file.path} file={file} />
          ))}
        </ul>
      )}
    </section>
  );
}
