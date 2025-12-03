import { css } from '@linaria/core';

import { motion } from '@/design-system';

export const icon = css`
  transition: ${motion.easing} ${motion.duration};
  transition-property: stroke, fill;
`;
