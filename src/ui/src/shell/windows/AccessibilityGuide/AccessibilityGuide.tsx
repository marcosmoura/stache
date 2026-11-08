import { useEffect, useState } from 'react';

import {
  Alert02Icon,
  AlertCircleIcon,
  ArrowRight01Icon,
  Cancel01Icon,
  CommandIcon,
  InformationCircleIcon,
  Loading03Icon,
  ShieldKeyIcon,
  Tick02Icon,
} from '@hugeicons/core-free-icons';
import { HugeiconsIcon } from '@hugeicons/react';
import type { IconSvgElement } from '@hugeicons/react';
import { getCurrentWindow } from '@tauri-apps/api/window';

import {
  openAccessibilitySettings,
  quitApp,
  requestAccessibility,
  useAppInfo,
  useHostStatus,
} from '../../../sdk';
import type { AppInfo, HostStatus } from '../../../sdk';

import * as styles from './AccessibilityGuide.styles';

type Action = 'request' | 'settings' | 'quit';
type Tone = 'overlay' | 'mauve' | 'peach' | 'green';

const COMMANDS: Record<Action, () => Promise<void>> = {
  request: requestAccessibility,
  settings: openAccessibilitySettings,
  quit: quitApp,
};

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

function closeSelf() {
  try {
    void getCurrentWindow()
      .close()
      .catch(() => {});
  } catch {
    // Outside a Tauri webview there is no window to close.
  }
}

function Icon({ icon, size = 16 }: { icon: IconSvgElement; size?: number }) {
  return <HugeiconsIcon icon={icon} size={size} strokeWidth={1.75} aria-hidden="true" />;
}

function Identity({ app, status }: { app: AppInfo | undefined; status: HostStatus }) {
  const { appPath } = status.access;

  return (
    <div className={styles.identity}>
      <dl className={styles.fields}>
        <div className={styles.field}>
          <dt>Name</dt>
          <dd>{app?.name ?? 'Stache Next'}</dd>
        </div>
        {app ? (
          <div className={styles.field}>
            <dt>Bundle ID</dt>
            <dd>{app.identifier}</dd>
          </div>
        ) : null}
        <div className={styles.field}>
          <dt>Path</dt>
          <dd>{appPath ?? 'not an app bundle'}</dd>
        </div>
      </dl>
      <p className={styles.hint}>
        <Icon icon={InformationCircleIcon} size={14} />
        <span>Make sure you turn on Stache Next, not Stache.</span>
      </p>
    </div>
  );
}

function Guidance({ app, status }: { app: AppInfo | undefined; status: HostStatus }) {
  const { appPath } = status.access;

  return (
    <>
      <ol className={styles.steps}>
        <li>
          <span>
            Click <strong>Request Access</strong>, or <strong>Open System Settings</strong>.
          </span>
        </li>
        <li>
          <span>
            In Privacy &amp; Security &gt; Accessibility, turn on <strong>Stache Next</strong>.
          </span>
        </li>
        <li>
          <span>Come back here; Stache Next starts by itself.</span>
        </li>
      </ol>
      <Identity app={app} status={status} />
      <details className={styles.troubleshooting}>
        <summary>
          <span className={styles.chevron} data-chevron="true">
            <Icon icon={ArrowRight01Icon} size={14} />
          </span>
          Troubleshooting
        </summary>
        {appPath ? (
          <p>
            Stache Next is already on but this window stays open? The entry belongs to an older
            build. Select it, remove it with −, then turn on the new entry or add the app at the
            path above.
          </p>
        ) : (
          <p>
            This copy is not an app bundle. macOS may assign the permission to the app that launched
            it, such as your terminal.
          </p>
        )}
      </details>
    </>
  );
}

function Hero({
  tone,
  icon,
  title,
  centered = false,
  children,
}: {
  tone: Tone;
  centered?: boolean;
  icon: IconSvgElement;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className={styles.hero} data-centered={centered ? 'true' : undefined}>
      <span className={styles.badge} data-tone={tone}>
        <Icon icon={icon} size={centered ? 28 : 22} />
      </span>
      <div className={styles.heroText}>
        <h1 className={styles.title}>{title}</h1>
        {children}
      </div>
    </div>
  );
}

function ActionButton({
  action,
  busy,
  primary,
  onRun,
  children,
}: {
  action: Action;
  busy: boolean;
  primary?: boolean;
  onRun: (action: Action) => void;
  children: string;
}) {
  return (
    <button
      type="button"
      className={styles.button}
      data-primary={primary ? 'true' : undefined}
      data-action={action}
      disabled={busy}
      aria-busy={busy ? 'true' : undefined}
      onClick={() => onRun(action)}
    >
      {busy ? (
        <span className={styles.spinner} data-spinner="true">
          <Icon icon={Loading03Icon} size={14} />
        </span>
      ) : null}
      {children}
    </button>
  );
}

/**
 * Guidance window shown while the Accessibility launch gate blocks. It reads the
 * shared host status feed only; commands return immediately and their results
 * arrive through that feed.
 */
export function AccessibilityGuide() {
  const appInfo = useAppInfo();
  const status = useHostStatus();
  const [busy, setBusy] = useState<ReadonlySet<Action>>(new Set());
  const [failure, setFailure] = useState<string | null>(null);
  const state = status.data?.access.state ?? 'checking';

  useEffect(() => {
    if (state === 'open') {
      closeSelf();
    }
  }, [state]);

  useEffect(() => {
    // The window is transparent; only this surface clears the document background.
    document.documentElement.dataset.surface = 'access';

    return () => {
      delete document.documentElement.dataset.surface;
    };
  }, []);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape' && !event.repeat) {
        closeSelf();
      }
    }
    window.addEventListener('keydown', onKeyDown);

    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  async function run(action: Action) {
    setFailure(null);
    setBusy((current) => new Set(current).add(action));

    try {
      await COMMANDS[action]();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy((current) => {
        const next = new Set(current);

        next.delete(action);

        return next;
      });
    }
  }

  const data = status.data;
  const revoked = state === 'blocked' && data?.access.revoked === true;
  const message = [data?.error, failure].filter(Boolean).join('; ');
  const panelState = !data ? 'checking' : state;
  const section = !data || state === 'checking' ? 'checking' : revoked ? 'revoked' : state;

  const centered = section === 'checking' || section === 'open';

  return (
    <main className={`${styles.surface} ${styles.panel}`} data-state={panelState}>
      <header className={styles.header}>
        <div className={styles.dragRegion} data-tauri-drag-region>
          <span className={styles.glyph}>
            <Icon icon={CommandIcon} size={13} />
          </span>
          <span className={styles.appName}>Stache Next</span>
        </div>
        <button type="button" className={styles.close} aria-label="Close" onClick={closeSelf}>
          <Icon icon={Cancel01Icon} size={16} />
        </button>
      </header>

      <div className={styles.scroll} data-layout={centered ? 'centered' : 'top'}>
        <div className={styles.fade} key={section}>
          {section === 'checking' ? (
            <Hero tone="overlay" icon={Loading03Icon} title="Stache Next" centered>
              <p className={styles.body} role="status">
                Checking Accessibility access…
              </p>
            </Hero>
          ) : null}

          {data && state === 'open' ? (
            <Hero tone="green" icon={Tick02Icon} title="Stache Next" centered>
              <p className={styles.body} role="status">
                Access granted. Starting Stache Next…
              </p>
            </Hero>
          ) : null}

          {data && state === 'blocked' ? (
            <>
              <Hero
                tone={revoked ? 'peach' : 'mauve'}
                icon={revoked ? Alert02Icon : ShieldKeyIcon}
                title={
                  revoked
                    ? 'Accessibility access was turned off'
                    : 'Stache Next needs Accessibility access'
                }
              >
                <p className={styles.body}>
                  {revoked
                    ? 'Stache Next stopped all features until it is turned on again.'
                    : 'Stache Next reads keyboard shortcuts and controls windows and apps. macOS only allows that with Accessibility access. Until you grant it, Stache Next stays idle.'}
                </p>
              </Hero>
              <Guidance app={appInfo.data} status={data} />
            </>
          ) : null}
        </div>
      </div>

      {message ? (
        <p className={styles.alert} role="alert">
          <Icon icon={AlertCircleIcon} size={16} />
          <span>{message}</span>
        </p>
      ) : null}

      {state === 'open' && data ? null : (
        <footer className={styles.actions}>
          <ActionButton action="request" busy={busy.has('request')} primary onRun={run}>
            Request Access
          </ActionButton>
          <ActionButton action="settings" busy={busy.has('settings')} onRun={run}>
            Open System Settings
          </ActionButton>
          <ActionButton action="quit" busy={busy.has('quit')} onRun={run}>
            Quit Stache Next
          </ActionButton>
        </footer>
      )}
    </main>
  );
}
