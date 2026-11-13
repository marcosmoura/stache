import type { HostStatus } from '@/ipc';

/** What the guide shows. */
export type GuideSection = 'checking' | 'blocked' | 'revoked' | 'open';

/** The hero badge tone. */
export type GuideTone = 'neutral' | 'accent' | 'attention' | 'success';

/** The section for a host status; no status yet is `checking`. */
export function guideSection(status: HostStatus | undefined): GuideSection {
  if (!status) {
    return 'checking';
  }

  const { state, revoked } = status.access;

  if (state === 'blocked') {
    return revoked ? 'revoked' : 'blocked';
  }

  return state;
}

/** Checking and open center a large badge; blocked and revoked put it beside the title. */
export function isCentered(section: GuideSection): boolean {
  return section === 'checking' || section === 'open';
}

type Hero = { tone: GuideTone; title: string; body: string };

/** The hero of each section, naming the app `name`. */
export function heroes(name: string): Readonly<Record<GuideSection, Hero>> {
  return {
    checking: { tone: 'neutral', title: name, body: 'Checking Accessibility access…' },
    blocked: {
      tone: 'accent',
      title: `${name} needs Accessibility access`,
      body: `${name} reads keyboard shortcuts and controls windows and apps. macOS only allows that with Accessibility access. Until you allow it, ${name} waits and does nothing.`,
    },
    revoked: {
      tone: 'attention',
      title: 'Accessibility access was turned off',
      body: `${name} stopped all features until it is turned on again.`,
    },
    open: { tone: 'success', title: name, body: `Access granted. Starting ${name}…` },
  };
}

/** The alert line: the host error, then any failed command, joined with `; `. */
export function alertMessage(hostError: string | null | undefined, failures: string[]): string {
  return [hostError, ...failures].filter(Boolean).join('; ');
}
