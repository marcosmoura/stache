import { DEFAULT_NEXT_APP, MEASUREMENTS_DIRECTORY, REPOSITORY_ROOT } from './paths';
import type { MeasureOptions } from './types';

export class UsageError extends Error {}

export type ParsedCommandLine = { kind: 'help' } | { kind: 'run'; options: MeasureOptions };

const MINIMUM_LAUNCHES = 5;

/**
 * Parses the measurement command line without touching the machine.
 *
 * `--help` is handled before any target, process, or audio inspection so that
 * the documented safe command can be checked while another Stache instance is
 * running.
 */
export function parseCommandLine(args: string[]): ParsedCommandLine {
  let newAppPath = DEFAULT_NEXT_APP;
  let legacyAppPath: string | null = null;
  let launches = MINIMUM_LAUNCHES;
  let idleSeconds = 60;

  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    switch (flag) {
      case '--help':
      case '-h':
        return { kind: 'help' };
      case '--new-app':
        newAppPath = requireValue(args, index, flag);
        index += 1;
        break;
      case '--legacy-app':
        legacyAppPath = requireValue(args, index, flag);
        index += 1;
        break;
      case '--launches':
        launches = parseInteger(requireValue(args, index, flag), flag, MINIMUM_LAUNCHES);
        index += 1;
        break;
      case '--idle-seconds':
        idleSeconds = parseInteger(requireValue(args, index, flag), flag, 1);
        index += 1;
        break;
      default:
        throw new UsageError(`unknown argument: ${flag}`);
    }
  }

  return {
    kind: 'run',
    options: { newAppPath, legacyAppPath, launches, idleSeconds },
  };
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
    'Measure release startup, idle cost, and process trees for Stache Next.',
    '',
    'Usage:',
    '  bun run measure -- [options]',
    '',
    'Options:',
    '  --new-app <path>       Stache Next release .app bundle or executable.',
    `                         Default: ${DEFAULT_NEXT_APP}`,
    '  --legacy-app <path>    Optional legacy release .app bundle or executable.',
    '                         Requires no other legacy Stache process to be running.',
    '  --launches <count>     Timed launches per scenario (default and minimum: 5).',
    '  --idle-seconds <sec>   Idle sampling window per scenario (default: 60).',
    '  --help, -h             Print this help without reading process or audio state.',
    '',
    'Prerequisites:',
    '  - macOS 26 with Xcode command-line tools and the repository toolchain.',
    '  - New host built with: bun run tauri:build',
    '  - Legacy comparison, when requested, built from _legacy/ per _legacy/README.md.',
    '  - No Stache Next process running. A running legacy Stache (for example',
    '    /Applications/Stache.app) blocks only --legacy-app runs; otherwise keep its',
    '    proxyAudio disabled. This tool refuses and never terminates another instance.',
    '  - AC power recommended; the report records the actual power source.',
    '  - Do not run a manual audio-device test at the same time.',
    '  - Run one measurement at a time; two tool instances are not mutually excluded.',
    '',
    'Safety guarantees:',
    '  - Every child gets an isolated HOME, TMPDIR and XDG_CONFIG_HOME under a',
    '    short temporary root; real ~/.config/stache is never read or written.',
    '  - At most one measured process tree exists at a time; the next launch waits',
    '    for the previous tree to exit completely.',
    '  - SIGTERM is sent only to the process this tool spawned; escalation uses the',
    '    recorded owned PIDs and only after re-checking their executable path.',
    '  - Temporary roots are removed on success, error and SIGINT, unless a measured',
    '    process tree survives; that root is kept and the run fails.',
    '  - A failed or interrupted run still writes the report with partial data.',
    '  - Default audio devices are captured, protected by inert pilot config and',
    '    restored when a change is still detected.',
    '',
    'Output:',
    `  ${MEASUREMENTS_DIRECTORY}/<UTC timestamp>/report.md and samples.json`,
    '',
    'Exact guarded command:',
    `  bun run measure -- \\`,
    `    --new-app "${DEFAULT_NEXT_APP}" \\`,
    '    --legacy-app "/path/to/legacy/Stache.app"',
    '',
    `Repository: ${REPOSITORY_ROOT}`,
    '',
  ].join('\n');
}
