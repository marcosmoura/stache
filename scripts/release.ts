import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

import { ReleaseError, runRelease, type Runner } from './release/pipeline';

const run: Runner = (command, args, options = {}) => {
  const result = spawnSync(command, args, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: options.inherit ? 'inherit' : 'pipe',
  });

  if (result.error) {
    return { status: 1, stdout: '', stderr: result.error.message };
  }

  return { status: result.status ?? 1, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
};

try {
  runRelease({
    root: resolve(import.meta.dirname, '..'),
    run,
    env: process.env,
    log: (line) => console.log(line),
  });
} catch (error) {
  console.error(`release failed: ${error instanceof ReleaseError ? error.message : String(error)}`);
  process.exit(1);
}
