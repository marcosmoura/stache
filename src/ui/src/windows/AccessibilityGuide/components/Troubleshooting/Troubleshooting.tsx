import { useId, useState } from 'react';

import { ArrowRight01Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';

import { styles } from './Troubleshooting.style';

export type TroubleshootingProps = {
  /** Whether the running executable is inside an `.app` bundle. */
  bundled: boolean;
};

/** A collapsed disclosure with the fix for the common failure of each launch kind. */
export function Troubleshooting({ bundled }: TroubleshootingProps) {
  const [open, setOpen] = useState(false);
  const id = useId();

  return (
    <div {...stylex.props(styles.root)}>
      <button
        type="button"
        {...stylex.props(styles.summary)}
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen(!open)}
      >
        <span {...stylex.props(styles.chevron, open && styles.chevronOpen)} data-chevron>
          <Icon icon={ArrowRight01Icon} size={14} strokeWidth={1.75} />
        </span>
        Troubleshooting
      </button>
      {open ? (
        <p {...stylex.props(styles.text)} id={id}>
          {bundled
            ? 'Stache Next is already on but this window stays open? The entry belongs to an older build. Select it, remove it with −, then turn on the new entry or add the app at the path above.'
            : 'This copy is not an app bundle. macOS may assign the permission to the app that launched it, such as your terminal.'}
        </p>
      ) : null}
    </div>
  );
}
