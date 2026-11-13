import { useEffect, useState } from 'react';

import { File01Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';
import { useMutation, useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import {
  events,
  subscribe,
  type ConfigFileView,
  type HostStatus,
  type ModuleStatusView,
  type SessionAction,
} from '@/ipc';
import { useFeed } from '@/queries';
import { hostStatusFeed, hostStatusOptions } from '@/queries/host';
import {
  moduleSessionActionOptions,
  retryModuleOptions,
  setModuleEnabledOptions,
} from '@/queries/overview';
import { errorMessage } from '@/utils';
import { FILE_ICONS } from '@/windows/Popover/utils/tabs';

import { AccessBanner } from './components/AccessBanner';
import { Footer } from './components/Footer';
import { ModuleRow } from './components/ModuleRow';
import { styles } from './OverviewTab.style';
import { styles as row } from './styles/row.style';
import {
  headerLine,
  moduleFile,
  enableOutcomeText,
  reconcile,
  sessionOutcomeText,
  type ErrorMap,
  type Pending,
  type PendingMap,
} from './utils/modules';

export type OverviewTabProps = {
  /** Selects a sidebar tab (a module's name selects its file tab). */
  onSelectTab: (id: string) => void;
  /** The catalog files, to know which module has a file tab. */
  files: readonly ConfigFileView[];
};

/**
 * The popover's Overview: everything the former tray menu offered. A host
 * status line, the Accessibility banner while the gate is blocked, one row
 * per module (state, enable switch, Pause/Resume and Retry when eligible,
 * runtime detail and configuration diagnostic), file diagnostics and the
 * footer actions. Controls never flip optimistically: a clicked control is
 * pending until the host status reflects it or its outcome arrives.
 */
export function OverviewTab({ onSelectTab, files }: OverviewTabProps) {
  useFeed(hostStatusFeed);

  const status = useQuery(hostStatusOptions());
  const [rows, setRows] = useState<{ pending: PendingMap; errors: ErrorMap }>({
    pending: {},
    errors: {},
  });
  const { pending, errors } = rows;
  const data = status.data;

  // Every `status-changed` (even one equal to the last, which the query
  // keeps as the same object) settles the pending controls it reflects.
  useEffect(
    () =>
      subscribe(events.statusChanged, (payload) => {
        setRows((current) =>
          reconcile(current.pending, current.errors, payload.status as HostStatus),
        );
      }),
    [],
  );

  const enable = useMutation(setModuleEnabledOptions());
  const session = useMutation(moduleSessionActionOptions());
  const retry = useMutation(retryModuleOptions());

  const settle = (id: string, error: string | null) => {
    setRows((current) => {
      const { [id]: _settled, ...rest } = current.pending;

      return {
        pending: rest,
        errors: error === null ? current.errors : { ...current.errors, [id]: error },
      };
    });
  };

  const begin = (id: string, entry: Pending) => {
    setRows((current) => {
      const { [id]: _cleared, ...errors } = current.errors;

      return { pending: { ...current.pending, [id]: entry }, errors };
    });
  };

  const onToggle = (module: ModuleStatusView) => {
    const target = !module.enabled;

    begin(module.id, { kind: 'enable', target });
    enable.mutate(
      { id: module.id, enabled: target },
      {
        onSuccess: (outcome) => settle(module.id, enableOutcomeText(outcome)),
        onError: (error) => settle(module.id, errorMessage(error)),
      },
    );
  };

  const onSession = (module: ModuleStatusView, action: SessionAction) => {
    begin(module.id, { kind: 'session', action });
    session.mutate(
      { id: module.id, action },
      {
        onSuccess: (outcome) => settle(module.id, sessionOutcomeText(action, outcome)),
        onError: (error) => settle(module.id, errorMessage(error)),
      },
    );
  };

  const onRetry = (module: ModuleStatusView, baseline: HostStatus) => {
    begin(module.id, { kind: 'retry', baseline });
    retry.mutate(module.id, { onError: (error) => settle(module.id, errorMessage(error)) });
  };

  if (data === undefined) {
    return status.isError ? (
      <p {...stylex.props(styles.error)} role="alert">
        Host status unavailable: {errorMessage(status.error)}
      </p>
    ) : (
      <p {...stylex.props(styles.muted)} role="status">
        Reading host status…
      </p>
    );
  }

  const header = headerLine(data);
  const fileNames = new Set(files.map((file) => file.file));
  const notes = data.config.filter((file) => file.diagnostic != null || file.warnings.length > 0);

  return (
    <div {...stylex.props(styles.root)} data-testid="overview">
      {header ? (
        <p {...stylex.props(styles.muted)} role="status">
          {header}
        </p>
      ) : null}
      {data.error ? (
        <p {...stylex.props(styles.error)} role="alert" title={data.error}>
          {data.error}
        </p>
      ) : null}
      <AccessBanner status={data} />

      <section aria-labelledby="overview-modules">
        <h3 {...stylex.props(styles.heading)} id="overview-modules">
          Modules
        </h3>
        <ul {...stylex.props(styles.card)} aria-labelledby="overview-modules">
          {data.modules.map((module) => (
            <ModuleRow
              key={module.id}
              module={module}
              pending={pending[module.id]?.kind ?? null}
              error={errors[module.id] ?? null}
              hasTab={fileNames.has(moduleFile(module.id))}
              onSelect={() => onSelectTab(moduleFile(module.id))}
              onToggle={() => onToggle(module)}
              onSession={(action) => onSession(module, action)}
              onRetry={() => onRetry(module, data)}
            />
          ))}
        </ul>
      </section>

      {notes.length > 0 ? (
        <section aria-labelledby="overview-config">
          <h3 {...stylex.props(styles.heading)} id="overview-config">
            Configuration
          </h3>
          <ul {...stylex.props(styles.card)} aria-labelledby="overview-config">
            {notes.map((file) => (
              <li key={file.file} {...stylex.props(row.row)} data-file={file.file}>
                <span {...stylex.props(row.tile)}>
                  <Icon icon={FILE_ICONS[file.file] ?? File01Icon} size={16} />
                </span>
                <div {...stylex.props(row.text)}>
                  <span {...stylex.props(row.name)}>{file.file}</span>
                  {file.diagnostic != null ? (
                    <span {...stylex.props(row.secondary, row.danger)} title={file.diagnostic}>
                      {file.diagnostic}
                    </span>
                  ) : null}
                  {file.warnings.map((warning, index) => (
                    <span key={index} {...stylex.props(row.secondary, row.warning)} title={warning}>
                      {warning}
                    </span>
                  ))}
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <Footer />
    </div>
  );
}
