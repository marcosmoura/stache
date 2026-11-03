import { spawnSync } from 'node:child_process';
import path from 'node:path';

/** Paths outside the active tree, even when force-staged: archive, worktrees, builds, generated output. */
const ignoredDirectories = [
  '.worktrees',
  '_legacy',
  'coverage',
  'node_modules',
  'src/crates/app/gen',
  'src/ui/coverage',
  'src/ui/dist',
  'src/ui/test-results',
  'target',
  'test-results',
];

const ignoredFiles = ['bun.lock', 'Cargo.lock', 'src/ui/src/sdk/bindings.gen.ts'];

const formattedExtensions = [
  '.js',
  '.mjs',
  '.cjs',
  '.ts',
  '.tsx',
  '.json',
  '.jsonc',
  '.css',
  '.md',
  '.yaml',
  '.yml',
];

const scriptExtensions = ['.js', '.mjs', '.cjs', '.ts', '.tsx'];

const isActiveFile = (file: string) =>
  !ignoredDirectories.some((directory) => file.startsWith(`${directory}/`)) &&
  !ignoredFiles.includes(file);

const isRustInput = (file: string) => {
  const isRustFile = file.endsWith('.rs');
  const isCargoFile = path.basename(file) === 'Cargo.toml';
  const isRustToolchainFile = ['rust-toolchain.toml', 'rustfmt.toml'].includes(file);

  return isRustFile || isCargoFile || isRustToolchainFile;
};

const run = (command: string, args: string[]) => {
  const result = spawnSync(command, args, { stdio: 'inherit' });

  if (result.error) {
    throw result.error;
  }

  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.status})`);
  }
};

export default {
  '*': {
    title: 'Check active staged files',
    // A function task is needed: lint-staged globs cannot combine these extension sets with the
    // nested exclusions, and `tsc -b` must run without filenames. Passing argv arrays keeps names
    // with spaces or apostrophes literal.
    task: (stagedFiles: string[]) => {
      const files = stagedFiles
        .map((file) => path.relative(import.meta.dirname, file))
        .filter(isActiveFile);
      const formatted = files.filter((file) => formattedExtensions.includes(path.extname(file)));
      const scripts = files.filter((file) => scriptExtensions.includes(path.extname(file)));
      const styles = files.filter((file) => file.endsWith('.styles.tsx') || file.endsWith('.css'));

      if (formatted.length) {
        run('oxfmt', ['--check', '--disable-nested-config', '--', ...formatted]);
      }

      if (scripts.length) {
        run('oxlint', ['--deny-warnings', '--', ...scripts]);
        run('tsc', ['-b']);
      }

      if (styles.length) {
        run('stylelint', ['--', ...styles]);
      }

      if (files.some(isRustInput)) {
        run('cargo', ['fmt', '--all', '--', '--check']);
        run('bun', ['run', 'lint:native']);
      }
    },
  },
};
