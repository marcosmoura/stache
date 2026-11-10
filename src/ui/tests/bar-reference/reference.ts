/**
 * Loads the committed Bar legacy reference (Vite `?raw` imports) for browser tests.
 *
 * The reference is data produced by `scripts/bar-reference/extract.ts`; nothing here imports from
 * the new implementation.
 */
import manifestJson from './manifest.json';
import type { ReferenceManifest } from './verify.ts';

const rawFiles = import.meta.glob(['./reference.css', './fixtures/*.json', './icons/*.svg'], {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** Reference output files keyed by their manifest path (for example `icons/SunIcon.svg`). */
export const referenceFiles: Readonly<Record<string, string>> = Object.fromEntries(
  Object.entries(rawFiles).map(([key, content]) => [key.replace(/^\.\//u, ''), content]),
);

export const manifest = manifestJson as unknown as ReferenceManifest;

export const referenceCss: string = referenceFiles['reference.css'];

export interface ReferenceFixture {
  description: string;
  /** Markup the legacy components render when the viewport matches `(width > 2036px)`. */
  wide: string;
  /** Markup when `(width <= 2036px)` matches (compact Stack gap, battery label). */
  compact: string;
}

export const fixtures: Readonly<Record<string, ReferenceFixture>> = JSON.parse(
  referenceFiles['fixtures/fixtures.json'],
) as Record<string, ReferenceFixture>;

/** Harness sources, scanned by the verifier for imports of the new implementation. */
export const harnessSources: Readonly<Record<string, string>> = import.meta.glob(
  ['./reference.ts', './render.ts'],
  { query: '?raw', import: 'default', eager: true },
) as Record<string, string>;
