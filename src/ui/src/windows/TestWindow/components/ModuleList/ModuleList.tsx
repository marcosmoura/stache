import type { ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import type { ModuleStatusView } from '@/ipc';

import { card } from '../../styles/card.style';
import { MODULE_STATE_LABELS, yesNo } from '../../TestWindow.labels';
import { StateBadge } from '../StateBadge';

import { styles } from './ModuleList.style';

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div {...stylex.props(styles.row)}>
      <dt {...stylex.props(styles.term)}>{label}</dt>
      <dd {...stylex.props(styles.value)}>{children}</dd>
    </div>
  );
}

function ModuleCard({ module }: { module: ModuleStatusView }) {
  return (
    <li {...stylex.props(card.card)}>
      <div {...stylex.props(card.heading)}>
        <h3 {...stylex.props(card.title)}>{module.id}</h3>
        <StateBadge state={module.state}>{MODULE_STATE_LABELS[module.state]}</StateBadge>
      </div>
      <dl {...stylex.props(styles.meta)}>
        <Row label="Enabled">{yesNo(module.enabled)}</Row>
        <Row label="Paused">{yesNo(module.paused)}</Row>
        <Row label="Revision">
          {/* Opaque decimal text: never converted to a number. */}
          <code {...stylex.props(styles.code)}>{module.revision}</code>
        </Row>
      </dl>
      {module.detail ? (
        <div {...stylex.props(card.callout, card.diagnostic)} data-kind="detail">
          <h4 {...stylex.props(card.calloutLabel)}>Runtime detail</h4>
          <p {...stylex.props(card.calloutText)}>{module.detail}</p>
        </div>
      ) : null}
      {module.diagnostic ? (
        <div {...stylex.props(card.callout, card.diagnostic)} data-kind="diagnostic">
          <h4 {...stylex.props(card.calloutLabel)}>Configuration diagnostic</h4>
          <p {...stylex.props(card.calloutText)}>{module.diagnostic}</p>
        </div>
      ) : null}
    </li>
  );
}

/** Every registered module with its runtime state and rejected-configuration diagnostic. */
export function ModuleList({ modules }: { modules: readonly ModuleStatusView[] }) {
  return (
    <section {...stylex.props(card.section)} aria-labelledby="modules-heading">
      <h2 {...stylex.props(card.sectionTitle)} id="modules-heading">
        Modules
      </h2>
      {modules.length === 0 ? (
        <p {...stylex.props(card.empty)}>No modules registered.</p>
      ) : (
        <ul {...stylex.props(card.list)}>
          {modules.map((module) => (
            <ModuleCard key={module.id} module={module} />
          ))}
        </ul>
      )}
    </section>
  );
}
