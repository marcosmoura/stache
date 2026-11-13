import { PowerIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import { useQuitApp } from '@/queries/access';

import { styles } from './Footer.style';

/** The Overview's footer actions. */
export function Footer() {
  const quit = useQuitApp();

  return (
    <nav {...stylex.props(styles.footer)} aria-label="Actions">
      <button
        type="button"
        {...stylex.props(styles.action)}
        disabled={quit.isPending}
        onClick={() => quit.mutate()}
      >
        <Icon icon={PowerIcon} size={16} />
        Quit Stache Next
      </button>
    </nav>
  );
}
