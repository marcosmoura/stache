import { Shell } from '../Shell';
import { TestWindow } from '../TestWindow';

import { resolveSurface } from './surface';
import * as styles from './WindowRouter.styles';

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

export type WindowRouterProps = {
  /** Overridable for tests; defaults to the webview query string. */
  search?: string;
};

export function WindowRouter({ search = window.location.search }: WindowRouterProps) {
  const surface = resolveSurface(search);

  if (surface === 'test') return <TestWindow />;
  if (surface === 'shell') return <Shell />;
  return <UnknownSurface />;
}
