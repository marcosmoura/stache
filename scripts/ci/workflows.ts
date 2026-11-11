/**
 * Line-based checks for the CI rules in `.github/workflows/*.yml` and `*.yaml`.
 *
 * It reads workflows written the way this repository writes them; it is not a YAML parser.
 */
import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';

export interface WorkflowFile {
  file: string;
  text: string;
}

const USES = /^(\s*)(?:-\s+)?uses:\s*(\S+)(.*)$/;
const PINNED = /^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/;
const VERSION_COMMENT = /^\s+# v\d+\.\d+\.\d+\s*$/;
const RUN = /^(\s*)(?:-\s+)?run:\s*(.*)$/;
const GUARD = 'cargo fetch --locked';

/**
 * A step runs cargo when a `run:` command calls `cargo` or the Tauri CLI directly, calls `bunx` or
 * `npx`, or calls `bun <script>`, because any package or script may build or test the Rust
 * workspace. Only the Bun built-ins that cannot build Rust (`install`, `audit`, `pm`, `--version`,
 * `-v`) are exempt.
 */
const CARGO_RUNNING =
  /(?:^|[\s;&|('"])(?:(?:(?:\S*\/)?cargo|bunx|npx|\S*node_modules\/\.bin\/tauri)(?=[\s'"]|$)|bun\s+(?!(?:install|audit|pm|--version|-v)(?:[\s'"]|$))\S)/m;

function indentOf(line: string): number {
  return line.length - line.trimStart().length;
}

/** Lines of the step item that holds line `at`: its `- ` line and everything nested in it. */
function stepLines(lines: string[], at: number): number[] {
  const dash = /^(\s*)-\s/.exec(lines[at]);
  // A key on a continuation line (`- name:` first, `uses:` after) sits two columns right of the dash.
  const own = dash ? dash[1].length : indentOf(lines[at]) - 2;
  let start = at;

  while (start > 0 && (lines[start].trim() === '' || indentOf(lines[start]) !== own)) {
    start -= 1;
  }

  const result = [start];

  for (let index = start + 1; index < lines.length; index += 1) {
    const line = lines[index];

    if (line.trim() === '') {
      continue;
    }

    if (indentOf(line) <= own) {
      break;
    }

    result.push(index);
  }

  return result;
}

/** Index of the job each line belongs to: the keys directly under `jobs:`; -1 outside a job. */
function jobOf(lines: string[]): number[] {
  const jobs = Array.from({ length: lines.length }, () => -1);
  const top = lines.findIndex((line) => /^jobs:\s*(#.*)?$/.test(line));

  if (top < 0) {
    return jobs;
  }

  let keyIndent = -1;
  let current = -1;

  for (let index = top + 1; index < lines.length; index += 1) {
    const line = lines[index];

    if (line.trim() === '' || line.trimStart().startsWith('#')) {
      jobs[index] = current;
      continue;
    }

    if (indentOf(line) === 0) {
      break;
    }

    if (keyIndent < 0) {
      keyIndent = indentOf(line);
    }

    if (indentOf(line) === keyIndent && /^\s*[\w-]+:/.test(line)) {
      current = index;
    }

    jobs[index] = current;
  }

  return jobs;
}

/** Lines that open a step: the list items directly under a `steps:` key. */
function stepStarts(lines: string[]): Set<number> {
  const starts = new Set<number>();

  lines.forEach((line, index) => {
    if (!/^\s*steps:\s*(#.*)?$/.test(line)) {
      return;
    }

    const own = indentOf(line);
    let item = -1;

    for (let next = index + 1; next < lines.length; next += 1) {
      const candidate = lines[next];
      const isItem = candidate.trimStart().startsWith('-');

      if (candidate.trim() === '' || candidate.trimStart().startsWith('#')) {
        continue;
      }

      if (indentOf(candidate) < own || (indentOf(candidate) === own && !isItem)) {
        break;
      }

      if (item < 0) {
        item = indentOf(candidate);
      }

      if (isItem && indentOf(candidate) === item) {
        starts.add(next);
      }
    }
  });

  return starts;
}

/** Commands of each `run:`, with the line they start on; block scalars are joined. */
function runCommands(lines: string[]): { line: number; command: string }[] {
  const commands: { line: number; command: string }[] = [];

  for (let index = 0; index < lines.length; index += 1) {
    const match = RUN.exec(lines[index]);

    if (!match) {
      continue;
    }

    const start = index;
    const own = indentOf(lines[index]);
    let command = match[2];

    if (command.startsWith('|') || command.startsWith('>')) {
      const body: string[] = [];

      while (index + 1 < lines.length) {
        const next = lines[index + 1];

        if (next.trim() !== '' && indentOf(next) <= own) {
          break;
        }

        body.push(next.trim());
        index += 1;
      }

      command = body.join('\n');
    }

    commands.push({ line: start, command });
  }

  return commands;
}

export function checkWorkflow({ file, text }: WorkflowFile): string[] {
  const lines = text.split('\n');
  const errors: string[] = [];
  const at = (index: number, message: string) => errors.push(`${file}:${index + 1}: ${message}`);
  const steps = stepStarts(lines);
  let topPermissions = -1;

  lines.forEach((line, index) => {
    const uses = USES.exec(line);

    if (uses) {
      if (!PINNED.test(uses[2])) {
        at(index, `uses "${uses[2]}" is not pinned to a 40-character commit SHA`);
      }

      if (!VERSION_COMMENT.test(uses[3])) {
        at(index, `uses "${uses[2]}" lacks a "# vX.Y.Z" version comment`);
      }

      if (uses[2].startsWith('actions/checkout@')) {
        const step = stepLines(lines, index);

        if (!step.some((i) => /^\s*persist-credentials:\s*false\s*$/.test(lines[i]))) {
          at(index, 'checkout step lacks "persist-credentials: false"');
        }
      }
    }

    if (steps.has(index) && /^\s*-\s*\{/.test(line)) {
      at(index, 'flow-style steps ("- { ... }") are not supported; write the step as a block');
    }

    const tool = /^\s*tool:\s*(.+)$/.exec(line);

    if (tool) {
      for (const name of tool[1].split(',').map((entry) => entry.trim())) {
        if (!/^[\w-]+@\d+\.\d+\.\d+$/.test(name)) {
          at(index, `tool "${name}" lacks an exact @<version>`);
        }
      }
    }

    if (/^permissions:/.test(line)) {
      topPermissions = index;
    }
  });

  if (topPermissions < 0) {
    at(0, 'top-level "permissions:" block is missing');
  } else {
    const grants: number[] = [];

    for (
      let index = topPermissions + 1;
      index < lines.length && /^\s+\S/.test(lines[index]);
      index += 1
    ) {
      grants.push(index);
    }

    if (lines[topPermissions].trim() !== 'permissions:') {
      at(topPermissions, 'top-level permissions must be a block granting only "contents: read"');
    } else if (grants.length === 0) {
      at(topPermissions, 'top-level permissions grant nothing; expected "contents: read"');
    }

    for (const index of grants) {
      if (lines[index].trim() !== 'contents: read') {
        at(
          index,
          `top-level permissions grant "${lines[index].trim()}"; only "contents: read" is allowed`,
        );
      }
    }
  }

  const jobs = jobOf(lines);
  const commands = runCommands(lines);

  for (const job of new Set(commands.map(({ line }) => jobs[line]))) {
    const own = commands.filter(({ line }) => jobs[line] === job);
    const guard = own.find(({ command }) => command.trim() === GUARD);
    const first = own.find(
      ({ command }) => command.trim() !== GUARD && CARGO_RUNNING.test(command),
    );

    if (!first) {
      continue;
    }

    if (!guard) {
      at(first.line, `no "${GUARD}" step before this cargo-running step`);
    } else if (first.line < guard.line) {
      at(
        first.line,
        `cargo-running step comes before the "${GUARD}" step on line ${guard.line + 1}`,
      );
    }
  }

  return errors;
}

export function readWorkflows(root: string): WorkflowFile[] {
  const directory = path.join(root, '.github', 'workflows');

  return readdirSync(directory)
    .filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
    .sort()
    .map((name) => ({
      file: path.join('.github', 'workflows', name),
      text: readFileSync(path.join(directory, name), 'utf8'),
    }));
}

export function checkWorkflows(files: WorkflowFile[]): string[] {
  return files.flatMap(checkWorkflow);
}
