/// <reference types="vitest/config" />
import { defineConfig } from 'vite';

/**
 * Node-environment suite for the root TypeScript tooling under `scripts/`.
 *
 * The UI suite stays browser-only in `vite.config.ts`; this project never loads
 * WebKit and never runs a process query outside its fakes.
 */
export default defineConfig({
  test: {
    root: import.meta.dirname,
    environment: 'node',
    include: ['scripts/**/*.test.ts'],
    exclude: ['**/_legacy/**', '**/.worktrees/**', '**/node_modules/**', '**/target/**'],
  },
});
