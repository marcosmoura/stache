import type { ComponentType } from 'react';

import * as styles from './Shell.styles';
import { AccessibilityGuide } from './windows/AccessibilityGuide';
import { Placeholder } from './windows/Placeholder';
import { TestWindow } from './windows/TestWindow';

/** Every window surface the shell provides, keyed by its `?surface=` value. */
const WINDOWS = {
  shell: Placeholder,
  test: TestWindow,
  access: AccessibilityGuide,
} satisfies Record<string, ComponentType>;

type Surface = keyof typeof WINDOWS;

/**
 * Resolves the `surface` query value. A missing value falls back to the plain
 * shell; an unrecognized value is rejected rather than rendered as a test
 * surface.
 */
function resolveSurface(search: string): Surface | null {
  const value = new URLSearchParams(search).get('surface');

  if (value === null) {
    return 'shell';
  }

  return Object.hasOwn(WINDOWS, value) ? (value as Surface) : null;
}

function UnknownSurface() {
  return (
    <main className={styles.page}>
      <div className={styles.content}>
        <h1 className={styles.title}>Unknown surface</h1>
        <p className={styles.body}>
          This window requested a surface that the shell does not provide.
        </p>
      </div>
    </main>
  );
}

export type ShellProps = {
  /** Overridable for tests; defaults to the webview query string. */
  search?: string;
};

export function Shell({ search = window.location.search }: ShellProps) {
  const surface = resolveSurface(search);

  if (surface === null) {
    return <UnknownSurface />;
  }

  const Window = WINDOWS[surface];

  return <Window />;
}
