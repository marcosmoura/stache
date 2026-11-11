import { Stack } from '../../../components/Stack';
import { Surface } from '../../../components/Surface';

import { AppList } from './components/AppList';
import { WorkspaceList } from './components/WorkspaceList';
import { useSpaces } from './Spaces.state';
import * as styles from './Spaces.styles';

/**
 * Legacy shown this while its tiling queries were suspended; it now shows while
 * Tiling runs and has not published its first state (`BarView.tiling` `loading`).
 */
export function SpacesFallback() {
  return (
    <Stack>
      <Surface className={styles.fallback}>Loading...</Surface>
    </Stack>
  );
}

/**
 * The Tiling strip. Unresolved, absent or empty Tiling keeps legacy's empty
 * `<div />` so the grid's first column stays in place and Media/Status do not move.
 */
export function Spaces() {
  const {
    isEnabled,
    isLoading,
    workspaces,
    focusedApp,
    focusedWorkspace,
    apps,
    onSpaceClick,
    onAppClick,
  } = useSpaces();

  if (isLoading) {
    return <SpacesFallback />;
  }

  if (!isEnabled || !workspaces.length) {
    return <div />;
  }

  return (
    <Stack data-testid="spaces-container">
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace={focusedWorkspace}
        onSpaceClick={onSpaceClick}
      />

      <AppList apps={apps} focusedApp={focusedApp} onAppClick={onAppClick} />
    </Stack>
  );
}
