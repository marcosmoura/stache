import { memo, useCallback } from 'react';

import { LayoutGroup } from 'motion/react';

import { Surface } from '@/components/Surface';

import { Workspace } from '../Workspace';

import * as styles from './WorkspaceList.styles';
import type { WorkspaceListProps } from './WorkspaceList.types';

export const WorkspaceList = memo(function WorkspaceList({
  workspaces,
  focusedWorkspace,
  onSpaceClick,
}: WorkspaceListProps) {
  const handleClick = useCallback((name: string) => onSpaceClick(name), [onSpaceClick]);

  return (
    <LayoutGroup id="workspaces">
      <Surface className={styles.workspaces}>
        {workspaces.map(({ name }) => (
          <Workspace
            key={name}
            name={name}
            isFocused={focusedWorkspace === name}
            onClick={handleClick(name)}
          />
        ))}
      </Surface>
    </LayoutGroup>
  );
});
