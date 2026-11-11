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
 * A step runs cargo when a `run:` command calls `cargo` directly or calls `bun run`, because any
 * package script may build or test the Rust workspace.
 */
const CARGO_RUNNING = /(^|[\s;&|(])(cargo|bun run)\s/;

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

    if (command === '|' || command === '>') {
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
    } else if (/^\s+permissions:/.test(line)) {
      at(index, 'job-level permissions are not allowed; use the top-level block only');
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

  const commands = runCommands(lines);
  const guard = commands.find(({ command }) => command.trim() === GUARD);
  const first = commands.find(
    ({ command }) => command.trim() !== GUARD && CARGO_RUNNING.test(`${command} `),
  );

  if (first) {
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
