import { useEffect, useState } from 'react';

import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';
import { useReducedMotion } from 'motion/react';
import * as m from 'motion/react-m';

import { Icon } from '@/components/Icon';
import { barSnapshotOptions } from '@/queries/bar';
import { durations, transitions } from '@/theme/motion';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../../styles/widgetIcon.style';
import { workspaceIcon } from '../icons';
import { isFocusedWorkspace, isInactiveWorkspace, refusalIdentity } from '../Spaces.state';

import { styles } from './Workspace.style';

export type WorkspaceProps = {
  name: string;
  /** The `layoutId` of the strip's shared highlight. */
  highlightId: string;
  onFocus: (name: string) => void;
};

const INSTANT = { duration: 0 };

/**
 * One workspace: an icon-only button named by the workspace. The focused one
 * is marked active and holds the highlight, which moves between workspaces.
 * An inactive one is dimmed and `aria-disabled` but still sends its click;
 * Tiling decides. When Tiling publishes a new refusal of its focus, it
 * flashes once; the refusal present at its first render does not flash.
 * It reads only its own values, so another workspace's change does not
 * re-render it.
 */
export function Workspace({ name, highlightId, onFocus }: WorkspaceProps) {
  const { data: active = false } = useQuery({
    ...barSnapshotOptions(),
    select: (outcome) => isFocusedWorkspace(outcome, name),
  });
  const { data: inactive = false } = useQuery({
    ...barSnapshotOptions(),
    select: (outcome) => isInactiveWorkspace(outcome, name),
  });
  const { data: refusal = null } = useQuery({
    ...barSnapshotOptions(),
    select: (outcome) => refusalIdentity(outcome, name),
  });
  const reduced = useReducedMotion() ?? false;
  // The refusal last seen (initially the one at first render, which never
  // flashes) and the flash: `key` restarts it, `on` while it shows.
  const [seen, setSeen] = useState(refusal);
  const [flash, setFlash] = useState({ key: 0, on: false });

  if (refusal !== seen) {
    setSeen(refusal);

    if (refusal !== null) {
      setFlash({ key: flash.key + 1, on: true });
    }
  }

  const endFlash = () => setFlash((current) => ({ ...current, on: false }));

  // Under reduced motion the tint shows, unanimated, for the flash's duration.
  useEffect(() => {
    if (!flash.on || !reduced) {
      return;
    }

    const timer = setTimeout(endFlash, durations.flash * 1000);

    return () => clearTimeout(timer);
  }, [flash, reduced]);

  return (
    <button
      type="button"
      aria-label={name}
      aria-pressed={active}
      aria-disabled={inactive || undefined}
      data-active={active || undefined}
      data-inactive={inactive || undefined}
      data-flashing={flash.on || undefined}
      onClick={() => onFocus(name)}
      {...stylex.props(styles.root, inactive && styles.inactive)}
    >
      <Icon
        icon={workspaceIcon(name)}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon.root, inactive && !flash.on && styles.inactiveIcon]}
      />
      {/* No exit: the highlight leaves at once and the new one flies in from its place. */}
      {active && (
        <m.span
          layoutId={highlightId}
          data-highlight
          transition={reduced ? INSTANT : transitions.spring}
          {...stylex.props(styles.highlight)}
        />
      )}
      {flash.on &&
        (reduced ? (
          <span key={flash.key} data-flash {...stylex.props(styles.flash)} />
        ) : (
          <m.span
            key={flash.key}
            data-flash
            initial={{ opacity: 0 }}
            animate={{ opacity: [0, 1, 0] }}
            transition={transitions.flash}
            onAnimationComplete={endFlash}
            {...stylex.props(styles.flash)}
          />
        ))}
    </button>
  );
}

export default Workspace;
