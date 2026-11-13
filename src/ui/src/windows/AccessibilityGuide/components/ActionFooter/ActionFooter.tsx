import { Loading03Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';

import { styles } from './ActionFooter.style';

/** One footer button. */
export type FooterAction = {
  /** Stable key, also `data-action`. */
  id: string;
  label: string;
  /** While true the button is disabled and shows a spinner. */
  pending: boolean;
  onRun: () => void;
};

export type ActionFooterProps = {
  /** The first action is the primary one and spans the footer. */
  actions: readonly FooterAction[];
};

/** The guide's fixed footer of command buttons. */
export function ActionFooter({ actions }: ActionFooterProps) {
  return (
    <footer {...stylex.props(styles.footer)}>
      {actions.map(({ id, label, pending, onRun }, index) => (
        <button
          key={id}
          type="button"
          {...stylex.props(styles.button, index === 0 && styles.primary)}
          data-action={id}
          disabled={pending}
          aria-busy={pending || undefined}
          onClick={onRun}
        >
          {pending ? (
            <span {...stylex.props(styles.spinner)} data-spinner>
              <Icon icon={Loading03Icon} size={12} strokeWidth={1.75} />
            </span>
          ) : null}
          {label}
        </button>
      ))}
    </footer>
  );
}
