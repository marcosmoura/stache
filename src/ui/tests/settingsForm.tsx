import type { ReactNode } from 'react';

import { vi } from 'vitest';
import { render as renderInBrowser } from 'vitest-browser-react';

import type { ConfigEditOp } from '@/ipc';
import type { EditResult } from '@/queries/configDocument';
import { CopyContext } from '@/windows/Popover/components/FileTab/utils/copyContext';
import { FormContext } from '@/windows/Popover/components/FileTab/utils/form';
import type { FieldNode } from '@/windows/Popover/components/FileTab/utils/schema';
import type { FileCopy } from '@/windows/Popover/settingsCopy';

/** Node builders for settings-form component tests. */
const base = { description: null, default: undefined };

export const node = {
  boolean: (): FieldNode => ({ ...base, kind: 'boolean' }),
  string: (): FieldNode => ({ ...base, kind: 'string', minLength: 0 }),
  number: (minimum: number | null = null, maximum: number | null = null): FieldNode => ({
    ...base,
    kind: 'number',
    integer: true,
    minimum,
    maximum,
  }),
  enum: (...values: string[]): FieldNode => ({
    ...base,
    kind: 'enum',
    options: values.map((value) => ({ value, description: null })),
  }),
  const: (value: string): FieldNode => ({ ...base, kind: 'const', value }),
  object: (properties: Record<string, FieldNode>): FieldNode & { kind: 'object' } => ({
    ...base,
    kind: 'object',
    properties: Object.entries(properties).map(([key, child]) => ({
      key,
      node: child,
      required: false,
    })),
  }),
  map: (values: FieldNode): FieldNode & { kind: 'map' } => ({ ...base, kind: 'map', values }),
  array: (items: FieldNode): FieldNode & { kind: 'array' } => ({ ...base, kind: 'array', items }),
  optional: (inner: FieldNode): FieldNode & { kind: 'optional' } => ({
    ...base,
    kind: 'optional',
    inner,
  }),
  union: (variants: Record<string, FieldNode>): FieldNode & { kind: 'union' } => ({
    ...base,
    kind: 'union',
    variants: Object.entries(variants).map(([id, child]) => ({ id, node: child })),
  }),
  unknown: (): FieldNode => ({ ...base, kind: 'unknown', reason: 'test' }),
};

export type FormOptions = {
  value?: unknown;
  defaults?: unknown;
  readOnly?: boolean;
  copy?: FileCopy;
  result?: EditResult;
};

/**
 * Renders a settings-form component inside its form and copy contexts. Every
 * edit resolves to `result` (default `ok`); `edits` records the ops sent.
 */
export async function renderForm(ui: ReactNode, options: FormOptions = {}) {
  const edits: ConfigEditOp[] = [];
  const openInEditor = vi.fn();
  const edit = (op: ConfigEditOp) => {
    edits.push(op);

    return Promise.resolve(options.result ?? ({ status: 'ok' } as const));
  };
  const screen = await renderInBrowser(
    <FormContext
      value={{
        value: options.value ?? {},
        defaults: options.defaults ?? {},
        readOnly: options.readOnly ?? false,
        edit,
        openInEditor,
      }}
    >
      <CopyContext value={options.copy ?? {}}>{ui}</CopyContext>
    </FormContext>,
  );

  return { screen, edits, openInEditor };
}
