import type { FieldProps } from '../../utils/fields';
import { isScalar } from '../../utils/schema';
import { CardsBody } from '../CardsBody';
import { Field } from '../Field';
import { ListBody } from '../ListBody';
import { MapBody } from '../MapBody';
import { ObjectBody } from '../ObjectBody';

/** The body of a table, map or list (without a header). */
export function Body({ node, path, at, label, depth }: FieldProps) {
  switch (node.kind) {
    case 'object':
      return <ObjectBody node={node} path={path} at={at} depth={depth} />;
    case 'map':
      return <MapBody node={node} path={path} at={at} depth={depth} />;
    case 'array':
      return isScalar(node.items) ? (
        <ListBody node={node} path={path} at={at} label={label} depth={depth} />
      ) : (
        <CardsBody node={node} path={path} at={at} depth={depth} />
      );
    default:
      return (
        <Field node={node} path={path} at={at} label={label} inTable={false} depth={depth} bare />
      );
  }
}
