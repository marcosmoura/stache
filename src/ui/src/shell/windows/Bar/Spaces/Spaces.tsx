import { Suspense } from 'react';

import { Stack } from '../../../components/Stack';
import { Surface } from '../../../components/Surface';

import * as styles from './Spaces.styles';

/**
 * Tiling is not part of the Bar yet (no workspace or app strip), so legacy's
 * "tiling absent" branch is the only one: the empty slot that keeps the grid's
 * first column in place so Media and Status do not move.
 */
function SpacesContent() {
  return <div />;
}

/**
 * Legacy shown this while its tiling queries were suspended. No Bar data source
 * can suspend today, so it is unreachable in production; it is kept
 * value-identical for the tiling integration and rendered directly by tests.
 */
export function SpacesFallback() {
  return (
    <Stack>
      <Surface className={styles.fallback}>Loading...</Surface>
    </Stack>
  );
}

export function Spaces() {
  return (
    <Suspense fallback={<SpacesFallback />}>
      <SpacesContent />
    </Suspense>
  );
}
