import { lazy, Suspense } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { motion } from 'motion/react';

import { useWidgets } from './Widgets.state';
import * as styles from './Widgets.styles';

const resolveModule = (moduleName: string) => (module: Record<string, React.ComponentType>) => ({
  default: module[moduleName],
});

const Calendar = lazy(() => import('./components/Calendar').then(resolveModule('Calendar')));
const Battery = lazy(() => import('./components/Battery').then(resolveModule('Battery')));
const Weather = lazy(() => import('./components/Weather').then(resolveModule('Weather')));

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnReconnect: true,
      refetchIntervalInBackground: true,
      retry: true,
    },
  },
});

const WidgetsContent = () => {
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
};

export const Widgets = () => (
  <QueryClientProvider client={queryClient}>
    <Suspense fallback={null}>
      <WidgetsContent />
    </Suspense>
  </QueryClientProvider>
);
