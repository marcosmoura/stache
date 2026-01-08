import { Stack } from '@/components/Stack';

import { AppList } from './components/AppList';
import { WorkspaceList } from './components/WorkspaceList';

import { useSpaces } from './Spaces.state';

export const Spaces = () => {
  const { workspaces, focusedApp, focusedWorkspace, apps, onSpaceClick, onAppClick } = useSpaces();

  if (!workspaces.length) {
    return null;
  }

  return (
    <Stack data-test-id="spaces-container">
      <WorkspaceList
        workspaces={workspaces}
        focusedWorkspace={focusedWorkspace}
        onSpaceClick={onSpaceClick}
      />

      <AppList apps={apps} focusedApp={focusedApp} onAppClick={onAppClick} />
    </Stack>
  );
};
