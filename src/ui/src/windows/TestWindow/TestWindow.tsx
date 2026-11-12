import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';

import type { HostStatus } from '@/ipc';
import { useFeed } from '@/queries';
import { appInfoOptions, hostStatusFeed, hostStatusOptions } from '@/queries/host';
import { errorMessage } from '@/utils';

import { ConfigList } from './components/ConfigList';
import { ModuleList } from './components/ModuleList';
import { card } from './styles/card.style';
import { styles } from './TestWindow.style';

function Alert({ title, children }: { title: string; children: string }) {
  return (
    <div {...stylex.props(styles.alert)} role="alert">
      <p {...stylex.props(styles.alertTitle)}>{title}</p>
      <p {...stylex.props(styles.alertText)}>{children}</p>
    </div>
  );
}

function HostSummary({ status }: { status: HostStatus }) {
  return (
    <div {...stylex.props(styles.summary)}>
      <p
        {...stylex.props(styles.readiness, status.ready && styles.ready)}
        data-ready={status.ready}
      >
        {status.ready ? 'Host ready' : 'Host starting'}
      </p>
      {status.error ? <Alert title="Host error">{status.error}</Alert> : null}
    </div>
  );
}

/**
 * The on-demand developer surface for the host contract: identity, host
 * readiness, modules and configuration files. Read-only; it sends no command
 * beyond `app_info` and `host_status`.
 */
export function TestWindow() {
  useFeed(hostStatusFeed);

  const appInfo = useQuery(appInfoOptions());
  const status = useQuery(hostStatusOptions());
  const app = appInfo.data;

  return (
    <main {...stylex.props(styles.page)} data-surface="test">
      <div {...stylex.props(styles.content)}>
        <header {...stylex.props(styles.header)}>
          <div>
            <h1 {...stylex.props(styles.title)}>{app?.name ?? 'Stache Next'}</h1>
            <p {...stylex.props(styles.subtitle)}>Host test surface</p>
          </div>
          {app ? (
            <span {...stylex.props(styles.identity)}>
              {app.identifier} (v{app.version})
            </span>
          ) : null}
        </header>

        {appInfo.isError ? (
          <p {...stylex.props(styles.note)} role="status">
            Application identity unavailable: {errorMessage(appInfo.error)}
          </p>
        ) : null}

        <section {...stylex.props(card.section)} aria-labelledby="host-heading">
          <h2 {...stylex.props(card.sectionTitle)} id="host-heading">
            Host
          </h2>
          {status.data === undefined && !status.isError ? (
            <p {...stylex.props(styles.note)} role="status">
              Reading host status…
            </p>
          ) : null}
          {status.isError ? (
            <Alert title="Host status unavailable">{errorMessage(status.error)}</Alert>
          ) : null}
          {status.data ? <HostSummary status={status.data} /> : null}
        </section>

        {status.data ? (
          <>
            <ModuleList modules={status.data.modules} />
            <ConfigList files={status.data.config} />
          </>
        ) : null}
      </div>
    </main>
  );
}
