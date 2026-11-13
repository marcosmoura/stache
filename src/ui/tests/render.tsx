import type { ReactNode } from 'react';

import { render as renderInBrowser } from 'vitest-browser-react';

import { Providers } from '@/app';

/** Renders a component inside the app providers. */
export function render(ui: ReactNode) {
  return renderInBrowser(ui, { wrapper: Providers });
}
