import { Suspense } from 'react';

import { Stack } from '@/components/Stack';
import { Surface } from '@/components/Surface';

import { AppList } from './components/AppList';
import { WorkspaceList } from './components/WorkspaceList';
import { useSpaces } from './Spaces.state';
import * as styles from './Spaces.styles';

const SpacesContent = () => {
  const { isEnabled, workspaces, focusedApp, focusedWorkspace, apps, onSpaceClick, onAppClick } =
    useSpaces();

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
};

const SpacesFallback = () => (
  <Stack>
    <Surface className={styles.fallback}>Loading...</Surface>
  </Stack>
);

export const Spaces = () => (
  <Suspense fallback={<SpacesFallback />}>
    <SpacesContent />
  </Suspense>
);
