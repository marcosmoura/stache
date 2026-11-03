import { Suspense, memo } from 'react';
import { ErrorBoundary } from 'react-error-boundary';

import { cx } from '@linaria/core';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import { QueryErrorFallback } from '@/components/QueryErrorFallback';
import { queryClientDefaults } from '@/utils/queryClientDefaults';

import { useBar } from './Bar.state';
import * as styles from './Bar.styles';
import { Media } from './Media';
import { Spaces } from './Spaces';
import { Status } from './Status';

const queryClient = new QueryClient({
  defaultOptions: queryClientDefaults,
});

const BarContent = memo(function BarContent() {
  const { menuHidden } = useBar();

  return (
    <div className={cx(styles.bar, menuHidden && styles.barHidden)}>
      <Spaces />
      <Media />
      <Status />
    </div>
  );
});

export function Bar() {
  return (
    <QueryClientProvider client={queryClient}>
      <ErrorBoundary
        fallback={<QueryErrorFallback />}
        onReset={() => {
          queryClient.resetQueries();
        }}
      >
        <Suspense fallback={null}>
          <BarContent />
        </Suspense>
      </ErrorBoundary>
    </QueryClientProvider>
  );
}
