import { useId } from 'react';

import * as stylex from '@stylexjs/stylex';

import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { styles as shared } from '../../styles/form.style';
import { useCopy } from '../../utils/copyContext';
import type { FieldProps } from '../../utils/fields';
import { effectiveAt, useForm, useWrite } from '../../utils/form';
import { seed, type FieldNode } from '../../utils/schema';
import { setOp } from '../../utils/values';
import { Field } from '../Field';
import { Notes } from '../Notes';
import { Row } from '../Row';

/** An optional table or union: "Not set" with Set, else the inner field. */
export function OptionalBlock(props: FieldProps & { node: FieldNode & { kind: 'optional' } }) {
  const { node, path, at, label, inTable } = props;
  const form = useForm();
  const id = useId();
  const copy = useCopy(at);
  const write = useWrite();
  const value = effectiveAt(form, path, node);

  if (value != null) {
    return <Field {...props} node={node.inner} inTable={inTable} />;
  }

  return (
    <Row
      id={id}
      path={path}
      label={label}
      hint={copy.hint ?? null}
      helpId={`${id}-help`}
      pending={write.pending}
      marker={null}
      controls={
        <>
          <span {...stylex.props(shared.muted)}>Not set</span>
          <button
            type="button"
            id={id}
            disabled={form.readOnly || write.pending}
            onClick={() => void write.run(setOp(form.value, form.defaults, path, seed(node.inner)))}
            {...stylex.props(controls.button)}
          >
            Set
          </button>
        </>
      }
      notes={<Notes message={write.message} />}
    />
  );
}
