import { describe, expect, it } from 'vitest';

import {
  schemaTree,
  variantId,
  type FieldNode,
} from '@/windows/Popover/components/FileTab/utils/schema';

import { childPath, COPY, humanise, type FieldCopy } from '.';

const SCHEMAS = import.meta.glob<unknown>('../../../../../../schemas/*.json', {
  eager: true,
  import: 'default',
});

/** What the form looks up: every copy path, with the enum values and union forms there. */
type Need = {
  label: boolean;
  /** A list or map is here: it needs a noun for its items. */
  list: boolean;
  /** The keys of every table here, for `order`. */
  keys: Set<string>;
  options: Set<string>;
  variants: Set<string>;
};

function needs(node: FieldNode, at: string, out: Map<string, Need>, label = false): void {
  const need = out.get(at) ?? {
    label: false,
    list: false,
    keys: new Set(),
    options: new Set(),
    variants: new Set(),
  };

  need.label ||= label;
  out.set(at, need);

  switch (node.kind) {
    case 'object':
      node.properties.forEach((property) => {
        need.keys.add(property.key);
        needs(property.node, childPath(at, property.key), out, true);
      });
      break;
    case 'map':
      need.list = true;
      needs(node.values, childPath(at, '*'), out);
      break;
    case 'array':
      need.list = true;
      needs(node.items, childPath(at, '*'), out);
      break;
    case 'optional':
      needs(node.inner, at, out);
      break;
    case 'enum':
      node.options.forEach((option) => need.options.add(option.value));
      break;
    case 'union':
      node.variants.forEach((variant) => {
        need.variants.add(variantId(variant.node));
        needs(variant.node, at, out);
      });
      break;
    default:
  }
}

const FILES = Object.entries(SCHEMAS).map(([path, schema]) => {
  const name = path.split('/').at(-1)!.replace('.schema.json', '.toml');
  const found = new Map<string, Need>();

  needs(schemaTree(schema), '', found);

  return { name, needs: found, copy: COPY[name] ?? {} };
});

/** Every piece of text in one field's copy. */
const texts = (field: FieldCopy) => [
  field.label,
  field.hint,
  field.noun,
  field.kindLabel,
  field.valueLabel,
  ...Object.values({ ...field.options, ...field.variants }).flatMap((choice) => [
    choice.label,
    choice.hint,
  ]),
];

const ALL = FILES.flatMap((file) =>
  Object.entries(file.copy).flatMap(([at, field]) =>
    texts(field)
      .filter((text) => text !== undefined)
      .map((text) => ({ where: `${file.name} ${at}`, text })),
  ),
);

/** Code-like tokens copy must not contain. */
const CODE = [/[`~/=_<>{}[\]\\|$]/, /\b(toml|schema|struct|env|json|true|false|null)\b/i];

/**
 * The only words allowed to break the casing rules (no all-caps words, no
 * camelCase): product names and acronyms people know.
 */
const WORDS = ['API', 'CPU', 'ID', 'AM', 'PM', 'macOS', 'iTunes'];

const withoutWords = (text: string) =>
  WORDS.reduce((rest, word) => rest.replaceAll(new RegExp(`\\b${word}\\b`, 'g'), ''), text);

describe('settings copy', () => {
  it('humanises a key without copy', () => {
    expect(humanise('default_layout')).toBe('Default layout');
    expect(humanise('hourCycle')).toBe('Hour cycle');
  });

  it('covers the ten catalog files', () => {
    expect(FILES).toHaveLength(10);
    expect([...Object.keys(COPY)].sort()).toEqual(FILES.map((file) => file.name).sort());
  });

  it('labels every key, enum value and union form of every schema', () => {
    const missing = FILES.flatMap(({ name, needs: found, copy }) =>
      [...found].flatMap(([at, need]) => {
        const field = copy[at];

        return [
          ...(need.label && !field ? [`${name} ${at}`] : []),
          ...[...need.options]
            .filter((value) => !field?.options?.[value])
            .map((value) => `${name} ${at} = ${value}`),
          ...[...need.variants]
            .filter((id) => !field?.variants?.[id])
            .map((id) => `${name} ${at} | ${id}`),
        ];
      }),
    );

    expect(missing).toEqual([]);
  });

  it('names the items of every list and map', () => {
    const missing = FILES.flatMap(({ name, needs: found, copy }) =>
      [...found]
        .filter(([at, need]) => need.list && !copy[at]?.noun)
        .map(([at]) => `${name} ${at}`),
    );

    expect(missing).toEqual([]);
  });

  it('orders only real keys of a table, each once', () => {
    const bad = FILES.flatMap(({ name, needs: found, copy }) =>
      Object.entries(copy).flatMap(([at, field]) => {
        const order = field.order ?? [];
        const keys = found.get(at)?.keys ?? new Set();

        return [
          ...order.filter((key) => !keys.has(key)).map((key) => `${name} ${at}: ${key}`),
          ...(new Set(order).size === order.length ? [] : [`${name} ${at}: repeated key`]),
        ];
      }),
    );

    expect(bad).toEqual([]);
  });

  it('orders every table with more than one key, the switch first', () => {
    const tables = FILES.flatMap(({ name, needs: found, copy }) =>
      [...found]
        .filter(([, need]) => need.keys.size > 1)
        .flatMap(([at, need]) => {
          const order = copy[at]?.order;

          if (!order) {
            return [`${name} ${at}: no order`];
          }

          return need.keys.has('enabled') && order[0] !== 'enabled'
            ? [`${name} ${at}: enabled not first`]
            : [];
        }),
    );

    expect(tables).toEqual([]);
  });

  it('has no stale paths, values or forms', () => {
    const stale = FILES.flatMap(({ name, needs: found, copy }) =>
      Object.entries(copy).flatMap(([at, field]) => {
        const need = found.get(at);

        if (!need) {
          return [`${name} ${at}`];
        }

        return [
          ...Object.keys(field.options ?? {})
            .filter((value) => !need.options.has(value))
            .map((value) => `${name} ${at} = ${value}`),
          ...Object.keys(field.variants ?? {})
            .filter((id) => !need.variants.has(id))
            .map((id) => `${name} ${at} | ${id}`),
        ];
      }),
    );

    expect(stale).toEqual([]);
  });

  it('keeps labels within 32 characters and hints within 60', () => {
    const long = FILES.flatMap(({ name, copy }) =>
      Object.entries(copy).flatMap(([at, field]) => {
        const choices = Object.values({ ...field.options, ...field.variants });

        return [
          ...[field.label, ...choices.map((choice) => choice.label)].filter(
            (label) => label.length > 32,
          ),
          ...[field.hint, ...choices.map((choice) => choice.hint)].filter(
            (hint) => hint !== undefined && hint.length > 60,
          ),
        ].map((text) => `${name} ${at}: ${text}`);
      }),
    );

    expect(long).toEqual([]);
  });

  it('reads as plain language, with no code-like tokens', () => {
    const bad = ALL.filter(({ text }) => {
      const rest = withoutWords(text);

      return (
        CODE.some((pattern) => pattern.test(text)) ||
        /^whether\b/i.test(text) ||
        /[a-z][A-Z]/.test(rest) ||
        /\b[A-Z]{2,}/.test(rest)
      );
    });

    expect(bad).toEqual([]);
  });

  it('starts every text with a capital and keeps each hint to one sentence', () => {
    expect(ALL.filter(({ text }) => !/^([A-Z0-9]|macOS|iTunes)/.test(text))).toEqual([]);
    expect(ALL.filter(({ text }) => /[.!?]\s+\S/.test(text))).toEqual([]);
  });
});
