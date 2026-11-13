import { availableParallelism } from 'node:os';

import { DEFAULT_NEXT_APP } from '../measure/paths';

import type { BarOptions } from './types';

export class UsageError extends Error {}

export type ParsedBarCommandLine = { kind: 'help' } | { kind: 'run'; options: BarOptions };

export const MINIMUM_LAUNCHES = 5;
export const MINIMUM_WARMUP_SECONDS = 10;
export const MINIMUM_SAMPLE_SECONDS = 60;

/**
 * The busy pre-flight sums per-process CPU, where 100% is one core. A normal
 * idle desktop (WindowServer, audio and HID drivers, an editor, a terminal)
 * already sums to well over half a core, so the limit scales with the machine:
 * 15% of total capacity (150 on 10 cores). Real background work (builds, test
 * suites, encoders) still trips it.
 */
export const BUSY_CPU_SHARE_OF_CAPACITY = 0.15;

/** The default `--max-busy-cpu` for a machine with `cores` logical CPUs. */
export function defaultMaxBusyCpuPercent(cores: number = availableParallelism()): number {
  return Math.max(30, Math.round(Math.max(1, cores) * 100 * BUSY_CPU_SHARE_OF_CAPACITY));
}

const DEFAULT_MAX_BUSY_CPU_PERCENT = defaultMaxBusyCpuPercent();
const DEFAULT_PREFLIGHT_SECONDS = 10;

/** Parses the command line without touching the machine; `--help` reads nothing. */
export function parseBarCommandLine(args: string[]): ParsedBarCommandLine {
  const options: BarOptions = {
    appPath: DEFAULT_NEXT_APP,
    launchesPerArm: MINIMUM_LAUNCHES,
    warmupSeconds: MINIMUM_WARMUP_SECONDS,
    sampleSeconds: MINIMUM_SAMPLE_SECONDS,
    height: 28,
    padding: 12,
    ssid: 'default',
    maxBusyCpuPercent: DEFAULT_MAX_BUSY_CPU_PERCENT,
    preflightSeconds: DEFAULT_PREFLIGHT_SECONDS,
  };

  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];

    switch (flag) {
      case '--help':
      case '-h':
        return { kind: 'help' };
      case '--ssid-off':
        options.ssid = 'off';
        break;
      case '--app':
        options.appPath = requireValue(args, index, flag);
        index += 1;
        break;
      case '--launches':
        options.launchesPerArm = parseInteger(
          requireValue(args, index, flag),
          flag,
          MINIMUM_LAUNCHES,
        );
        index += 1;
        break;
      case '--warmup-seconds':
        options.warmupSeconds = parseInteger(
          requireValue(args, index, flag),
          flag,
          MINIMUM_WARMUP_SECONDS,
        );
        index += 1;
        break;
      case '--sample-seconds':
        options.sampleSeconds = parseInteger(
          requireValue(args, index, flag),
          flag,
          MINIMUM_SAMPLE_SECONDS,
        );
        index += 1;
        break;
      case '--height':
        options.height = parseInteger(requireValue(args, index, flag), flag, 1);
        index += 1;
        break;
      case '--padding':
        options.padding = parseInteger(requireValue(args, index, flag), flag, 0);
        index += 1;
        break;
      case '--max-busy-cpu':
        options.maxBusyCpuPercent = parseInteger(requireValue(args, index, flag), flag, 1);
        index += 1;
        break;
      default:
        throw new UsageError(`unknown argument: ${flag}`);
    }
  }

  return { kind: 'run', options };
}

function requireValue(args: string[], index: number, flag: string): string {
  const value = args[index + 1];

  if (value === undefined || value.startsWith('--')) {
    throw new UsageError(`${flag} requires a value`);
  }

  return value;
}

function parseInteger(value: string, flag: string, minimum: number): number {
  if (!/^\d+$/.test(value)) {
    throw new UsageError(`${flag} expects a whole number, got ${value}`);
  }

  const parsed = Number.parseInt(value, 10);

  if (parsed < minimum) {
    throw new UsageError(`${flag} must be at least ${minimum}`);
  }

  return parsed;
}

export function helpText(): string {
  return [
    'Measure the Stache Bar cost: Bar off versus Bar on, release .app only.',
    '',
    'Usage:',
    '  bun run measure:bar -- [options]',
    '',
    'Options:',
    '  --app <path>            Release Stache .app bundle (a debug build is refused).',
    `                          Default: ${DEFAULT_NEXT_APP}`,
    `  --launches <count>      Valid launches per arm (default and minimum: ${MINIMUM_LAUNCHES}).`,
    `  --warmup-seconds <sec>  Wait after readiness before sampling (default and minimum: ${MINIMUM_WARMUP_SECONDS}).`,
    `  --sample-seconds <sec>  Sampling window per launch (default and minimum: ${MINIMUM_SAMPLE_SECONDS}).`,
    '  --height <points>       Bar height written to both arms (default: 28).',
    '  --padding <points>      Bar padding written to both arms (default: 12).',
    '  --ssid-off              Run the labelled "SSID off" comparison series',
    '                          (STACHE_BAR_WIFI_SSID_SUMMARY=0 in the child).',
    '  --max-busy-cpu <pct>    Refuse to start above this whole-machine CPU percent',
    `                          (sum over processes, 100 = one core; default: 15% of all cores,`,
    `                          ${DEFAULT_MAX_BUSY_CPU_PERCENT} on this machine).`,
    '  --help, -h              Print this help without reading process state.',
    '',
    'Preconditions (the tool checks what it can and aborts a trial as invalid otherwise):',
    '  - A release bundle built with `bun run tauri:build`. Never a debug build.',
    '  - THE ACCESSIBILITY LAUNCH GATE: every module, Bar included, is held unless the',
    '    exact .app path being measured is trusted for Accessibility. Grant it for that',
    '    path in System Settings > Privacy & Security > Accessibility before running.',
    '    Without it the "on" arm would measure a gated host; the tool reports that',
    '    trial as invalid (never a sample) and stops.',
    '  - No Stache process running. The tool refuses and',
    '    never terminates another instance.',
    '  - An otherwise idle machine on stable power; a busy-machine pre-flight refuses.',
    '  - One measurement at a time; do not run another measurement tool alongside.',
    '',
    'Output: docs/specs/2026-11-03-bar/measurements/<UTC timestamp>/report.md and samples.json',
    '',
    'Exact command:',
    '  bun run measure:bar -- --app "target/release/bundle/macos/Stache.app"',
    '',
  ].join('\n');
}
