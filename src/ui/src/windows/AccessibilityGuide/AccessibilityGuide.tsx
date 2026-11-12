import { useEffect } from 'react';

import {
  Alert02Icon,
  AlertCircleIcon,
  Cancel01Icon,
  CommandIcon,
  Loading03Icon,
  ShieldKeyIcon,
  Tick02Icon,
} from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';

import { Icon, type IconSource } from '@/components/Icon';
import { closeCurrentWindow, dragRegion, type HostStatus } from '@/ipc';
import { useFeed } from '@/queries';
import {
  useOpenAccessibilitySettings,
  useQuitApp,
  useRequestAccessibility,
} from '@/queries/access';
import { appInfoOptions, hostStatusFeed, hostStatusOptions } from '@/queries/host';
import { errorMessage } from '@/utils';

import {
  alertMessage,
  guideSection,
  HEROES,
  isCentered,
  type GuideSection,
} from './AccessibilityGuide.state';
import { styles } from './AccessibilityGuide.style';
import { ActionFooter } from './components/ActionFooter';
import { GuideSteps } from './components/GuideSteps';
import { HeroBadge } from './components/HeroBadge';
import { IdentityCard } from './components/IdentityCard';
import { Troubleshooting } from './components/Troubleshooting';

const ICONS: Readonly<Record<GuideSection, IconSource>> = {
  checking: Loading03Icon,
  blocked: ShieldKeyIcon,
  revoked: Alert02Icon,
  open: Tick02Icon,
};

const selectAccess = (status: HostStatus) => ({
  section: guideSection(status),
  appPath: status.access.appPath ?? null,
  error: status.error ?? null,
});

function onEscape(event: KeyboardEvent) {
  if (event.key === 'Escape' && !event.repeat) {
    void closeCurrentWindow();
  }
}

/**
 * The window shown while the Accessibility launch gate blocks. It reads the
 * host status feed; its commands return at once and their effect arrives
 * through that feed. It closes itself once access is open.
 */
export function AccessibilityGuide() {
  useFeed(hostStatusFeed);

  const { data: app } = useQuery(appInfoOptions());
  const { data: access } = useQuery({ ...hostStatusOptions(), select: selectAccess });
  const request = useRequestAccessibility();
  const settings = useOpenAccessibilitySettings();
  const quit = useQuitApp();
  const section: GuideSection = access?.section ?? 'checking';
  const centered = isCentered(section);
  const hero = HEROES[section];
  const message = alertMessage(
    access?.error,
    [request, settings, quit].flatMap((mutation) =>
      mutation.isError ? [errorMessage(mutation.error)] : [],
    ),
  );

  useEffect(() => {
    if (section === 'open') {
      void closeCurrentWindow();
    }
  }, [section]);

  useEffect(() => {
    window.addEventListener('keydown', onEscape);

    return () => window.removeEventListener('keydown', onEscape);
  }, []);

  return (
    <main {...stylex.props(styles.panel)} data-surface="access" data-state={section}>
      <header {...stylex.props(styles.header)}>
        <div {...stylex.props(styles.dragRegion)} {...dragRegion}>
          <span {...stylex.props(styles.glyph, styles.passThrough)}>
            <Icon icon={CommandIcon} size={13} strokeWidth={1.75} />
          </span>
          <span {...stylex.props(styles.appName, styles.passThrough)}>Stache Next</span>
        </div>
        <button
          type="button"
          {...stylex.props(styles.close)}
          aria-label="Close"
          onClick={() => void closeCurrentWindow()}
        >
          <Icon icon={Cancel01Icon} size={16} strokeWidth={1.75} />
        </button>
      </header>

      <div {...stylex.props(styles.scroll)}>
        <div key={section} {...stylex.props(styles.content, centered && styles.centered)}>
          <div {...stylex.props(styles.hero, centered && styles.heroCentered)}>
            <HeroBadge tone={hero.tone} icon={ICONS[section]} large={centered} />
            <div {...stylex.props(styles.heroText)}>
              <h1 {...stylex.props(styles.title)}>{hero.title}</h1>
              <p {...stylex.props(styles.body)} role={centered ? 'status' : undefined}>
                {hero.body}
              </p>
            </div>
          </div>
          {section === 'blocked' || section === 'revoked' ? (
            <>
              <GuideSteps />
              <IdentityCard
                name={app?.name ?? 'Stache Next'}
                identifier={app?.identifier ?? null}
                appPath={access?.appPath ?? null}
              />
              <Troubleshooting bundled={access?.appPath != null} />
            </>
          ) : null}
        </div>
      </div>

      {message ? (
        <p {...stylex.props(styles.alert)} role="alert">
          <span {...stylex.props(styles.alertIcon)}>
            <Icon icon={AlertCircleIcon} size={16} strokeWidth={1.75} />
          </span>
          <span>{message}</span>
        </p>
      ) : null}

      {section === 'open' ? null : (
        <ActionFooter
          actions={[
            {
              id: 'request',
              label: 'Request Access',
              pending: request.isPending,
              onRun: () => request.mutate(),
            },
            {
              id: 'settings',
              label: 'Open System Settings',
              pending: settings.isPending,
              onRun: () => settings.mutate(),
            },
            {
              id: 'quit',
              label: 'Quit Stache Next',
              pending: quit.isPending,
              onRun: () => quit.mutate(),
            },
          ]}
        />
      )}
    </main>
  );
}
