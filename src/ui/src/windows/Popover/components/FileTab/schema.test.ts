import { describe, expect, it } from 'vitest';

import {
  isScalar,
  matches,
  schemaTree,
  seed,
  unknownNodes,
  variantOf,
  type FieldNode,
} from './schema';

const SCHEMAS = import.meta.glob<Record<string, unknown>>('../../../../../../../schemas/*.json', {
  eager: true,
  import: 'default',
});

const byName = (name: string) => {
  const entry = Object.entries(SCHEMAS).find(([path]) => path.endsWith(`/${name}.schema.json`));

  if (!entry) {
    throw new Error(`missing schema ${name}`);
  }

  return entry[1];
};

const property = (node: FieldNode, key: string): FieldNode => {
  if (node.kind !== 'object') {
    throw new Error(`not an object: ${node.kind}`);
  }

  const found = node.properties.find((candidate) => candidate.key === key);

  if (!found) {
    throw new Error(`no property ${key}`);
  }

  return found.node;
};

describe('schemaTree', () => {
  it('walks all ten committed schemas with no unknown fallback', () => {
    const names = Object.keys(SCHEMAS).map((path) => path.split('/').at(-1));

    expect(names).toHaveLength(10);

    for (const [path, schema] of Object.entries(SCHEMAS)) {
      const tree = schemaTree(schema);

      expect(tree.kind, path).toBe('object');
      expect(unknownNodes(tree), path).toEqual([]);
    }
  });

  it('reads booleans, enums with descriptions, bounded integers and strings', () => {
    const tiling = schemaTree(byName('tiling'));
    const layout = property(tiling, 'default_layout');

    expect(property(tiling, 'enabled')).toMatchObject({ kind: 'boolean', default: false });
    expect(layout).toMatchObject({ kind: 'enum', default: 'dwindle' });
    expect(layout.kind === 'enum' && layout.options[0]).toEqual({
      value: 'dwindle',
      description: 'Binary space partitioning.',
    });
    expect(property(property(tiling, 'master'), 'ratio')).toMatchObject({
      kind: 'number',
      integer: true,
      minimum: 0,
      maximum: null,
      default: 60,
    });
  });

  it('reads optionals, unions, maps and arrays of tables', () => {
    const tiling = schemaTree(byName('tiling'));
    const workspaces = property(tiling, 'workspaces');

    expect(workspaces.kind).toBe('array');

    const workspace = workspaces.kind === 'array' ? workspaces.items : workspaces;

    expect(property(workspace, 'layout')).toMatchObject({
      kind: 'optional',
      inner: { kind: 'enum' },
    });
    expect(property(workspace, 'preset_on_open')).toMatchObject({
      kind: 'optional',
      inner: { kind: 'string' },
    });
    expect(property(tiling, 'gaps')).toMatchObject({ kind: 'union' });

    const keybindings = schemaTree(byName('keybindings'));
    const bindings = property(keybindings, 'bindings');

    expect(bindings).toMatchObject({ kind: 'map', values: { kind: 'union' } });

    const bar = schemaTree(byName('bar'));
    const start = property(property(bar, 'layout'), 'start');
    const entry = start.kind === 'array' ? start.items : start;

    // A widget name or one of the eight table forms, flattened.
    expect(entry.kind === 'union' && entry.variants.map((variant) => variant.label)).toEqual([
      'Choice',
      'spaces',
      'media',
      'weather',
      'cpu',
      'battery',
      'keep_awake',
      'wifi',
      'clock',
    ]);
  });

  it('falls back to unknown for an unrecognised construct', () => {
    const tree = schemaTree({
      type: 'object',
      properties: {
        fine: { type: 'boolean' },
        odd: { not: { type: 'string' }, description: 'Anything but text.' },
        dangling: { $ref: '#/$defs/Missing' },
      },
    });

    expect(unknownNodes(tree)).toEqual([
      'odd: unsupported schema',
      'dangling: unresolved reference #/$defs/Missing',
    ]);
    expect(property(tree, 'odd')).toMatchObject({
      kind: 'unknown',
      description: 'Anything but text.',
    });
  });

  it('guards recursive references', () => {
    const tree = schemaTree({
      type: 'object',
      properties: { node: { $ref: '#/$defs/Node' } },
      $defs: { Node: { type: 'object', properties: { child: { $ref: '#/$defs/Node' } } } },
    });

    expect(unknownNodes(tree)).toEqual(['node.child: recursive reference #/$defs/Node']);
  });
});

describe('variants and seeds', () => {
  const tiling = schemaTree(byName('tiling'));
  const gaps = property(tiling, 'gaps');

  it('picks the variant a value fits', () => {
    expect(gaps.kind).toBe('union');

    if (gaps.kind !== 'union') {
      return;
    }

    expect(variantOf(gaps, { inner: 4 })).toBe(0);
    expect(variantOf(gaps, [{ screen: 'main' }])).toBe(1);
    expect(variantOf(gaps, 'nope')).toBe(-1);
  });

  it('matches objects by required keys and constants', () => {
    const bar = schemaTree(byName('bar'));
    const start = property(property(bar, 'layout'), 'start');
    const entry = start.kind === 'array' ? start.items : start;

    expect(entry.kind).toBe('union');

    if (entry.kind !== 'union') {
      return;
    }

    expect(entry.variants[variantOf(entry, 'clock')].label).toBe('Choice');
    expect(entry.variants[variantOf(entry, { widget: 'cpu', id: 'c' })].label).toBe('cpu');
    expect(matches(entry, { widget: 'nope' })).toBe(false);
  });

  it('seeds new values from defaults, required keys and bounds', () => {
    const workspaces = property(tiling, 'workspaces');

    expect(workspaces.kind === 'array' && seed(workspaces.items)).toEqual({ name: 'new' });
    expect(seed(property(tiling, 'default_layout'))).toBe('dwindle');
    expect(
      seed({
        kind: 'number',
        integer: true,
        minimum: 2,
        maximum: null,
        description: null,
        default: undefined,
      }),
    ).toBe(2);
    expect(isScalar(property(tiling, 'enabled'))).toBe(true);
    expect(isScalar(gaps)).toBe(false);
  });
});
