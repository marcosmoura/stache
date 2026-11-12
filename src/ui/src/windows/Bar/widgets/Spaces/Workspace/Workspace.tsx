import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';
import { useReducedMotion } from 'motion/react';
import * as m from 'motion/react-m';

import { Icon } from '@/components/Icon';
import { barSnapshotOptions } from '@/queries/bar';
import { transitions } from '@/theme/motion';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../../styles/widgetIcon.style';
import { workspaceIcon } from '../icons';
import { isFocusedWorkspace } from '../Spaces.state';

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
 * It reads only its own focus, so another workspace's change does not
 * re-render it.
 */
export function Workspace({ name, highlightId, onFocus }: WorkspaceProps) {
  const { data: active = false } = useQuery({
    ...barSnapshotOptions(),
    select: (outcome) => isFocusedWorkspace(outcome, name),
  });
  const reduced = useReducedMotion() ?? false;

  return (
    <button
      type="button"
      aria-label={name}
      aria-pressed={active}
      data-active={active || undefined}
      onClick={() => onFocus(name)}
      {...stylex.props(styles.root)}
    >
      <Icon icon={workspaceIcon(name)} strokeWidth={WIDGET_ICON_STROKE} style={widgetIcon.root} />
      {/* No exit: the highlight leaves at once and the new one flies in from its place. */}
      {active && (
        <m.span
          layoutId={highlightId}
          data-highlight
          transition={reduced ? INSTANT : transitions.spring}
          {...stylex.props(styles.highlight)}
        />
      )}
    </button>
  );
}

export default Workspace;
