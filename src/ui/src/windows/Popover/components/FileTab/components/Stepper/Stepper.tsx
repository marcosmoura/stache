import { ArrowDown01Icon, ArrowUp01Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';

import { styles } from './Stepper.style';

export type StepperProps = {
  /** The field's label, for "Increase …" and "Decrease …". */
  label: string;
  value: unknown;
  minimum: number | null;
  maximum: number | null;
  disabled: boolean;
  onStep: (delta: 1 | -1) => void;
};

/** A small NSStepper: two stacked arrow buttons beside a number field. */
export function Stepper({ label, value, minimum, maximum, disabled, onStep }: StepperProps) {
  return (
    <span {...stylex.props(styles.stepper)}>
      <button
        type="button"
        aria-label={`Increase ${label}`}
        title={`Increase ${label}`}
        disabled={disabled || (maximum !== null && value === maximum)}
        onClick={() => onStep(1)}
        {...stylex.props(styles.step)}
      >
        <Icon icon={ArrowUp01Icon} size={9} strokeWidth={2.5} />
      </button>
      <button
        type="button"
        aria-label={`Decrease ${label}`}
        title={`Decrease ${label}`}
        disabled={disabled || (minimum !== null && value === minimum)}
        onClick={() => onStep(-1)}
        {...stylex.props(styles.step)}
      >
        <Icon icon={ArrowDown01Icon} size={9} strokeWidth={2.5} />
      </button>
    </span>
  );
}
