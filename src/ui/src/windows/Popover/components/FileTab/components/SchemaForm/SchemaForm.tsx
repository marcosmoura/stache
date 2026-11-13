import * as stylex from '@stylexjs/stylex';

import { fieldCopy, type FileCopy } from '@/windows/Popover/settingsCopy';

import { CopyContext } from '../../utils/copyContext';
import type { FieldNode } from '../../utils/schema';
import { Field } from '../Field';
import { ObjectBody } from '../ObjectBody';

import { styles } from './SchemaForm.style';

/*
 * The generic settings form (D6–D8): one component per control kind, all
 * driven by the field tree. A field shows the file's value, else the default
 * (marked "default"); a set key inside a table has "Reset to default"
 * (`remove`). Switches, selects, steppers and structural actions write at
 * once; text and numbers on blur or Enter. Every label, hint, unit and option
 * name comes from the file's copy (`settingsCopy`), looked up by copy path;
 * schema descriptions are never shown.
 */

/** The whole form of one file. */
export function SchemaForm({ tree, copy }: { tree: FieldNode; copy: FileCopy }) {
  return (
    <CopyContext value={copy}>
      <div {...stylex.props(styles.form)} data-testid="settings-form">
        {tree.kind === 'object' ? (
          <ObjectBody node={tree} path={[]} at="" depth={0} />
        ) : (
          <Field
            node={tree}
            path={[]}
            at=""
            label={fieldCopy(copy, '').label}
            inTable={false}
            depth={0}
          />
        )}
      </div>
    </CopyContext>
  );
}
