import type { ConfigEditOp, JsonValue, PathSegment } from '@/ipc';

/*
 * Value helpers of the settings form: reads at a key path and the edit op
 * for a change. The host creates missing tables on `set`, but an array that
 * is absent from the file (its elements come from the default) is written
 * whole, so editing a default element materialises the array first.
 */

export type Path = readonly PathSegment[];

/** The value at `path`, `undefined` when any segment is absent. */
export function getAt(value: unknown, path: Path): unknown {
  let current = value;

  for (const segment of path) {
    if (typeof segment === 'number') {
      if (!Array.isArray(current) || segment >= current.length) {
        return undefined;
      }
    } else if (
      typeof current !== 'object' ||
      current === null ||
      Array.isArray(current) ||
      !(segment in current)
    ) {
      return undefined;
    }

    current = (current as Record<string | number, unknown>)[segment];
  }

  return current;
}

/** A copy of `value` with `path` replaced by `next` (`undefined` deletes). */
export function setAt(value: unknown, path: Path, next: unknown): unknown {
  if (path.length === 0) {
    return next;
  }

  const [head, ...rest] = path;

  if (typeof head === 'number') {
    const array = Array.isArray(value) ? [...(value as unknown[])] : [];

    if (next === undefined && rest.length === 0) {
      array.splice(head, 1);
    } else {
      array[head] = setAt(array[head], rest, next);
    }

    return array;
  }

  const object = { ...(typeof value === 'object' && value !== null ? value : {}) } as Record<
    string,
    unknown
  >;

  if (next === undefined && rest.length === 0) {
    delete object[head];
  } else {
    object[head] = setAt(object[head], rest, next);
  }

  return object;
}

/** The first array along `path` that the file lacks, with its index in `path`. */
function missingArray(file: unknown, path: Path): number | null {
  for (let index = 0; index < path.length; index += 1) {
    if (typeof path[index] === 'number' && getAt(file, path.slice(0, index)) === undefined) {
      return index;
    }
  }

  return null;
}

/**
 * Applies `change` (a function of the effective value at `path`) as one op.
 * `direct` is the op when everything along `path` that matters is in the
 * file; otherwise the absent array is written whole from `defaults`.
 */
function plan(
  file: unknown,
  defaults: unknown,
  path: Path,
  direct: ConfigEditOp,
  change: (current: unknown) => unknown,
): ConfigEditOp {
  const cut = missingArray(file, path);
  const arrayPath = cut === null ? null : path.slice(0, cut);
  // `insert`/`move`/`rename` target the container itself.
  const absentContainer =
    (direct.op === 'insert' || direct.op === 'move') && getAt(file, path) === undefined
      ? path
      : null;
  const target = arrayPath ?? absentContainer;

  if (target === null) {
    return direct;
  }

  const base = structuredClone(getAt(defaults, target) ?? []);
  const inner = path.slice(target.length);
  const next = setAt(base, inner, change(getAt(base, inner)));

  return { op: 'set', path: [...target], value: next as JsonValue };
}

/** Sets the value at `path` (`null` removes it). */
export function setOp(file: unknown, defaults: unknown, path: Path, value: JsonValue) {
  const direct: ConfigEditOp =
    value === null ? { op: 'remove', path: [...path] } : { op: 'set', path: [...path], value };

  return plan(file, defaults, path, direct, () => (value === null ? undefined : value));
}

/** Removes the key or the array element at `path`. */
export function removeOp(file: unknown, defaults: unknown, path: Path) {
  return plan(file, defaults, path, { op: 'remove', path: [...path] }, () => undefined);
}

/** Inserts `value` at `index` of the array at `path`. */
export function insertOp(
  file: unknown,
  defaults: unknown,
  path: Path,
  index: number,
  value: JsonValue,
) {
  return plan(file, defaults, path, { op: 'insert', path: [...path], index, value }, (current) => {
    const array = Array.isArray(current) ? [...(current as unknown[])] : [];

    array.splice(index, 0, value);

    return array;
  });
}

/** Moves the element `from` of the array at `path` to `to`. */
export function moveOp(file: unknown, defaults: unknown, path: Path, from: number, to: number) {
  return plan(file, defaults, path, { op: 'move', path: [...path], from, to }, (current) => {
    const array = Array.isArray(current) ? [...(current as unknown[])] : [];
    const [item] = array.splice(from, 1);

    array.splice(to, 0, item);

    return array;
  });
}

/** Renames the map key `from` to `to` in the map at `path`. */
export function renameOp(file: unknown, defaults: unknown, path: Path, from: string, to: string) {
  const inFile = getAt(file, [...path, from]) !== undefined;

  if (inFile) {
    return { op: 'rename', path: [...path], from, to } satisfies ConfigEditOp;
  }

  const value = getAt(defaults, [...path, from]);

  return setOp(file, defaults, [...path, to], (value ?? null) as JsonValue);
}

/** A key path as text, `workspaces[1].layout`. */
export function pathText(path: Path): string {
  return path
    .map((segment, index) =>
      typeof segment === 'number' ? `[${segment}]` : index === 0 ? segment : `.${segment}`,
    )
    .join('');
}

/** A short TOML-like rendering of a value, for read-only views. */
export function tomlText(value: unknown): string {
  if (value === undefined || value === null) {
    return '';
  }

  if (typeof value === 'string') {
    return JSON.stringify(value);
  }

  if (Array.isArray(value)) {
    return `[${value.map(tomlText).join(', ')}]`;
  }

  if (typeof value === 'object') {
    const entries = Object.entries(value).map(([key, item]) => {
      const name = /^[A-Za-z0-9_-]+$/.test(key) ? key : JSON.stringify(key);

      return `${name} = ${tomlText(item)}`;
    });

    return `{ ${entries.join(', ')} }`;
  }

  return String(value);
}
