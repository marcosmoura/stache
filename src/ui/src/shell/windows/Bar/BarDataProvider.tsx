import type { ReactNode } from 'react';

import { BarDataContext, type BarData } from './Bar.data';

export type BarDataProviderProps = {
  data: BarData;
  children: ReactNode;
};

/** Supplies Bar data; the production Bar uses the absent default. */
export function BarDataProvider({ data, children }: BarDataProviderProps) {
  return <BarDataContext value={data}>{children}</BarDataContext>;
}
