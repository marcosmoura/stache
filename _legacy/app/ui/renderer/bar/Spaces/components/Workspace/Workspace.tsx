import { memo, useCallback } from 'react';

import { AnimatePresence, motion } from 'motion/react';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';

import { springTransition, workspaceIcons } from '../../Spaces.constants';

import * as styles from './Workspace.styles';
import type { WorkspaceProps } from './Workspace.types';

export const Workspace = memo(function Workspace({ name, isFocused, onClick }: WorkspaceProps) {
  // Stable per-item closure so memo() isn't defeated on list re-renders.
  const handleClick = useCallback(() => onClick(name), [onClick, name]);

  return (
    <Button className={styles.workspace} onClick={handleClick}>
      <Icon icon={workspaceIcons[name]} />
      <AnimatePresence initial={false}>
        {isFocused && (
          <motion.div
            layoutId="workspace-indicator"
            transition={springTransition}
            className={styles.workspaceIndicator}
          />
        )}
      </AnimatePresence>
    </Button>
  );
});
