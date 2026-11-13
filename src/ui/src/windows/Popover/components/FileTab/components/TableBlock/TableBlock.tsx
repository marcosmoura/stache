import { useCopy } from '../../utils/copyContext';
import type { FieldProps } from '../../utils/fields';
import { effectiveAt, isSetAt, useForm, useWrite } from '../../utils/form';
import { Body } from '../Body';
import { Marker } from '../Marker';
import { Section } from '../Section';

/** A nested table, a map or a list: a section around its body. */
export function TableBlock(props: FieldProps) {
  const { node, path, at, label, inTable, depth, bare } = props;
  const form = useForm();
  const write = useWrite();
  const copy = useCopy(at);

  if (bare) {
    return <Body {...props} />;
  }

  const value = effectiveAt(form, path, node);
  const count = Array.isArray(value) ? value.length : null;

  return (
    <Section
      path={path}
      label={label}
      hint={copy.hint ?? null}
      // Long lists start collapsed; deeper tables too.
      defaultOpen={depth <= 1 && (count === null || count <= 8)}
      isSet={isSetAt(form, path)}
      inTable={inTable}
      write={write}
      head={count !== null ? <Marker>{count}</Marker> : null}
    >
      <Body {...props} />
    </Section>
  );
}
