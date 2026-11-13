import { Suspense, type ReactNode } from 'react';
import { ErrorBoundary, type FallbackProps } from 'react-error-boundary';

import * as stylex from '@stylexjs/stylex';

import { resolveRoute, type Route } from '@/router';

import { styles } from './App.style';
import { Providers } from './providers';

/** Shown when a window throws while rendering. */
export function AppErrorFallback({ error }: FallbackProps) {
  const message = error instanceof Error ? error.message : String(error);

  return (
    <div {...stylex.props(styles.fallback)} role="alert">
      <h1 {...stylex.props(styles.title)}>Something went wrong</h1>
      <p {...stylex.props(styles.detail)}>{message}</p>
    </div>
  );
}

/** Providers, the top-level error boundary and the suspense boundary around a window. */
export function AppShell({ children }: { children: ReactNode }) {
  return (
    <Providers>
      <ErrorBoundary FallbackComponent={AppErrorFallback}>
        <Suspense fallback={null}>{children}</Suspense>
      </ErrorBoundary>
    </Providers>
  );
}

function RoutedWindow({ route }: { route: Route }) {
  if (route.kind === 'fallback') {
    const { Window } = route;

    return <Window requested={route.requested} />;
  }

  const { Window } = route;

  return <Window surface={route.surface} />;
}

/** The root component: resolves `?surface=` once and renders that window. */
export function App({ search = window.location.search }: { search?: string }) {
  return (
    <AppShell>
      <RoutedWindow route={resolveRoute(search)} />
    </AppShell>
  );
}
