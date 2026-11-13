import { ArrowDown01Icon, ArrowUp01Icon, Delete02Icon } from '@hugeicons/core-free-icons';

import type { ArrayEdits } from '../../utils/arrayEdits';
import { RoundButton } from '../RoundButton';

/** A list item's move up, move down and remove. */
export function ItemActions({
  title,
  index,
  count,
  edits,
}: {
  title: string;
  index: number;
  count: number;
  edits: ArrayEdits;
}) {
  return (
    <>
      <RoundButton
        label={`Move ${title} up`}
        icon={ArrowUp01Icon}
        disabled={edits.disabled || index === 0}
        onClick={() => edits.move(index, index - 1)}
      />
      <RoundButton
        label={`Move ${title} down`}
        icon={ArrowDown01Icon}
        disabled={edits.disabled || index === count - 1}
        onClick={() => edits.move(index, index + 1)}
      />
      <RoundButton
        label={`Remove ${title}`}
        icon={Delete02Icon}
        disabled={edits.disabled}
        onClick={() => edits.remove(index)}
      />
    </>
  );
}
