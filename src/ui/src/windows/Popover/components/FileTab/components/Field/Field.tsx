import type { FieldProps } from '../../utils/fields';
import { isScalar } from '../../utils/schema';
import { OptionalBlock } from '../OptionalBlock';
import { ScalarRow } from '../ScalarRow';
import { TableBlock } from '../TableBlock';
import { UnionBlock } from '../UnionBlock';
import { UnknownRow } from '../UnknownRow';

/** One field of the tree, rendered by the component for its kind. */
export function Field(props: FieldProps) {
  const { node } = props;

  // A constant (a Bar item's widget tag) is not a setting: the form shows nothing.
  if (node.kind === 'const') {
    return null;
  }

  if (isScalar(node)) {
    return <ScalarRow {...props} />;
  }

  switch (node.kind) {
    case 'optional':
      return <OptionalBlock {...props} node={node} />;
    case 'union':
      return <UnionBlock {...props} node={node} />;
    case 'object':
    case 'map':
    case 'array':
      return <TableBlock {...props} />;
    default:
      return <UnknownRow {...props} />;
  }
}
