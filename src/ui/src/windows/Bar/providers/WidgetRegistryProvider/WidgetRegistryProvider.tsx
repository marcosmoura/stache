import type { ReactNode } from 'react';

import { WidgetRegistryContext } from '../../widgets/registry';
import type { PartialWidgetRegistry } from '../../widgets/types';

/** Supplies a registry other than the production one (tests, the end-to-end harness). */
export function WidgetRegistryProvider({
  registry,
  children,
}: {
  registry: PartialWidgetRegistry;
  children: ReactNode;
}) {
  return <WidgetRegistryContext value={registry}>{children}</WidgetRegistryContext>;
}
