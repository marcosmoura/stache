import { lazy, memo, Suspense } from 'react';
import { ErrorBoundary } from 'react-error-boundary';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { motion } from 'motion/react';

import { QueryErrorFallback } from '@/components/QueryErrorFallback';
import { queryClientDefaults } from '@/utils/queryClientDefaults';
import { resolveModule } from '@/utils/resolveModule';

import { useWidgets } from './Widgets.state';
import * as styles from './Widgets.styles';

const Calendar = lazy(() => import('./components/Calendar').then(resolveModule('Calendar')));
const Battery = lazy(() => import('./components/Battery').then(resolveModule('Battery')));
const Weather = lazy(() => import('./components/Weather').then(resolveModule('Weather')));

const queryClient = new QueryClient({
  defaultOptions: queryClientDefaults,
});

const WidgetsContent = memo(function WidgetsContent() {
  const { isAnimatingIn, transition, contentRef, activeWidget } = useWidgets();

  return (
    <div className={styles.widgets} ref={contentRef}>
      <motion.div
        className={styles.widget}
        animate={{ y: isAnimatingIn ? 0 : -60, opacity: isAnimatingIn ? 1 : 0 }}
        transition={transition}
      >
        {activeWidget === 'calendar' && <Calendar />}
        {activeWidget === 'battery' && <Battery />}
        {activeWidget === 'weather' && <Weather />}
      </motion.div>
    </div>
  );
});

export function Widgets() {
  return (
    <QueryClientProvider client={queryClient}>
      <ErrorBoundary
        fallback={<QueryErrorFallback />}
        onReset={() => {
          queryClient.resetQueries();
        }}
      >
        <Suspense fallback={null}>
          <WidgetsContent />
        </Suspense>
      </ErrorBoundary>
    </QueryClientProvider>
  );
}
