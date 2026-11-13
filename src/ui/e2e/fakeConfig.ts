import { readFileSync } from 'node:fs';
import path from 'node:path';

import type {
  ConfigDocument_Serialize,
  ConfigEditOp,
  ConfigEditOutcome_Serialize,
  ConfigFileView_Serialize,
  JsonValue,
} from '../src/ipc/bindings.gen';

import type { FakeHost } from './fakeHost';

/*
 * An in-memory configuration store for end-to-end scenarios. Each file keeps
 * real TOML text; `config_document` parses it, `config_edit` changes only the
 * touched lines (so comments survive) and `config-changed` follows every
 * commit and every simulated outside save. The parser and the edits cover the
 * TOML the scenarios use (comments, `[table]`, `[[array]]`, one-line values),
 * not the whole language: this is a fake, not the host's `toml_edit` edit.
 */

type Json = JsonValue;
type Obj = { [key: string]: Json };

const SCHEMA_DIR = path.resolve(import.meta.dirname, '../../../schemas');

class TomlError extends Error {
  readonly line: number;
  readonly column: number;

  constructor(message: string, line: number, column: number) {
    super(message);
    this.line = line;
    this.column = column;
  }
}

/** One-line TOML value parser. */
function parseValue(source: string, line: number): { value: Json; rest: string } {
  let index = 0;
  const fail = (message: string): never => {
    throw new TomlError(message, line, index + 1);
  };
  const space = () => {
    while (source[index] === ' ' || source[index] === '\t') {
      index += 1;
    }
  };
  const key = (): string => {
    space();

    if (source[index] === '"') {
      return value() as string;
    }

    const match = /^[A-Za-z0-9_-]+/.exec(source.slice(index));

    if (!match) {
      fail('expected a key');
    }

    index += match![0].length;

    return match![0];
  };
  const value = (): Json => {
    space();
    const char = source[index];

    if (char === '"') {
      const match = /^"(?:[^"\\]|\\.)*"/.exec(source.slice(index));

      if (!match) {
        fail('unterminated string');
      }

      index += match![0].length;

      return JSON.parse(match![0]) as string;
    }

    if (char === '[') {
      index += 1;
      const items: Json[] = [];

      space();

      while (source[index] !== ']') {
        items.push(value());
        space();

        if (source[index] === ',') {
          index += 1;
          space();
        } else if (source[index] !== ']') {
          fail('expected `,` or `]`');
        }
      }

      index += 1;

      return items;
    }

    if (char === '{') {
      index += 1;
      const table: Obj = {};

      space();

      while (source[index] !== '}') {
        const name = key();

        space();

        if (source[index] !== '=') {
          fail('expected `=`');
        }

        index += 1;
        table[name] = value();
        space();

        if (source[index] === ',') {
          index += 1;
          space();
        } else if (source[index] !== '}') {
          fail('expected `,` or `}`');
        }
      }

      index += 1;

      return table;
    }

    const match = /^(true|false|-?\d+(?:\.\d+)?)/.exec(source.slice(index));

    if (!match) {
      return fail('invalid value');
    }

    index += match[0].length;

    return match[0] === 'true' ? true : match[0] === 'false' ? false : Number(match[0]);
  };

  const parsed = value();

  space();

  return { value: parsed, rest: source.slice(index) };
}

type Segment = string | number;

type KeyLine = { line: number; path: Segment[]; key: string };
type Header = { line: number; path: Segment[]; array: boolean };

type Parsed = { value: Obj; keys: KeyLine[]; headers: Header[]; lines: string[] };

const KEY_LINE = /^\s*("(?:[^"\\]|\\.)*"|[A-Za-z0-9_-]+)\s*=\s*(.*)$/;

function parseToml(text: string): Parsed {
  const lines = text.split('\n');
  const value: Obj = {};
  const keys: KeyLine[] = [];
  const headers: Header[] = [];
  let table: Segment[] = [];

  const container = (at: Segment[]): Obj => {
    let current: Json = value;

    for (const segment of at) {
      current = (current as Record<Segment, Json>)[segment];
    }

    return current as Obj;
  };

  lines.forEach((raw, index) => {
    const line = raw.trim();
    const number = index + 1;

    if (line === '' || line.startsWith('#')) {
      return;
    }

    const header = /^(\[\[?)([A-Za-z0-9_.-]+)(\]\]?)\s*(#.*)?$/.exec(line);

    if (header) {
      const names = header[2].split('.');
      const array = header[1] === '[[';

      if (array !== (header[3] === ']]')) {
        throw new TomlError('mismatched brackets', number, 1);
      }

      let current: Obj = value;

      names.slice(0, -1).forEach((name) => {
        current = (current[name] ??= {}) as Obj;
      });

      const last = names.at(-1)!;

      if (array) {
        const list = (current[last] ??= []) as Json[];

        list.push({});
        table = [...names, list.length - 1];
      } else {
        current[last] ??= {};
        table = names;
      }

      headers.push({ line: index, path: table, array });

      return;
    }

    const match = KEY_LINE.exec(raw);

    if (!match) {
      throw new TomlError('expected `=` after a key', number, raw.length + 1);
    }

    const key = match[1].startsWith('"') ? (JSON.parse(match[1]) as string) : match[1];
    const { value: parsed, rest } = parseValue(match[2], number);

    if (rest !== '' && !rest.startsWith('#')) {
      throw new TomlError('unexpected text after a value', number, raw.length - rest.length + 1);
    }

    container(table)[key] = parsed;
    keys.push({ line: index, path: [...table, key], key: match[1] });
  });

  return { value, keys, headers, lines };
}

const renderKey = (key: string) => (/^[A-Za-z0-9_-]+$/.test(key) ? key : JSON.stringify(key));

function render(value: Json): string {
  if (typeof value === 'string') {
    return JSON.stringify(value);
  }

  if (Array.isArray(value)) {
    return `[${value.map(render).join(', ')}]`;
  }

  if (value !== null && typeof value === 'object') {
    return `{ ${Object.entries(value)
      .map(([key, item]) => `${renderKey(key)} = ${render(item)}`)
      .join(', ')} }`;
  }

  return String(value);
}

const same = (a: Segment[], b: Segment[]) =>
  a.length === b.length && a.every((segment, index) => segment === b[index]);

/** The line span of each `[[path]]` element, with the comment lines directly above it. */
function blocks(parsed: Parsed, at: Segment[]) {
  const elements = parsed.headers.filter(
    (header) => header.array && same(header.path.slice(0, -1), at),
  );

  return elements.map((header) => {
    let start = header.line;

    while (start > 0 && parsed.lines[start - 1].trim().startsWith('#')) {
      start -= 1;
    }

    const next = parsed.headers.find((other) => other.line > header.line);
    let end = next ? next.line : parsed.lines.length;

    // Comments right above the next header belong to it; so do the blank lines before them.
    while (end > header.line + 1 && parsed.lines[end - 1].trim().startsWith('#')) {
      end -= 1;
    }

    while (end > header.line + 1 && parsed.lines[end - 1].trim() === '') {
      end -= 1;
    }

    return { start, end };
  });
}

function blockText(at: Segment[], value: Json) {
  const name = at.join('.');
  const body = Object.entries(value as Obj).map(
    ([key, item]) => `${renderKey(key)} = ${render(item)}`,
  );

  return [`[[${name}]]`, ...body];
}

/** Applies one op to the text. Returns the new text or throws a path error. */
function applyEdit(text: string, op: ConfigEditOp): string {
  const parsed = parseToml(text);
  const lines = [...parsed.lines];
  const keyAt = (at: Segment[]) => parsed.keys.find((key) => same(key.path, at));
  const pathError = (message: string): never => {
    throw new Error(message);
  };

  switch (op.op) {
    case 'set': {
      const existing = keyAt(op.path);

      if (existing) {
        const match = KEY_LINE.exec(lines[existing.line])!;
        const { rest } = parseValue(match[2], existing.line + 1);
        const indent = /^\s*/.exec(lines[existing.line])![0];

        lines[existing.line] =
          `${indent}${match[1]} = ${render(op.value)}${rest ? ` ${rest}` : ''}`;

        return lines.join('\n');
      }

      const parent = op.path.slice(0, -1);
      const key = String(op.path.at(-1));
      const entry = `${renderKey(key)} = ${render(op.value)}`;
      const header = parsed.headers.find((candidate) => same(candidate.path, parent));

      if (parent.length > 0 && !header) {
        if (parent.some((segment) => typeof segment === 'number')) {
          return pathError('missing array element');
        }

        return `${text.replace(/\n*$/, '')}\n\n[${parent.join('.')}]\n${entry}\n`;
      }

      const owned = parsed.keys.filter((candidate) => same(candidate.path.slice(0, -1), parent));
      const after = owned.at(-1)?.line ?? header?.line ?? -1;
      const at = after >= 0 ? after + 1 : (parsed.headers[0]?.line ?? lines.length);

      lines.splice(at, 0, entry);

      return lines.join('\n');
    }
    case 'remove': {
      const existing = keyAt(op.path);

      if (existing) {
        lines.splice(existing.line, 1);

        return lines.join('\n');
      }

      const index = op.path.at(-1);

      if (typeof index === 'number') {
        const block = blocks(parsed, op.path.slice(0, -1))[index] ?? pathError('no element');

        lines.splice(block.start, block.end - block.start);

        return lines.join('\n');
      }

      return text;
    }
    case 'insert': {
      const inline = keyAt(op.path);

      if (inline) {
        let found: Json = parsed.value;

        for (const segment of op.path) {
          found = (found as Record<Segment, Json>)[segment];
        }

        const list = [...(found as Json[])];

        list.splice(op.index, 0, op.value);

        return applyEdit(text, { op: 'set', path: op.path, value: list });
      }

      const list = blocks(parsed, op.path);
      const block = blockText(op.path, op.value);

      if (op.index < list.length) {
        lines.splice(list[op.index].start, 0, ...block, '');
      } else {
        const end = list.at(-1)?.end ?? lines.length;

        lines.splice(end, 0, '', ...block);
      }

      return lines.join('\n');
    }
    case 'move': {
      const list = blocks(parsed, op.path);

      if (!list[op.from] || !list[op.to]) {
        return pathError('index out of range');
      }

      const first = list[0].start;
      const last = list.at(-1)!.end;
      const chunks = list.map((block, index) =>
        lines.slice(block.start, index + 1 < list.length ? list[index + 1].start : last),
      );
      const [moved] = chunks.splice(op.from, 1);

      chunks.splice(op.to, 0, moved);
      // Keep one blank line between elements.
      const body = chunks.flatMap((chunk) => {
        const trimmed = [...chunk];

        while (trimmed.at(-1)?.trim() === '') {
          trimmed.pop();
        }

        return [...trimmed, ''];
      });

      body.pop();
      lines.splice(first, last - first, ...body);

      return lines.join('\n');
    }
    case 'rename': {
      const existing = keyAt([...op.path, op.from]) ?? pathError('no such key');

      if (keyAt([...op.path, op.to])) {
        return pathError('the key exists');
      }

      lines[existing.line] = lines[existing.line].replace(existing.key, renderKey(op.to));

      return lines.join('\n');
    }
  }
}

function fnv(text: string) {
  let hash = 0xcbf29ce484222325n;

  for (const byte of new TextEncoder().encode(text)) {
    hash ^= BigInt(byte);
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
  }

  return hash.toString(16).padStart(16, '0');
}

/** The catalog default of a file: its schema's root defaults. */
function defaultsOf(schema: { properties?: Record<string, { default?: Json }> }): Json {
  return Object.fromEntries(
    Object.entries(schema.properties ?? {})
      .filter(([, property]) => property.default !== undefined)
      .map(([key, property]) => [key, property.default as Json]),
  );
}

/** A per-file rule the fake checks before committing (the real host validates the schema). */
export type Validator = (value: Obj) => string | null;

export class FakeConfig {
  /** File name → text; `null` is a missing file. */
  readonly texts = new Map<string, string | null>();
  readonly validators = new Map<string, Validator>();
  private readonly host: FakeHost;

  constructor(host: FakeHost, files: Record<string, string | null>) {
    this.host = host;

    for (const [file, text] of Object.entries(files)) {
      this.texts.set(file, text);
    }

    host
      .answer('config_files', () => this.files())
      .answer('config_document', (args: unknown) => this.document((args as { file: string }).file))
      .answer('config_edit', (args: unknown) =>
        this.edit(
          (args as { request: { file: string; baseHash: string | null; op: ConfigEditOp } })
            .request,
        ),
      )
      .answer('open_config_file', { status: 'opened' });
  }

  /** The current text of `file`. */
  text(file: string) {
    return this.texts.get(file) ?? '';
  }

  /** Simulates a save from another app and announces it. */
  async externalWrite(file: string, text: string) {
    this.texts.set(file, text);
    await this.host.emit('config-changed', { files: [file] });
  }

  private files(): ConfigFileView_Serialize[] {
    return [...this.texts.keys()].map((file) => {
      const document = this.document(file).document;

      return {
        file,
        path: document.path,
        state:
          document.state === 'invalid'
            ? 'invalid'
            : document.state === 'missing'
              ? 'missing'
              : 'valid',
        active: document.state === 'valid',
        diagnostic: document.diagnostic?.message ?? null,
        warnings: [],
      } as ConfigFileView_Serialize;
    });
  }

  private document(file: string): { status: 'document'; document: ConfigDocument_Serialize } {
    const schema = JSON.parse(
      readFileSync(path.join(SCHEMA_DIR, file.replace('.toml', '.schema.json')), 'utf8'),
    ) as Json & { properties?: Record<string, { default?: Json }> };
    const text = this.texts.get(file) ?? null;
    const base = {
      file,
      path: `/Users/me/.config/stache/${file}`,
      default: defaultsOf(schema),
      schema,
      warnings: [],
    };

    if (text === null) {
      return {
        status: 'document',
        document: { ...base, state: 'missing', hash: null, value: null, diagnostic: null },
      };
    }

    try {
      const { value } = parseToml(text);

      return {
        status: 'document',
        document: { ...base, state: 'valid', hash: fnv(text), value, diagnostic: null },
      };
    } catch (error) {
      const at = error instanceof TomlError ? error : null;

      return {
        status: 'document',
        document: {
          ...base,
          state: 'invalid',
          hash: fnv(text),
          value: null,
          diagnostic: {
            message: (error as Error).message,
            line: at?.line ?? null,
            column: at?.column ?? null,
            field: null,
          },
        },
      };
    }
  }

  private async edit(request: {
    file: string;
    baseHash: string | null;
    op: ConfigEditOp;
  }): Promise<ConfigEditOutcome_Serialize> {
    const current = this.texts.get(request.file) ?? null;
    const hash = current === null ? null : fnv(current);

    if (hash !== request.baseHash) {
      return { status: 'stale', hash };
    }

    let next: string;

    try {
      parseToml(current ?? '');
      next = applyEdit(current ?? '#:schema stache\n', request.op);
    } catch (error) {
      return { status: 'refused', reason: 'path', message: (error as Error).message };
    }

    const problem = this.validators.get(request.file)?.(parseToml(next).value) ?? null;

    if (problem !== null) {
      return {
        status: 'invalid',
        diagnostic: { message: problem, line: null, column: null, field: null },
      };
    }

    if (next === current) {
      return { status: 'unchanged', hash };
    }

    this.texts.set(request.file, next);
    await this.host.emit('config-changed', { files: [request.file] });

    return {
      status: 'committed',
      hash: fnv(next),
      reload: { status: 'applied', revision: '2' } as never,
    };
  }
}
