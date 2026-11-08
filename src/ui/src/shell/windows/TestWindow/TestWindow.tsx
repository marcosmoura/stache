import { useAppInfo, useHostStatus } from '../../../sdk';
import type {
  ConfigFileState,
  ConfigFileView,
  HostStatus,
  ModuleState,
  ModuleStatusView,
} from '../../../sdk';

import * as styles from './TestWindow.styles';

const MODULE_STATE_LABELS: Record<ModuleState, string> = {
  stopped: 'Stopped',
  disabled: 'Disabled',
  running: 'Running',
  paused: 'Paused',
  waitingForPermission: 'Waiting for permission',
  unavailable: 'Unavailable',
  failed: 'Failed',
};

const CONFIG_STATE_LABELS: Record<ConfigFileState, string> = {
  missing: 'Missing',
  valid: 'Valid',
  invalid: 'Invalid',
};

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

function HostSummary({ status }: { status: HostStatus }) {
  return (
    <div className={styles.summary}>
      <p className={styles.readiness} data-ready={status.ready}>
        {status.ready ? 'Host ready' : 'Host starting'}
      </p>
      {status.error ? (
        <div className={styles.alert} role="alert">
          <p className={styles.alertTitle}>Host error</p>
          <p>{status.error}</p>
        </div>
      ) : null}
    </div>
  );
}

function ModuleCard({ module }: { module: ModuleStatusView }) {
  return (
    <li className={styles.card}>
      <div className={styles.cardHeading}>
        <h3 className={styles.cardTitle}>{module.id}</h3>
        <span className={styles.stateBadge} data-state={module.state}>
          {MODULE_STATE_LABELS[module.state]}
        </span>
      </div>
      <dl className={styles.meta}>
        <div className={styles.metaRow}>
          <dt className={styles.metaLabel}>Enabled</dt>
          <dd className={styles.metaValue}>{module.enabled ? 'Yes' : 'No'}</dd>
        </div>
        <div className={styles.metaRow}>
          <dt className={styles.metaLabel}>Paused</dt>
          <dd className={styles.metaValue}>{module.paused ? 'Yes' : 'No'}</dd>
        </div>
        <div className={styles.metaRow}>
          <dt className={styles.metaLabel}>Revision</dt>
          <dd className={styles.metaValue}>
            <code>{module.revision}</code>
          </dd>
        </div>
      </dl>
      {module.detail ? <p className={styles.note}>{module.detail}</p> : null}
      {module.diagnostic ? (
        <div className={styles.diagnostic}>
          <h4 className={styles.label}>Configuration diagnostic</h4>
          <p>{module.diagnostic}</p>
        </div>
      ) : null}
    </li>
  );
}

function ModulesSection({ modules }: { modules: ModuleStatusView[] }) {
  return (
    <section className={styles.panel} aria-labelledby="modules-heading">
      <h2 className={styles.sectionTitle} id="modules-heading">
        Modules
      </h2>
      {modules.length === 0 ? (
        <p className={styles.empty}>No modules registered.</p>
      ) : (
        <ul className={styles.list}>
          {modules.map((module) => (
            <ModuleCard key={module.id} module={module} />
          ))}
        </ul>
      )}
    </section>
  );
}

function ConfigCard({ file }: { file: ConfigFileView }) {
  return (
    <li className={styles.card}>
      <div className={styles.cardHeading}>
        <h3 className={styles.cardTitle}>{file.file}</h3>
        <span className={styles.stateBadge} data-state={file.state}>
          {CONFIG_STATE_LABELS[file.state]}
        </span>
      </div>
      <p className={styles.path}>
        <code>{file.path}</code>
      </p>
      <p className={styles.muted}>
        {file.active ? 'Active configuration' : 'Not part of the active configuration'}
      </p>
      {file.diagnostic ? (
        <div className={styles.diagnostic}>
          <h4 className={styles.label}>File diagnostic</h4>
          <p>{file.diagnostic}</p>
        </div>
      ) : null}
      {file.warnings.length > 0 ? (
        <div className={styles.warnings}>
          <h4 className={styles.label}>Warnings</h4>
          <ul className={styles.list}>
            {file.warnings.map((warning) => (
              <li key={`${file.path}:${warning}`}>{warning}</li>
            ))}
          </ul>
        </div>
      ) : null}
    </li>
  );
}

function ConfigSection({ files }: { files: ConfigFileView[] }) {
  return (
    <section className={styles.panel} aria-labelledby="config-heading">
      <h2 className={styles.sectionTitle} id="config-heading">
        Configuration
      </h2>
      {files.length === 0 ? (
        <p className={styles.empty}>No configuration files.</p>
      ) : (
        <ul className={styles.list}>
          {files.map((file) => (
            <ConfigCard key={file.path} file={file} />
          ))}
        </ul>
      )}
    </section>
  );
}

/** On-demand developer surface for the host contract; no configuration writes. */
export function TestWindow() {
  const appInfo = useAppInfo();
  const status = useHostStatus();
  const app = appInfo.data;

  return (
    <main className={styles.page}>
      <div className={styles.content}>
        <header className={styles.header}>
          <div>
            <h1 className={styles.title}>{app?.name ?? 'Stache Next'}</h1>
            <p className={styles.subtitle}>Host test surface</p>
          </div>
          {app ? (
            <span className={styles.identity}>
              {app.identifier} (v{app.version})
            </span>
          ) : null}
        </header>

        {appInfo.isError ? (
          <p className={styles.note} role="status">
            Application identity unavailable: {errorMessage(appInfo.error)}
          </p>
        ) : null}

        <section className={styles.panel} aria-labelledby="host-heading">
          <h2 className={styles.sectionTitle} id="host-heading">
            Host
          </h2>
          {status.isPending ? (
            <p className={styles.note} role="status">
              Reading host status…
            </p>
          ) : null}
          {status.isError ? (
            <div className={styles.alert} role="alert">
              <p className={styles.alertTitle}>Host status unavailable</p>
              <p>{errorMessage(status.error)}</p>
            </div>
          ) : null}
          {status.data ? <HostSummary status={status.data} /> : null}
        </section>

        {status.data ? (
          <>
            <ModulesSection modules={status.data.modules} />
            <ConfigSection files={status.data.config} />
          </>
        ) : null}
      </div>
    </main>
  );
}
