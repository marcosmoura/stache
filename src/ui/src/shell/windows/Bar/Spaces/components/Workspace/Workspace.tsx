import { memo, useCallback } from 'react';

import { AnimatePresence, motion } from 'motion/react';

import { Button } from '../../../../../components/Button';
import { Icon } from '../../../../../components/Icon';
import { springTransition, workspaceIcons } from '../../Spaces.constants';

import * as styles from './Workspace.styles';
import type { WorkspaceProps } from './Workspace.types';

export const Workspace = memo(function Workspace({ name, isFocused, onClick }: WorkspaceProps) {
  // Stable per-item closure so memo() isn't defeated on list re-renders.
  const handleClick = useCallback(() => onClick(name), [onClick, name]);
  const icon = Object.hasOwn(workspaceIcons, name) ? workspaceIcons[name] : undefined;

  return (
    <Button className={styles.workspace} onClick={handleClick}>
      {/* Legacy passed `undefined` to Icon for an unknown name, which throws and
          replaced the whole Bar with its error fallback (an empty tiling.toml names
          workspaces `workspace-N`). No fallback icon: the button renders without one. */}
      {icon && <Icon icon={icon} />}
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
