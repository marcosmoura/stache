import { useEffect, useState } from 'react';

import {
  Alert02Icon,
  File01Icon,
  PauseIcon,
  PlayIcon,
  PowerIcon,
  RefreshIcon,
  TestTube01Icon,
} from '@hugeicons/core-free-icons';
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
import {
  useOpenAccessibilitySettings,
  useQuitApp,
  useRequestAccessibility,
} from '@/queries/access';
import { hostStatusFeed, hostStatusOptions } from '@/queries/host';
import {
  moduleSessionActionOptions,
  retryModuleOptions,
  setModuleEnabledOptions,
  useOpenAccessibilityGuide,
  useOpenTestWindow,
} from '@/queries/overview';
import { errorMessage } from '@/utils';

import { FILE_ICONS } from '../../Popover.tabs';

import {
  bannerTitle,
  headerLine,
  moduleActions,
  moduleFile,
  moduleName,
  enableOutcomeText,
  reconcile,
  sessionOutcomeText,
  STATE_LABELS,
  STATE_TONES,
  type ErrorMap,
  type Pending,
  type PendingMap,
} from './OverviewTab.model';
import { styles } from './OverviewTab.style';

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
              <li key={file.file} {...stylex.props(styles.row)} data-file={file.file}>
                <span {...stylex.props(styles.tile)}>
                  <Icon icon={FILE_ICONS[file.file] ?? File01Icon} size={16} />
                </span>
                <div {...stylex.props(styles.text)}>
                  <span {...stylex.props(styles.name)}>{file.file}</span>
                  {file.diagnostic != null ? (
                    <span
                      {...stylex.props(styles.secondary, styles.danger)}
                      title={file.diagnostic}
                    >
                      {file.diagnostic}
                    </span>
                  ) : null}
                  {file.warnings.map((warning, index) => (
                    <span
                      key={index}
                      {...stylex.props(styles.secondary, styles.warning)}
                      title={warning}
                    >
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

function AccessBanner({ status }: { status: HostStatus }) {
  const guide = useOpenAccessibilityGuide();
  const request = useRequestAccessibility();
  const settings = useOpenAccessibilitySettings();
  const title = bannerTitle(status.access);

  if (title === null) {
    return null;
  }

  return (
    <section {...stylex.props(styles.banner)} aria-labelledby="overview-access" role="region">
      <div {...stylex.props(styles.bannerHead)}>
        <Icon icon={Alert02Icon} size={16} />
        <h3 {...stylex.props(styles.bannerTitle)} id="overview-access">
          {title}
        </h3>
      </div>
      <p {...stylex.props(styles.bannerText)}>
        Stache needs Accessibility to run its modules. Until it is granted every module waits;
        Enable, Pause, the Test Window and Quit keep working.
      </p>
      <div {...stylex.props(styles.bannerActions)}>
        <button
          type="button"
          {...stylex.props(styles.button, styles.primary)}
          disabled={guide.isPending}
          onClick={() => guide.mutate()}
        >
          Show Accessibility Help…
        </button>
        <button
          type="button"
          {...stylex.props(styles.button)}
          disabled={request.isPending}
          onClick={() => request.mutate()}
        >
          Request Accessibility…
        </button>
        <button
          type="button"
          {...stylex.props(styles.button)}
          disabled={settings.isPending}
          onClick={() => settings.mutate()}
        >
          Open Accessibility Settings…
        </button>
      </div>
    </section>
  );
}

type ModuleRowProps = {
  module: ModuleStatusView;
  pending: 'enable' | 'retry' | 'session' | null;
  error: string | null;
  hasTab: boolean;
  onSelect: () => void;
  onToggle: () => void;
  onSession: (action: SessionAction) => void;
  onRetry: () => void;
};

function ModuleRow({
  module,
  pending,
  error,
  hasTab,
  onSelect,
  onToggle,
  onSession,
  onRetry,
}: ModuleRowProps) {
  const name = moduleName(module.id);
  const actions = moduleActions(module);
  const busy = pending !== null;
  const tone = STATE_TONES[module.state];

  return (
    <li
      {...stylex.props(styles.row)}
      data-module={module.id}
      data-state={module.state}
      data-pending={pending ?? undefined}
      aria-busy={busy}
    >
      <span {...stylex.props(styles.tile)}>
        <Icon icon={FILE_ICONS[moduleFile(module.id)] ?? File01Icon} size={16} />
      </span>
      <div {...stylex.props(styles.text)}>
        <div {...stylex.props(styles.titleLine)}>
          {hasTab ? (
            <button
              type="button"
              {...stylex.props(styles.name, styles.nameButton)}
              onClick={onSelect}
              title={`Open ${moduleFile(module.id)}`}
            >
              {name}
            </button>
          ) : (
            <span {...stylex.props(styles.name)}>{name}</span>
          )}
          <span {...stylex.props(styles.state)} data-testid="state" data-tone={tone}>
            <span {...stylex.props(styles.dot, styles[tone])} aria-hidden="true" />
            {STATE_LABELS[module.state]}
          </span>
        </div>
        {module.detail ? (
          <span {...stylex.props(styles.secondary)} title={module.detail}>
            {module.detail}
          </span>
        ) : null}
        {module.diagnostic ? (
          <span
            {...stylex.props(styles.secondary, styles.attention)}
            title={`configuration: ${module.diagnostic}`}
          >
            configuration: {module.diagnostic}
          </span>
        ) : null}
        {error ? (
          <span {...stylex.props(styles.secondary, styles.danger)} role="alert" title={error}>
            {error}
          </span>
        ) : null}
      </div>
      <div {...stylex.props(styles.controls)}>
        {actions.retry ? (
          <button
            type="button"
            {...stylex.props(styles.round)}
            aria-label={`Retry ${name}`}
            title={`Retry ${name}`}
            disabled={busy}
            onClick={onRetry}
          >
            <Icon icon={RefreshIcon} size={14} />
          </button>
        ) : null}
        {actions.session ? (
          <button
            type="button"
            {...stylex.props(styles.round)}
            aria-label={`${actions.session === 'pause' ? 'Pause' : 'Resume'} ${name}`}
            title={`${actions.session === 'pause' ? 'Pause' : 'Resume'} ${name}`}
            disabled={busy}
            onClick={() => onSession(actions.session ?? 'pause')}
          >
            <Icon icon={actions.session === 'pause' ? PauseIcon : PlayIcon} size={14} />
          </button>
        ) : null}
        <button
          type="button"
          role="switch"
          aria-checked={module.enabled}
          aria-label={`Enable ${name}`}
          title={`${module.enabled ? 'Disable' : 'Enable'} ${name}`}
          disabled={busy}
          {...stylex.props(styles.switch, module.enabled && styles.switchOn)}
          onClick={onToggle}
        >
          <span {...stylex.props(styles.knob, module.enabled && styles.knobOn)} />
        </button>
      </div>
    </li>
  );
}

function Footer() {
  const testWindow = useOpenTestWindow();
  const quit = useQuitApp();

  return (
    <nav {...stylex.props(styles.footer)} aria-label="Actions">
      <button
        type="button"
        {...stylex.props(styles.action)}
        disabled={testWindow.isPending}
        onClick={() => testWindow.mutate()}
      >
        <Icon icon={TestTube01Icon} size={16} />
        Open Test Window
      </button>
      <hr {...stylex.props(styles.separator)} />
      <button
        type="button"
        {...stylex.props(styles.action)}
        disabled={quit.isPending}
        onClick={() => quit.mutate()}
      >
        <Icon icon={PowerIcon} size={16} />
        Quit Stache Next
      </button>
    </nav>
  );
}
