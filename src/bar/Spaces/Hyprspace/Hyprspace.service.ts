import {
  AppleFinderIcon,
  AppleReminderIcon,
  BrowserIcon,
  CircleIcon,
  CodeIcon,
  ComputerTerminal01Icon,
  DashboardCircleIcon,
  FigmaIcon,
  Mail01Icon,
  SlackIcon,
  SpotifyIcon,
} from '@hugeicons/core-free-icons';
import type { QueryClient } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import type { CLIEventPayload } from '@/types';

import type {
  HyprspaceCurrentWorkspacePayload,
  HyprspaceWorkspacesPayload,
} from './Hyprspace.types';

const defaultWorkspaces = [
  {
    name: 'terminal',
    icon: ComputerTerminal01Icon,
  },
  {
    name: 'coding',
    icon: CodeIcon,
  },
  {
    name: 'browser',
    icon: BrowserIcon,
  },
  {
    name: 'music',
    icon: SpotifyIcon,
  },
  {
    name: 'design',
    icon: FigmaIcon,
  },
  {
    name: 'communication',
    icon: SlackIcon,
  },
  {
    name: 'misc',
    icon: DashboardCircleIcon,
  },
  {
    name: 'files',
    icon: AppleFinderIcon,
  },
  {
    name: 'mail',
    icon: Mail01Icon,
  },
  {
    name: 'tasks',
    icon: AppleReminderIcon,
  },
];

export const fetchHyprspaceWorkspaceList = async () =>
  invoke<HyprspaceWorkspacesPayload>('get_hyprspace_workspaces');

export const fetchCurrentHyprspaceWorkspace = async () =>
  invoke<HyprspaceCurrentWorkspacePayload>('get_hyprspace_focused_workspace');

export const onCLIEvent = ({ name, data }: CLIEventPayload, queryClient: QueryClient) => {
  const workspacesQueryKey = ['hyprspace_workspaces'];
  const currentWorkspaceQueryKey = ['hyprspace_current_workspace'];

  if (name === 'workspace-changed') {
    queryClient.cancelQueries({ queryKey: currentWorkspaceQueryKey });
    queryClient.setQueryData<HyprspaceCurrentWorkspacePayload>(currentWorkspaceQueryKey, {
      workspace: data || '',
    });
    queryClient.refetchQueries({ queryKey: workspacesQueryKey });
  }

  if (name === 'focus-changed') {
    queryClient.refetchQueries({ queryKey: currentWorkspaceQueryKey });
    queryClient.refetchQueries({ queryKey: workspacesQueryKey });
  }
};

const findWorkspaceIndex = (workspaceName: string) => {
  return defaultWorkspaces.findIndex(({ name }) => name === workspaceName);
};

export const getSortedWorkspaces = (workspaceData?: HyprspaceWorkspacesPayload) => {
  if (!workspaceData) {
    return null;
  }

  workspaceData.sort((a, b) => {
    const indexA = findWorkspaceIndex(a.workspace);
    const indexB = findWorkspaceIndex(b.workspace);

    return indexA - indexB;
  });

  return workspaceData.map(({ workspace }) => ({
    name: workspace,
    icon: defaultWorkspaces.find(({ name }) => name === workspace)?.icon || CircleIcon,
  }));
};

export const onWorkspaceClick = async (workspaceName: string) => {
  try {
    await invoke('go_to_hyprspace_workspace', { workspace: workspaceName });
  } catch (error) {
    console.error('Error switching workspace:', error);
  }
};
