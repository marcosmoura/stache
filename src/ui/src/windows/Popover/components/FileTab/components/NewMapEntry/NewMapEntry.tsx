import { useEffect, useRef, useState } from 'react';

import * as stylex from '@stylexjs/stylex';

import { styles } from '../../styles/form.style';
import { useForm, useWrite } from '../../utils/form';
import { seed, type FieldNode } from '../../utils/schema';
import { setOp, type Path } from '../../utils/values';
import { Notes } from '../Notes';

export type NewMapEntryProps = {
  node: FieldNode & { kind: 'map' };
  path: Path;
  /** The keys already in the map. */
  keys: readonly string[];
  /** The map's item noun, the key field's label ("Shortcut"). */
  noun: string;
  onClose: () => void;
};

/**
 * The inline row "Add …" opens: an empty, focused key field. Enter or blur
 * writes the entry (the key with the value's seed) once a valid key is typed;
 * an empty field closes the row on blur; Esc cancels without writing (and
 * does not close the popover).
 */
export function NewMapEntry({ node, path, keys, noun, onClose }: NewMapEntryProps) {
  const form = useForm();
  const write = useWrite();
  const input = useRef<HTMLInputElement>(null);
  const [key, setKey] = useState('');
  // Set while a write is in flight or once the row is closing: the blur that
  // follows Enter (or removal) must not commit again.
  const busy = useRef(false);

  useEffect(() => input.current?.focus(), []);

  const close = () => {
    busy.current = true;
    onClose();
  };

  const commit = async (onBlur: boolean) => {
    if (busy.current) {
      return;
    }

    const next = key.trim();

    if (next === '') {
      if (onBlur) {
        close();
      } else {
        write.setMessage('Enter a key.');
      }

      return;
    }

    if (keys.includes(next)) {
      write.setMessage(`"${next}" already exists.`);

      return;
    }

    busy.current = true;
    const result = await write.run(
      setOp(form.value, form.defaults, [...path, next], seed(node.values)),
    );

    busy.current = false;

    if (result.status === 'ok') {
      close();
    }
  };

  return (
    <li {...stylex.props(styles.item)} role="group" aria-label={`New ${noun.toLowerCase()}`}>
      <div {...stylex.props(styles.itemHead)}>
        <input
          ref={input}
          type="text"
          aria-label={noun}
          aria-invalid={write.message !== null}
          placeholder={`New ${noun.toLowerCase()}`}
          spellCheck={false}
          disabled={form.readOnly || write.pending}
          value={key}
          onChange={(event) => {
            setKey(event.currentTarget.value);
            write.setMessage(null);
          }}
          onBlur={() => void commit(true)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') {
              event.preventDefault();
              void commit(false);
            } else if (event.key === 'Escape') {
              event.preventDefault();
              close();
            }
          }}
          {...stylex.props(
            styles.input,
            styles.wide,
            write.message !== null && styles.inputInvalid,
          )}
        />
      </div>
      <Notes message={write.message} />
    </li>
  );
}
