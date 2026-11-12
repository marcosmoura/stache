import * as stylex from '@stylexjs/stylex';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { AnimatePresence } from 'motion/react';

import { useCompact } from '@/hooks/useMediaQuery';
import type { BarTilingTarget } from '@/ipc';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarTilingAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import type { WidgetProps } from '../types';

import { App } from './App';
import { renderedStrip, stripShape } from './Spaces.state';
import { styles } from './Spaces.style';
import { Workspace } from './Workspace';

/**
 * Tiling's strip: the workspaces (in Tiling's published order, the focused
 * one highlighted) and the windows of the focused workspace (the focused one
 * labelled). Nothing for
 * no Tiling, `absent` or zero workspaces; `Loading...` while Tiling has not
 * published. The strip selects only names and keys; each item selects its own
 * focus and label, so a focus change re-renders only the items it touches.
 */
export function Spaces({ id }: WidgetProps<'spaces'>) {
  const compact = useCompact();
  const { data: shape = null } = useQuery({
    ...barSnapshotOptions(),
    select: (outcome) => stripShape(outcome, compact),
  });
  const client = useQueryClient();
  const run = useBarTilingAction();

  // A click echoes the generation and sequence of the strip on screen; nothing
  // is sent unless that strip is available.
  const focus = (action: BarTilingTarget) => {
    const rendered = renderedStrip(client.getQueryData(barSnapshotOptions().queryKey));

    if (rendered) {
      run({ ...rendered, action });
    }
  };
  const focusWorkspace = (name: string) => focus({ kind: 'focusWorkspace', name });
  const focusWindow = (window: string) => focus({ kind: 'focusWindow', window });

  if (!shape) {
    return null;
  }

  if (shape.kind === 'loading') {
    return <BarItem>Loading...</BarItem>;
  }

  return (
    <div data-spaces {...stylex.props(styles.row)}>
      <BarItem label="Workspaces" style={styles.workspaces}>
        {shape.workspaces.map((name) => (
          <Workspace
            key={name}
            name={name}
            highlightId={`spaces-highlight-${id}`}
            onFocus={focusWorkspace}
          />
        ))}
      </BarItem>
      <AnimatePresence>
        {shape.apps.map((key) => (
          <App key={key} windowKey={key} compact={compact} onFocus={focusWindow} />
        ))}
      </AnimatePresence>
    </div>
  );
}

export default Spaces;
