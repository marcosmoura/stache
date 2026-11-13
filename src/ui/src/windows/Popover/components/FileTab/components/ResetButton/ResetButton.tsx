import { Undo02Icon } from '@hugeicons/core-free-icons';

import { RoundButton } from '../RoundButton';

/** "Reset to default": removes a set key from its table. */
export function ResetButton({
  label,
  onReset,
  disabled,
}: {
  label: string;
  onReset: () => void;
  disabled: boolean;
}) {
  return (
    <RoundButton
      label={`Reset ${label} to default`}
      title="Reset to default"
      icon={Undo02Icon}
      disabled={disabled}
      onClick={onReset}
    />
  );
}
