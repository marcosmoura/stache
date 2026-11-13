import '@/main.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App, Providers } from '@/app';
import { WidgetRegistryProvider } from '@/windows/Bar/providers';

import { fixtureRegistry } from '../../tests/fixtureRegistry';

import { BarItemGallery } from './BarItemGallery';

/*
 * Boots the same App as `src/main.tsx`, with the fixture widget registry.
 * Served only by the Vite dev server (`/e2e/harness.html`); the production
 * build has a single `index.html` input and never includes it.
 * `?gallery=bar-item` renders the `BarItem` gallery for screenshot specs instead.
 */
const root = document.getElementById('root');

if (!root) {
  throw new Error('Missing UI root');
}

createRoot(root).render(
  <StrictMode>
    {new URLSearchParams(window.location.search).get('gallery') === 'bar-item' ? (
      <Providers>
        <BarItemGallery />
      </Providers>
    ) : (
      <WidgetRegistryProvider registry={fixtureRegistry}>
        <App />
      </WidgetRegistryProvider>
    )}
  </StrictMode>,
);
