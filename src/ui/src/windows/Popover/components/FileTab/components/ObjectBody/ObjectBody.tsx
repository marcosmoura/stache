import { use } from 'react';

import * as stylex from '@stylexjs/stylex';

import { childPath, fieldCopy } from '@/windows/Popover/settingsCopy';

import { styles } from '../../styles/form.style';
import { CopyContext } from '../../utils/copyContext';
import { ordered } from '../../utils/fields';
import { isScalar, type FieldNode } from '../../utils/schema';
import type { Path } from '../../utils/values';
import { Field } from '../Field';

export type ObjectBodyProps = {
  node: FieldNode & { kind: 'object' };
  path: Path;
  at: string;
  depth: number;
};

/** Groups consecutive scalar keys on one card; nested values follow as sections. */
export function ObjectBody({ node, path, at, depth }: ObjectBodyProps) {
  const copy = use(CopyContext);
  const groups: Array<{ scalar: boolean; keys: typeof node.properties }> = [];

  for (const property of ordered(node.properties, fieldCopy(copy, at).order)) {
    const scalar = isScalar(property.node);
    const last = groups.at(-1);

    if (last && last.scalar && scalar) {
      last.keys.push(property);
    } else {
      groups.push({ scalar, keys: [property] });
    }
  }

  return (
    <>
      {groups.map((group) => {
        const fields = group.keys.map((property) => (
          <Field
            key={property.key}
            node={property.node}
            path={[...path, property.key]}
            at={childPath(at, property.key)}
            label={fieldCopy(copy, childPath(at, property.key)).label}
            inTable
            depth={depth + 1}
          />
        ));

        return group.scalar ? (
          <div key={group.keys[0].key} {...stylex.props(styles.card)}>
            {fields}
          </div>
        ) : (
          fields
        );
      })}
    </>
  );
}
