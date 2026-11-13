import { PRODUCT_NAME } from '@/identity';
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

/** The hero of each section. */
export const HEROES: Readonly<Record<GuideSection, Hero>> = {
  checking: { tone: 'neutral', title: PRODUCT_NAME, body: 'Checking Accessibility access…' },
  blocked: {
    tone: 'accent',
    title: `${PRODUCT_NAME} needs Accessibility access`,
    body: `${PRODUCT_NAME} reads keyboard shortcuts and controls windows and apps. macOS only allows that with Accessibility access. Until you allow it, ${PRODUCT_NAME} waits and does nothing.`,
  },
  revoked: {
    tone: 'attention',
    title: 'Accessibility access was turned off',
    body: `${PRODUCT_NAME} stopped all features until it is turned on again.`,
  },
  open: { tone: 'success', title: PRODUCT_NAME, body: `Access granted. Starting ${PRODUCT_NAME}…` },
};

/** The alert line: the host error, then any failed command, joined with `; `. */
export function alertMessage(hostError: string | null | undefined, failures: string[]): string {
  return [hostError, ...failures].filter(Boolean).join('; ');
}
