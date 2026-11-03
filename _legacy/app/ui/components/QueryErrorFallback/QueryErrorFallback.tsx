import { useErrorBoundary } from 'react-error-boundary';

import { Button } from '@/components/Button';

import * as styles from './QueryErrorFallback.styles';

/**
 * Compact, retry-capable fallback for transient query errors.
 *
 * Rendered by an ErrorBoundary when a Suspense query rejects (e.g. a Tauri
 * command failing during a transient state). The Retry button resets the
 * boundary and refetches the underlying queries instead of leaving the
 * window permanently blank.
 */
export function QueryErrorFallback() {
  const { resetBoundary } = useErrorBoundary();

  return (
    <div className={styles.fallback} role="alert">
      <span className={styles.message}>Something went wrong</span>
      <Button type="button" className={styles.retry} onClick={resetBoundary} aria-label="Retry">
        Retry
      </Button>
    </div>
  );
}
