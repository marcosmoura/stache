import type { ReactNode } from 'react';

import { MotionProvider } from '../MotionProvider';
import { QueryProvider } from '../QueryProvider';

/**
 * The app-wide provider composition point. Tests render through the same
 * component. One query client per window.
 */
export function Providers({ children }: { children: ReactNode }) {
  return (
    <QueryProvider>
      <MotionProvider>{children}</MotionProvider>
    </QueryProvider>
  );
}
