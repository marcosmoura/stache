import * as stylex from '@stylexjs/stylex';

import { Icon, type IconSource } from '@/components/Icon';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

export type RoundButtonProps = {
  /** The accessible name (and the tooltip unless `title` is given). */
  label: string;
  icon: IconSource;
  title?: string;
  disabled?: boolean;
  onClick: () => void;
};

/** A small borderless icon button: reset, move, remove. */
export function RoundButton({ label, icon, title, disabled, onClick }: RoundButtonProps) {
  return (
    <button
      type="button"
      aria-label={label}
      title={title ?? label}
      disabled={disabled}
      onClick={onClick}
      {...stylex.props(controls.round)}
    >
      <Icon icon={icon} size={14} />
    </button>
  );
}
