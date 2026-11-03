export const SURFACES = ['shell', 'test'] as const;

export type Surface = (typeof SURFACES)[number];

/**
 * Resolves the `surface` query value. A missing value falls back to the plain
 * shell; an unrecognized value is rejected rather than rendered as a test
 * surface.
 */
export function resolveSurface(search: string): Surface | null {
  const value = new URLSearchParams(search).get('surface');
  if (value === null) return 'shell';
  return SURFACES.find((surface) => surface === value) ?? null;
}
