import * as stylex from '@stylexjs/stylex';

import { space } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  form: { display: 'flex', flexDirection: 'column', gap: space.md },
});
