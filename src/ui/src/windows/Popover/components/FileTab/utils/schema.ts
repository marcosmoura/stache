import type { JsonValue } from '@/ipc';

/*
 * The schema walk: a pure function from a file's JSON Schema to the field
 * tree the settings form renders (D6). It resolves `$ref` into `$defs` (a
 * sibling `properties`/`required` is merged in), reads `anyOf`/`oneOf` with a
 * `null` branch (or a `type` array with `null`) as an optional field, a
 * `oneOf`/`anyOf` of string constants as an enum with descriptions, any other
 * `anyOf`/`oneOf` as a union of variants, `additionalProperties` as a map and
 * `items` as a list. Anything else becomes `unknown`, which the form shows
 * read-only with its TOML text and "Open in editor".
 */

type Schema = { [key: string]: unknown };

export type EnumOption = { value: string; description: string | null };

type Base = {
  description: string | null;
  /** The schema `default`, `undefined` when there is none. */
  default: unknown;
};

/** A union form; `id` keys its copy (`variantId`). */
export type Variant = { id: string; node: FieldNode };

export type PropertyNode = { key: string; node: FieldNode; required: boolean };

export type FieldNode = Base &
  (
    | { kind: 'boolean' }
    | { kind: 'enum'; options: EnumOption[] }
    | { kind: 'number'; integer: boolean; minimum: number | null; maximum: number | null }
    | { kind: 'string'; minLength: number }
    | { kind: 'const'; value: JsonValue }
    | { kind: 'object'; properties: PropertyNode[] }
    | { kind: 'map'; values: FieldNode }
    | { kind: 'array'; items: FieldNode }
    | { kind: 'optional'; inner: FieldNode }
    | { kind: 'union'; variants: Variant[] }
    | { kind: 'unknown'; reason: string }
  );

export type FieldKind = FieldNode['kind'];

const isSchema = (value: unknown): value is Schema =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const text = (value: unknown) => (typeof value === 'string' ? value : null);

const isNullBranch = (branch: unknown) =>
  isSchema(branch) && branch.type === 'null' && Object.keys(branch).length === 1;

const isStringConst = (branch: unknown): branch is Schema =>
  isSchema(branch) && typeof branch.const === 'string';

function resolveRef(root: Schema, ref: string): Schema | null {
  const match = /^#\/(\$defs|definitions)\/(.+)$/.exec(ref);
  const defs = match ? root[match[1]] : undefined;
  const target = isSchema(defs) && match ? defs[match[2]] : undefined;

  return isSchema(target) ? target : null;
}

/** Folds a `$ref` target under its sibling keys (siblings win; properties merge). */
function mergeRef(target: Schema, sibling: Schema): Schema {
  const { $ref: _ref, ...rest } = sibling;
  const properties = {
    ...(isSchema(target.properties) ? target.properties : {}),
    ...(isSchema(rest.properties) ? rest.properties : {}),
  };
  const required = [
    ...(Array.isArray(target.required) ? target.required : []),
    ...(Array.isArray(rest.required) ? rest.required : []),
  ];
  const merged: Schema = { ...target, ...rest };

  if (Object.keys(properties).length > 0) {
    merged.properties = properties;
  }

  if (required.length > 0) {
    merged.required = [...new Set(required)];
  }

  return merged;
}

const unknown = (reason: string, schema: Schema): FieldNode => ({
  kind: 'unknown',
  reason,
  description: text(schema.description),
  default: schema.default,
});

/**
 * A stable id for a union form: its constant tag (a Bar widget), else the
 * sorted keys of a table (`color+width`), else its kind (`number`).
 */
export function variantId(node: FieldNode): string {
  if (node.kind === 'object') {
    const constant = node.properties.find((property) => property.node.kind === 'const');

    if (constant && constant.node.kind === 'const') {
      return String(constant.node.value);
    }

    return node.properties
      .map((property) => property.key)
      .sort()
      .join('+');
  }

  return node.kind;
}

function walkBranches(branches: unknown[], schema: Schema, root: Schema, stack: string[]) {
  const description = text(schema.description);
  const nullable = branches.some(isNullBranch);
  const rest = branches.filter((branch) => !isNullBranch(branch));
  let inner: FieldNode;

  if (rest.length > 0 && rest.every(isStringConst)) {
    inner = {
      kind: 'enum',
      options: rest.map((branch) => ({
        value: branch.const as string,
        description: text(branch.description),
      })),
      description,
      default: schema.default,
    };
  } else if (rest.length === 1) {
    const branch = rest[0];
    const node = walk(isSchema(branch) ? branch : {}, root, stack);

    inner = { ...node, description: description ?? node.description, default: schema.default };
  } else {
    const variants: Variant[] = [];

    for (const branch of rest) {
      const node = walk(isSchema(branch) ? branch : {}, root, stack);

      // A union of unions (a widget name or one of its table forms) is flat.
      if (node.kind === 'union') {
        variants.push(...node.variants);
      } else {
        variants.push({ id: variantId(node), node });
      }
    }

    inner = { kind: 'union', variants, description, default: schema.default };
  }

  if (!nullable) {
    return inner;
  }

  return {
    kind: 'optional',
    inner: { ...inner, default: undefined },
    description,
    default: schema.default,
  } satisfies FieldNode;
}

function walk(schema: Schema, root: Schema, stack: string[]): FieldNode {
  if (typeof schema.$ref === 'string') {
    const target = resolveRef(root, schema.$ref);

    if (target === null) {
      return unknown(`unresolved reference ${schema.$ref}`, schema);
    }

    if (stack.includes(schema.$ref)) {
      return unknown(`recursive reference ${schema.$ref}`, schema);
    }

    const merged = mergeRef(target, schema);

    return walk(merged, root, [...stack, schema.$ref]);
  }

  const description = text(schema.description);
  const base = { description, default: schema.default };

  if ('const' in schema) {
    return { kind: 'const', value: schema.const as JsonValue, ...base };
  }

  if (Array.isArray(schema.anyOf) || Array.isArray(schema.oneOf)) {
    return walkBranches((schema.anyOf ?? schema.oneOf) as unknown[], schema, root, stack);
  }

  if (Array.isArray(schema.type)) {
    const types = schema.type as unknown[];

    return walkBranches(
      types.map((type) => (type === 'null' ? { type } : { ...schema, type })),
      schema,
      root,
      stack,
    );
  }

  if (Array.isArray(schema.enum) && schema.enum.every((value) => typeof value === 'string')) {
    return {
      kind: 'enum',
      options: (schema.enum as string[]).map((value) => ({ value, description: null })),
      ...base,
    };
  }

  switch (schema.type) {
    case 'boolean':
      return { kind: 'boolean', ...base };
    case 'integer':
    case 'number':
      return {
        kind: 'number',
        integer: schema.type === 'integer',
        minimum: typeof schema.minimum === 'number' ? schema.minimum : null,
        maximum: typeof schema.maximum === 'number' ? schema.maximum : null,
        ...base,
      };
    case 'string':
      return {
        kind: 'string',
        minLength: typeof schema.minLength === 'number' ? schema.minLength : 0,
        ...base,
      };
    case 'array':
      return isSchema(schema.items)
        ? { kind: 'array', items: walk(schema.items, root, stack), ...base }
        : unknown('array without items', schema);
    case 'object': {
      if (isSchema(schema.properties)) {
        const required = new Set(Array.isArray(schema.required) ? schema.required : []);

        return {
          kind: 'object',
          properties: Object.entries(schema.properties).map(([key, value]) => ({
            key,
            node: walk(isSchema(value) ? value : {}, root, stack),
            required: required.has(key),
          })),
          ...base,
        };
      }

      if (isSchema(schema.additionalProperties)) {
        return { kind: 'map', values: walk(schema.additionalProperties, root, stack), ...base };
      }

      return unknown('object without properties', schema);
    }
    default:
      return unknown('unsupported schema', schema);
  }
}

/** The field tree of a whole file schema (its root is normally an object). */
export function schemaTree(schema: unknown): FieldNode {
  return isSchema(schema) ? walk(schema, schema, []) : unknown('not a schema', {});
}

/** Every `unknown` node in a tree, with its path, for tests and diagnostics. */
export function unknownNodes(node: FieldNode, path = ''): string[] {
  switch (node.kind) {
    case 'unknown':
      return [`${path || '<root>'}: ${node.reason}`];
    case 'object':
      return node.properties.flatMap((property) =>
        unknownNodes(property.node, path ? `${path}.${property.key}` : property.key),
      );
    case 'map':
      return unknownNodes(node.values, `${path}.*`);
    case 'array':
      return unknownNodes(node.items, `${path}[]`);
    case 'optional':
      return unknownNodes(node.inner, path);
    case 'union':
      return node.variants.flatMap((variant) =>
        unknownNodes(variant.node, `${path}<${variant.id}>`),
      );
    default:
      return [];
  }
}

/** Whether a node renders as one inline control (not a section or a card). */
export function isScalar(node: FieldNode): boolean {
  switch (node.kind) {
    case 'boolean':
    case 'enum':
    case 'number':
    case 'string':
    case 'const':
      return true;
    case 'optional':
      return isScalar(node.inner);
    case 'union':
      return node.variants.every((variant) => isScalar(variant.node));
    default:
      return false;
  }
}

const isPlainObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

/** Whether `value` fits `node` (used to pick a union's variant). */
export function matches(node: FieldNode, value: unknown): boolean {
  switch (node.kind) {
    case 'boolean':
      return typeof value === 'boolean';
    case 'number':
      return typeof value === 'number' && (!node.integer || Number.isInteger(value));
    case 'string':
      return typeof value === 'string';
    case 'enum':
      return node.options.some((option) => option.value === value);
    case 'const':
      return value === node.value;
    case 'object':
      return (
        isPlainObject(value) &&
        node.properties.every(
          (property) =>
            (!property.required || property.key in value) &&
            (property.node.kind !== 'const' || value[property.key] === property.node.value),
        )
      );
    case 'map':
      return isPlainObject(value);
    case 'array':
      return Array.isArray(value);
    case 'optional':
      return value == null || matches(node.inner, value);
    case 'union':
      return node.variants.some((variant) => matches(variant.node, value));
    default:
      return false;
  }
}

/** The index of the union variant `value` fits, else -1. */
export function variantOf(node: FieldNode & { kind: 'union' }, value: unknown): number {
  return node.variants.findIndex((variant) => matches(variant.node, value));
}

/** A first value for a new element, a new map entry or a variant switch. */
export function seed(node: FieldNode, key?: string): JsonValue {
  if (node.default !== undefined && node.default !== null) {
    return structuredClone(node.default) as JsonValue;
  }

  switch (node.kind) {
    case 'boolean':
      return false;
    case 'number':
      return node.minimum ?? 0;
    case 'string':
      return node.minLength > 0 || key === 'name' ? 'new' : '';
    case 'enum':
      return node.options[0]?.value ?? '';
    case 'const':
      return node.value;
    case 'object':
      return Object.fromEntries(
        node.properties
          .filter((property) => property.required)
          .map((property) => [property.key, seed(property.node, property.key)]),
      );
    case 'map':
      return {};
    case 'array':
      return [];
    case 'union':
      return node.variants[0] ? seed(node.variants[0].node, key) : null;
    default:
      return null;
  }
}
