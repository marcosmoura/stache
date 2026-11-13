/**
 * The production query client for tests that drive a client directly instead of
 * rendering through `Providers`. Feature code under `src/` never imports the
 * app layer; the harness does it on its behalf.
 */
export { createQueryClient } from '@/app/providers/QueryProvider';
