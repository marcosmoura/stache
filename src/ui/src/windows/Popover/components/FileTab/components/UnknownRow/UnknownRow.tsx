import { useId } from 'react';

import { useCopy } from '../../utils/copyContext';
import type { FieldProps } from '../../utils/fields';
import { effectiveAt, useForm } from '../../utils/form';
import { Row } from '../Row';
import { UnknownValue } from '../UnknownValue';

/** A construct the walk did not recognise: its TOML text and "Open in editor". */
export function UnknownRow({ node, path, at, label }: FieldProps) {
  const form = useForm();
  const id = useId();
  const copy = useCopy(at);

  return (
    <Row
      id={id}
      path={path}
      label={label}
      hint={copy.hint ?? null}
      helpId={`${id}-help`}
      pending={false}
      marker={null}
      controls={<UnknownValue value={effectiveAt(form, path, node)} />}
    />
  );
}
