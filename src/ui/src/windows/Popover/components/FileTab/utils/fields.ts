import type { ReactNode } from 'react';

import { optionCopy, type OptionCopy } from '@/windows/Popover/settingsCopy';

import type { FieldNode, Variant } from './schema';
import type { Path } from './values';

/** What every field component of the settings form receives. */
export type FieldProps = {
  node: FieldNode;
  path: Path;
  /** The copy path: keys, with `*` for an array element or a map entry. */
  at: string;
  label: string;
  /** The field is a key of a table (it can be default and be reset). */
  inTable: boolean;
  depth: number;
  /** Render without the section header (array cards, map values). */
  bare?: boolean;
  /** The value of a union shown under its own header: no hint repeated. */
  plain?: boolean;
  /** Extra trailing controls (a list item's move and remove). */
  actions?: ReactNode;
};

/** A table's keys in the copy's `order`, the rest after them in schema order; constants dropped. */
export function ordered<T extends { key: string; node: FieldNode }>(
  properties: readonly T[],
  order: readonly string[] = [],
): T[] {
  const rank = (key: string) => {
    const index = order.indexOf(key);

    return index < 0 ? order.length : index;
  };

  return properties
    .filter((property) => property.node.kind !== 'const')
    .sort((a, b) => rank(a.key) - rank(b.key));
}

/** Why a typed number cannot be saved, else `null`. */
export function numberError(node: FieldNode & { kind: 'number' }, raw: string): string | null {
  const parsed = Number(raw.trim());

  if (raw.trim() === '' || Number.isNaN(parsed)) {
    return 'Enter a number.';
  }

  if (node.integer && !Number.isInteger(parsed)) {
    return 'Enter a whole number.';
  }

  if (node.minimum !== null && parsed < node.minimum) {
    return `Must be at least ${node.minimum}.`;
  }

  if (node.maximum !== null && parsed > node.maximum) {
    return `Must be at most ${node.maximum}.`;
  }

  return null;
}

/** A select option from its copy. */
export const choice = (value: string, copy: OptionCopy) => ({
  value,
  title: copy.label,
  hint: copy.hint ?? null,
});

/** A union's forms as select options, by index. */
export const variantOptions = (
  variants: readonly Variant[],
  copy: Record<string, OptionCopy> | undefined,
) => variants.map((variant, index) => choice(String(index), optionCopy(copy, variant.id)));
