import type { ReactNode } from 'react';
import { useState } from 'react';

import { QueryClientProvider } from '@tanstack/react-query';

import { createQueryClient } from './client';

/** One query client per window (per mounted provider). */
export function QueryProvider({ children }: { children: ReactNode }) {
  const [client] = useState(createQueryClient);

  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}
