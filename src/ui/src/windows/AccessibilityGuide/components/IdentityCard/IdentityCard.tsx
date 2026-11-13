import { InformationCircleIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';

import { styles } from './IdentityCard.style';

export type IdentityCardProps = {
  /** Product name; `PRODUCT_NAME` until `app_info` answers. */
  name: string;
  /** Bundle identifier; the row is omitted until `app_info` answers. */
  identifier: string | null;
  /** The enclosing `.app` path, or `null` for an unbundled binary. */
  appPath: string | null;
};

function Field({ label, children }: { label: string; children: string }) {
  return (
    <div {...stylex.props(styles.field)}>
      <dt {...stylex.props(styles.term)}>{label}</dt>
      <dd {...stylex.props(styles.value)}>{children}</dd>
    </div>
  );
}

/** Which app the user must turn on: name, bundle identifier and path, with a hint. */
export function IdentityCard({ name, identifier, appPath }: IdentityCardProps) {
  return (
    <div {...stylex.props(styles.card)}>
      <dl {...stylex.props(styles.fields)}>
        <Field label="Name">{name}</Field>
        {identifier === null ? null : <Field label="Bundle ID">{identifier}</Field>}
        <Field label="Path">{appPath ?? 'not an app bundle'}</Field>
      </dl>
      <p {...stylex.props(styles.hint)}>
        <span {...stylex.props(styles.hintIcon)}>
          <Icon icon={InformationCircleIcon} size={13} strokeWidth={1.75} />
        </span>
        <span>Make sure you turn on Stache Next, not Stache.</span>
      </p>
    </div>
  );
}
