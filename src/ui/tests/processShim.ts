/*
 * Motion's development checks (`invariant`, used by `LazyMotion strict`) are
 * guarded by `typeof process !== 'undefined'`, which is false in a browser, so
 * they never run there. Defining a development `process.env` before Motion
 * loads turns them on in tests. Runs before `setup.ts` (first `setupFiles` entry).
 */
const target = globalThis as { process?: { env: Record<string, string | undefined> } };

target.process ??= { env: { NODE_ENV: 'development' } };
