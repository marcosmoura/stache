import { fileURLToPath } from 'node:url';

/** Repository root, resolved from this file's location (`scripts/measure/`). */
export const REPOSITORY_ROOT = fileURLToPath(new URL('../..', import.meta.url)).replace(/\/+$/, '');

/** Bundle identifier of the new host. */
export const NEXT_BUNDLE_IDENTIFIER = 'com.marcosmoura.stache.next';

/** Bundle identifier of the archived legacy application. */
export const LEGACY_BUNDLE_IDENTIFIER = 'com.marcosmoura.stache';

/** Default release bundle built by `bun run tauri:build`. */
export const DEFAULT_NEXT_APP = 'target/release/bundle/macos/Stache Next.app';

/** Directory that receives timestamped reports and raw samples. */
export const MEASUREMENTS_DIRECTORY = 'docs/specs/2026-11-02-foundation/measurements';

/** Provisional target for the instrumented host tray-created measurement. */
export const TARGET_TRAY_READY_MS = 300;

/**
 * Interpretation of the provisional "≈0% idle CPU" target.
 *
 * The spec deliberately sets no numeric budget. This threshold only separates
 * the measured average from sampling noise; the raw value is always reported.
 */
export const NEAR_ZERO_CPU_PERCENT = 1;

/** Resolution of `ps` cumulative CPU time on macOS, in seconds. */
export const PS_CPU_TIME_RESOLUTION_SECONDS = 0.01;

/** Interval between idle process-tree snapshots. */
export const IDLE_SAMPLE_INTERVAL_MS = 5_000;

/** Wait after the last launch becomes ready before idle sampling starts. */
export const IDLE_WARMUP_SECONDS = 10;

/** Maximum time to wait for a target to acknowledge readiness. */
export const READY_TIMEOUT_MS = 30_000;

/** Maximum time to wait for a graceful SIGTERM exit. */
export const SHUTDOWN_TIMEOUT_MS = 10_000;
