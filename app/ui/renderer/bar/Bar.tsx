import { Suspense, memo } from 'react';
import { ErrorBoundary } from 'react-error-boundary';

import { cx } from '@linaria/core';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import { Media } from './Media';
import { Spaces } from './Spaces';
import { Status } from './Status';

import { useBar } from './Bar.state';
import * as styles from './Bar.styles';

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnReconnect: true,
      refetchIntervalInBackground: true,
      retry: true,
    },
  },
});

const BarContent = memo(() => {
  const { menuHidden } = useBar();

  return (
    <div className={cx(styles.bar, menuHidden ? styles.barHidden : '')}>
      <Spaces />
      <Media />
      <Status />
    </div>
  );
});

BarContent.displayName = 'BarContent';

export const Bar = () => (
  <QueryClientProvider client={queryClient}>
    <ErrorBoundary fallback={null}>
      <Suspense fallback={null}>
        <BarContent />
      </Suspense>
    </ErrorBoundary>
  </QueryClientProvider>
);
