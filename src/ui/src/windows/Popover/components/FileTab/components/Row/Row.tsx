import type { ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import { pathText, type Path } from '../../utils/values';
import { Marker } from '../Marker';

import { styles } from './Row.style';

export type RowProps = {
  /** The control the label points at. */
  id: string;
  path: Path;
  label: string;
  hint: string | null;
  helpId: string;
  pending: boolean;
  marker: string | null;
  controls: ReactNode;
  notes?: ReactNode;
};

/** One setting: the label (with its marker and hint) on the left, its controls on the right. */
export function Row({ id, path, label, hint, helpId, pending, marker, controls, notes }: RowProps) {
  return (
    <div
      {...stylex.props(styles.row, pending && styles.rowPending)}
      data-path={pathText(path)}
      aria-busy={pending}
    >
      <div {...stylex.props(styles.text)}>
        <span {...stylex.props(styles.labelLine)}>
          <label htmlFor={id} {...stylex.props(styles.label)}>
            {label}
          </label>
          {marker ? <Marker>{marker}</Marker> : null}
        </span>
        {hint ? (
          <span id={helpId} {...stylex.props(styles.help)}>
            {hint}
          </span>
        ) : null}
      </div>
      <div {...stylex.props(styles.controls)}>{controls}</div>
      {notes}
    </div>
  );
}
