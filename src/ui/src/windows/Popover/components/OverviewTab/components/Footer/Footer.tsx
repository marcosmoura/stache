import { PowerIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { PRODUCT_NAME } from '@/identity';
import { useQuitApp } from '@/queries/access';
import { appInfoOptions } from '@/queries/host';

import { styles } from './Footer.style';

/** The Overview's footer actions. */
export function Footer() {
  const quit = useQuitApp();
  const { data: app } = useQuery(appInfoOptions());

  return (
    <nav {...stylex.props(styles.footer)} aria-label="Actions">
      <button
        type="button"
        {...stylex.props(styles.action)}
        disabled={quit.isPending}
        onClick={() => quit.mutate()}
      >
        <Icon icon={PowerIcon} size={16} />
        Quit {app?.name ?? PRODUCT_NAME}
      </button>
    </nav>
  );
}
