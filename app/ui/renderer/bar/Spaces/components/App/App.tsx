import { memo, useMemo } from 'react';

import { cx } from '@linaria/core';
import { AnimatePresence, motion } from 'motion/react';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { motionRaw } from '@/design-system';

import { ease, getAppIcon } from '../../Spaces.constants';

import * as styles from './App.styles';
import type { AppProps } from './App.types';

const revealInitial = { width: 0, opacity: 0 };
const revealAnimate = { width: 'auto', opacity: 1 };

export const App = memo(function App({
  appName,
  displayName,
  windowId,
  isFocused,
  onClick,
}: AppProps) {
  const transition = useMemo(
    () => ({
      duration: Math.max(motionRaw.duration, (displayName.length * motionRaw.duration) / 15),
      ease,
    }),
    [displayName.length],
  );

  return (
    <motion.div
      key={windowId}
      initial={revealInitial}
      animate={revealAnimate}
      exit={revealInitial}
      transition={transition}
    >
      <Surface
        as={Button}
        className={cx(styles.app, isFocused && styles.appFocused)}
        onClick={onClick}
      >
        <Icon icon={getAppIcon(appName)} />
        <AnimatePresence>
          {isFocused && (
            <motion.div
              key={displayName}
              initial={revealInitial}
              animate={revealAnimate}
              exit={revealInitial}
              transition={transition}
              className={styles.appLabel}
            >
              {displayName}
            </motion.div>
          )}
        </AnimatePresence>
      </Surface>
    </motion.div>
  );
});
