import { use } from 'react';

import * as stylex from '@stylexjs/stylex';

import { childPath, fieldCopy } from '@/windows/Popover/settingsCopy';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { styles } from '../../styles/form.style';
import { useArrayEdits } from '../../utils/arrayEdits';
import { CopyContext } from '../../utils/copyContext';
import { effectiveAt, useForm } from '../../utils/form';
import { itemTitle } from '../../utils/itemTitle';
import { seed, type FieldNode } from '../../utils/schema';
import type { Path } from '../../utils/values';
import { Card } from '../Card';
import { Notes } from '../Notes';

/** An array of tables: collapsible cards with add, remove, drag and keyboard reorder. */
export function CardsBody({
  node,
  path,
  at,
  depth,
}: {
  node: FieldNode & { kind: 'array' };
  path: Path;
  at: string;
  depth: number;
}) {
  const form = useForm();
  const copy = use(CopyContext);
  const list = fieldCopy(copy, at);
  const noun = list.noun ?? list.label;
  const item = fieldCopy(copy, childPath(at, '*'));
  const edits = useArrayEdits(path);
  const value = effectiveAt(form, path, node);
  const items = Array.isArray(value) ? value : [];

  return (
    <>
      <ul {...stylex.props(styles.cards)}>
        {items.map((entry, index) => (
          <Card
            key={index}
            node={node.items}
            path={[...path, index]}
            at={childPath(at, '*')}
            index={index}
            count={items.length}
            title={itemTitle(entry, index, item, noun)}
            depth={depth}
            edits={edits}
          />
        ))}
      </ul>
      <Notes message={edits.write.message} />
      <div {...stylex.props(styles.actions)}>
        <button
          type="button"
          disabled={edits.disabled}
          onClick={() => edits.insert(items.length, seed(node.items))}
          {...stylex.props(controls.button)}
        >
          Add {noun.toLowerCase()}
        </button>
      </div>
    </>
  );
}
