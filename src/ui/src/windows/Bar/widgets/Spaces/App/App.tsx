import { useState } from 'react';

import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';
import { AnimatePresence, useReducedMotion } from 'motion/react';
import * as m from 'motion/react-m';

import { Icon } from '@/components/Icon';
import { barSnapshotOptions } from '@/queries/bar';
import { durations, easings } from '@/theme/motion';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../../styles/widgetIcon.style';
import { appIcon } from '../icons';
import { appView, revealDuration, type AppView } from '../Spaces.state';

import { styles } from './App.style';

export type AppProps = {
  /** The strip's window key. */
  windowKey: string;
  compact: boolean;
  onFocus: (windowKey: string) => void;
};

/** The opacity of an app that is not focused (today's 0.68). */
export const UNFOCUSED_OPACITY = 0.68;

const HIDDEN = { width: 0, opacity: 0 };
const REVEALED = { width: 'auto', opacity: 1 };

/**
 * One window of the focused workspace: its app icon, and its label while it
 * is the focused window. The item enters and leaves by width and opacity; the
 * label is revealed the same way, over a duration that grows with its length.
 * It reads only its own entry, so another app's change does not re-render it.
 */
export function App({ windowKey, compact, onFocus }: AppProps) {
  const { data: view = null } = useQuery({
    ...barSnapshotOptions(),
    select: (outcome) => appView(outcome, windowKey, compact),
  });
  // While it leaves, the strip no longer lists it: keep drawing the last entry.
  const [last, setLast] = useState<AppView | null>(view);

  if (view && view !== last) {
    setLast(view);
  }

  const reduced = useReducedMotion() ?? false;
  const shown = view ?? last;

  if (!shown) {
    return null;
  }

  const { appName, label, focused } = shown;
  const reveal = reduced
    ? { duration: 0 }
    : { duration: revealDuration(label, durations.bar), ease: easings.inOut };

  return (
    <m.div
      data-app={windowKey}
      initial={HIDDEN}
      animate={{ width: 'auto', opacity: focused ? 1 : UNFOCUSED_OPACITY }}
      exit={HIDDEN}
      transition={{
        ...reveal,
        opacity: reduced ? { duration: 0 } : { duration: durations.bar, ease: easings.inOut },
      }}
      {...stylex.props(styles.presence)}
    >
      <BarItem onClick={() => onFocus(windowKey)} label={focused ? undefined : label}>
        <Icon icon={appIcon(appName)} strokeWidth={WIDGET_ICON_STROKE} style={widgetIcon.root} />
        <AnimatePresence initial={false}>
          {focused && (
            <m.span
              key={label}
              data-label
              initial={HIDDEN}
              animate={REVEALED}
              exit={HIDDEN}
              transition={reveal}
              {...stylex.props(styles.label)}
            >
              {label}
            </m.span>
          )}
        </AnimatePresence>
      </BarItem>
    </m.div>
  );
}

export default App;
