import * as stylex from '@stylexjs/stylex';

import { childPath } from '@/windows/Popover/settingsCopy';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { styles } from '../../styles/form.style';
import { useArrayEdits } from '../../utils/arrayEdits';
import { useCopy } from '../../utils/copyContext';
import { effectiveAt, useForm } from '../../utils/form';
import { seed, type FieldNode } from '../../utils/schema';
import type { Path } from '../../utils/values';
import { ItemActions } from '../ItemActions';
import { Notes } from '../Notes';
import { ScalarRow } from '../ScalarRow';

/** A list of scalars: one row per item with move and remove, then Add. */
export function ListBody({
  node,
  path,
  at,
  label,
  depth,
}: {
  node: FieldNode & { kind: 'array' };
  path: Path;
  at: string;
  label: string;
  depth: number;
}) {
  const form = useForm();
  const copy = useCopy(at);
  const noun = copy.noun ?? label;
  const edits = useArrayEdits(path);
  const value = effectiveAt(form, path, node);
  const items = Array.isArray(value) ? value : [];

  return (
    <>
      <div {...stylex.props(styles.card)}>
        {items.map((_, index) => (
          <ScalarRow
            key={index}
            node={node.items}
            path={[...path, index]}
            at={childPath(at, '*')}
            label={`${noun} ${index + 1}`}
            inTable={false}
            depth={depth + 1}
            actions={
              <ItemActions
                title={`${noun} ${index + 1}`}
                index={index}
                count={items.length}
                edits={edits}
              />
            }
          />
        ))}
        {items.length === 0 ? <p {...stylex.props(styles.message, styles.muted)}>Empty</p> : null}
      </div>
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
