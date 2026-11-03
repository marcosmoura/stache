import type { ComponentType } from 'react';

/**
 * Creates a module resolver for dynamic imports with named exports.
 *
 * This utility is useful when lazy-loading components that are named exports
 * rather than default exports, which is common in barrel files (index.ts).
 *
 * @example
 * ```tsx
 * // Instead of:
 * const Bar = lazy(() => import('./bar').then(m => ({ default: m.Bar })));
 *
 * // Use:
 * const Bar = lazy(() => import('./bar').then(resolveModule('Bar')));
 * ```
 *
 * @param moduleName - The name of the exported component to resolve
 * @returns A function that extracts the named export and wraps it as a default export
 */
export function resolveModule(moduleName: string) {
  return (module: Record<string, ComponentType>): { default: ComponentType } => ({
    default: module[moduleName],
  });
}
