import { useState } from 'react';

import { ArrowRight01Icon, DragDropVerticalIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import type { PathSegment } from '@/ipc';

import { styles as shared } from '../../styles/form.style';
import type { ArrayEdits } from '../../utils/arrayEdits';
import type { FieldNode } from '../../utils/schema';
import { pathText } from '../../utils/values';
import { Field } from '../Field';
import { ItemActions } from '../ItemActions';

import { styles } from './Card.style';

/** One table of an array: a collapsible card with drag, keyboard reorder and remove. */
export function Card({
  node,
  path,
  at,
  index,
  count,
  title,
  depth,
  edits,
}: {
  node: FieldNode;
  path: readonly PathSegment[];
  at: string;
  index: number;
  count: number;
  title: string;
  depth: number;
  edits: ArrayEdits;
}) {
  const [open, setOpen] = useState(false);
  const [dropping, setDropping] = useState(false);

  return (
    <li
      {...stylex.props(shared.item, dropping && styles.dropTarget)}
      role="group"
      aria-label={title}
      data-path={pathText(path)}
      onDragOver={(event) => {
        if (!edits.disabled) {
          event.preventDefault();
          setDropping(true);
        }
      }}
      onDragLeave={() => setDropping(false)}
      onDrop={(event) => {
        event.preventDefault();
        setDropping(false);
        const from = Number(event.dataTransfer.getData('text/plain'));

        if (Number.isInteger(from) && from !== index) {
          edits.move(from, index);
        }
      }}
    >
      <div {...stylex.props(shared.itemHead)}>
        <span
          {...stylex.props(styles.handle)}
          draggable={!edits.disabled}
          aria-hidden
          onDragStart={(event) => {
            event.dataTransfer.setData('text/plain', String(index));
            event.dataTransfer.effectAllowed = 'move';
          }}
        >
          <Icon icon={DragDropVerticalIcon} size={14} />
        </span>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
          {...stylex.props(shared.toggle)}
        >
          <span {...stylex.props(shared.chevron, open && shared.chevronOpen)}>
            <Icon icon={ArrowRight01Icon} size={12} />
          </span>
          {title}
        </button>
        <ItemActions title={title} index={index} count={count} edits={edits} />
      </div>
      {open ? (
        <div {...stylex.props(shared.nested)}>
          <Field
            node={node}
            path={path}
            at={at}
            label={title}
            inTable={false}
            depth={depth + 1}
            bare
          />
        </div>
      ) : null}
    </li>
  );
}
