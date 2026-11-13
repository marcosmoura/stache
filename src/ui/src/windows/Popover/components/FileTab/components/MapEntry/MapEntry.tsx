import { Delete02Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { styles } from '../../styles/form.style';
import { useCopy } from '../../utils/copyContext';
import { useDraft, useForm, useWrite } from '../../utils/form';
import type { FieldNode } from '../../utils/schema';
import { pathText, removeOp, renameOp, type Path } from '../../utils/values';
import { Field } from '../Field';
import { Notes } from '../Notes';
import { RoundButton } from '../RoundButton';

/** One map entry: its key (renamed on Enter or blur), remove, and its value. */
export function MapEntry({
  node,
  path,
  at,
  entryKey,
  keys,
  noun,
  depth,
}: {
  node: FieldNode & { kind: 'map' };
  path: Path;
  /** The copy path of the entry (`bindings.*`). */
  at: string;
  entryKey: string;
  keys: readonly string[];
  /** The map's item noun, the key field's label ("Shortcut"). */
  noun: string;
  depth: number;
}) {
  const form = useForm();
  const write = useWrite();
  const copy = useCopy(at);
  const draft = useDraft(entryKey, (raw) => {
    const next = raw.trim();
    const error =
      next === ''
        ? 'Enter a key.'
        : keys.includes(next) && next !== entryKey
          ? `"${next}" already exists.`
          : null;

    if (error !== null) {
      write.setMessage(error);

      return Promise.resolve({ status: 'error', message: error } as const);
    }

    return write.run(renameOp(form.value, form.defaults, path, entryKey, next));
  });

  return (
    <li
      {...stylex.props(styles.item)}
      role="group"
      aria-label={entryKey}
      data-path={pathText([...path, entryKey])}
    >
      <div {...stylex.props(styles.itemHead)}>
        <input
          type="text"
          aria-label={noun}
          spellCheck={false}
          disabled={form.readOnly || write.pending}
          {...draft.inputProps}
          {...stylex.props(styles.input, styles.wide)}
        />
        <RoundButton
          label={`Remove ${entryKey}`}
          icon={Delete02Icon}
          disabled={form.readOnly || write.pending}
          onClick={() => void write.run(removeOp(form.value, form.defaults, [...path, entryKey]))}
        />
      </div>
      <Notes message={write.message} />
      <Field
        node={node.values}
        path={[...path, entryKey]}
        at={at}
        label={copy.valueLabel ?? copy.label}
        inTable={false}
        depth={depth + 1}
        bare
      />
    </li>
  );
}
