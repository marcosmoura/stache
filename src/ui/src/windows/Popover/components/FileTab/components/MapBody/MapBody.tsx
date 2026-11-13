import { useState } from 'react';

import * as stylex from '@stylexjs/stylex';

import { childPath } from '@/windows/Popover/settingsCopy';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { styles } from '../../styles/form.style';
import { useCopy } from '../../utils/copyContext';
import { effectiveAt, useForm } from '../../utils/form';
import type { FieldNode } from '../../utils/schema';
import type { Path } from '../../utils/values';
import { MapEntry } from '../MapEntry';
import { NewMapEntry } from '../NewMapEntry';

export type MapBodyProps = {
  node: FieldNode & { kind: 'map' };
  path: Path;
  at: string;
  depth: number;
};

/**
 * Key/value rows: rename the key, edit the value, remove. "Add …" opens an
 * inline new row whose key field is empty and focused; nothing is written
 * until a key is entered.
 */
export function MapBody({ node, path, at, depth }: MapBodyProps) {
  const form = useForm();
  const copy = useCopy(at);
  const [adding, setAdding] = useState(false);
  const noun = copy.noun ?? copy.label;
  const value = effectiveAt(form, path, node);
  const keys = value && typeof value === 'object' ? Object.keys(value) : [];

  return (
    <>
      <ul {...stylex.props(styles.cards)}>
        {keys.map((key) => (
          <MapEntry
            key={key}
            node={node}
            path={path}
            at={childPath(at, '*')}
            entryKey={key}
            keys={keys}
            noun={noun}
            depth={depth}
          />
        ))}
        {adding ? (
          <NewMapEntry
            node={node}
            path={path}
            keys={keys}
            noun={noun}
            onClose={() => setAdding(false)}
          />
        ) : null}
      </ul>
      <div {...stylex.props(styles.actions)}>
        <button
          type="button"
          disabled={form.readOnly || adding}
          onClick={() => setAdding(true)}
          {...stylex.props(controls.button)}
        >
          Add {noun.toLowerCase()}
        </button>
      </div>
    </>
  );
}
