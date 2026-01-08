import { cx } from '@linaria/core';

import * as styles from './Button.styles';
import type { ButtonProps } from './Button.types';

export const Button = ({
  type = 'button',
  active = false,
  children,
  className,
  ref,
  ...rest
}: ButtonProps) => (
  <button
    ref={ref}
    type={type}
    className={cx(styles.button, active && styles.buttonActive, className)}
    {...rest}
  >
    {children}
  </button>
);
