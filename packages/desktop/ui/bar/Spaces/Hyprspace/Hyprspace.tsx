import { useCallback, useMemo } from 'react';

import { cx } from '@linaria/core';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { useTauriEvent } from '@/hooks';
import { CliEvents, type CLIEventPayload } from '@/types';

import {
  fetchCurrentHyprspaceWorkspace,
  fetchHyprspaceWorkspaceList,
  getSortedWorkspaces,
  onCLIEvent,
  onWorkspaceClick,
} from './Hyprspace.service';
import * as styles from './Hyprspace.styles';
import type { WorkspaceList } from './Hyprspace.types';

export const Hyprspace = () => {
  const queryClient = useQueryClient();
  const { data: currentWorkspace } = useQuery({
    queryKey: ['hyprspace_current_workspace'],
    queryFn: fetchCurrentHyprspaceWorkspace,
    refetchOnMount: true,
  });
  const { data: workspaceData } = useQuery({
    queryKey: ['hyprspace_workspaces'],
    queryFn: fetchHyprspaceWorkspaceList,
    refetchOnMount: true,
  });

  useTauriEvent<CLIEventPayload>(CliEvents.COMMAND_RECEIVED, ({ payload }) => {
    onCLIEvent(payload, queryClient);
  });

  const workspaces: WorkspaceList | null = useMemo(
    () => getSortedWorkspaces(workspaceData),
    [workspaceData],
  );

  const onClick = useCallback((workspaceName: string) => () => onWorkspaceClick(workspaceName), []);

  if (!currentWorkspace || workspaceData?.length === 0) {
    return null;
  }

  return (
    <Surface className={styles.spaces}>
      {workspaces?.map(({ name, icon }) => {
        const isActive = name === currentWorkspace.workspace;
        const capitalizedName = name.charAt(0).toUpperCase() + name.slice(1);

        return (
          <Button
            key={name}
            className={cx(styles.space, isActive && styles.spaceActive)}
            active={isActive}
            onClick={onClick(name)}
          >
            <Icon icon={icon} />
            {isActive && <span>{capitalizedName}</span>}
          </Button>
        );
      })}
    </Surface>
  );
};
