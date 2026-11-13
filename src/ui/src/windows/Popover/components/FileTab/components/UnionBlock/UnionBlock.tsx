import { useId } from 'react';

import * as stylex from '@stylexjs/stylex';

import { useCopy } from '../../utils/copyContext';
import { variantOptions, type FieldProps } from '../../utils/fields';
import { effectiveAt, isSetAt, useForm, useWrite } from '../../utils/form';
import { isScalar, seed, variantOf, type FieldNode } from '../../utils/schema';
import { pathText, setOp } from '../../utils/values';
import { Body } from '../Body';
import { Notes } from '../Notes';
import { styles as row } from '../Row/Row.style';
import { ScalarRow } from '../ScalarRow';
import { Section } from '../Section';
import { Select } from '../Select';
import { UnknownRow } from '../UnknownRow';

/** A non-scalar union: a kind select in the header, the active variant's body below. */
export function UnionBlock({
  node,
  path,
  at,
  label,
  inTable,
  depth,
  bare,
}: FieldProps & { node: FieldNode & { kind: 'union' } }) {
  const form = useForm();
  const copy = useCopy(at);
  const id = useId();
  const write = useWrite();
  const value = effectiveAt(form, path, node);
  const variant = variantOf(node, value);
  const kind = (
    <Select
      id={id}
      label={`${label} kind`}
      value={String(Math.max(variant, 0))}
      disabled={form.readOnly || write.pending}
      options={variantOptions(node.variants, copy.variants)}
      onSelect={(index) =>
        void write.run(
          setOp(form.value, form.defaults, path, seed(node.variants[Number(index)].node)),
        )
      }
    />
  );
  const active = variant < 0 ? null : node.variants[variant].node;
  const body =
    active === null ? (
      <UnknownRow node={node} path={path} at={at} label={label} inTable={false} depth={depth} />
    ) : isScalar(active) ? (
      <ScalarRow
        node={active}
        path={path}
        at={at}
        label={copy.valueLabel ?? label}
        inTable={false}
        depth={depth}
        plain
      />
    ) : (
      <Body node={active} path={path} at={at} label={label} inTable={false} depth={depth} />
    );

  if (bare) {
    return (
      <>
        <div {...stylex.props(row.row)} data-path={pathText(path)}>
          <span {...stylex.props(row.label)}>{copy.kindLabel ?? 'Type'}</span>
          <div {...stylex.props(row.controls)}>{kind}</div>
          <Notes message={write.message} />
        </div>
        {body}
      </>
    );
  }

  return (
    <Section
      path={path}
      label={label}
      hint={copy.hint ?? null}
      defaultOpen={depth <= 2}
      isSet={isSetAt(form, path)}
      inTable={inTable}
      write={write}
      head={kind}
    >
      {body}
    </Section>
  );
}
