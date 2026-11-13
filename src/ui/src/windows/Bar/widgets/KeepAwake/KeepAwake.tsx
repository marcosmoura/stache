import { Coffee02Icon } from '@hugeicons/core-free-icons';
import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarSessionAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { keepAwakeAction, keepAwakeLabel, selectKeepAwake } from './KeepAwake.state';
import { styles } from './KeepAwake.style';

/**
 * The Keep Awake indicator: a coffee icon, filled in the success color while
 * the system is kept awake, outlined otherwise. Hidden until the first
 * observation. Click: pause or resume, from the rendered `paused` flag.
 */
export function KeepAwake(_props: WidgetProps<'keep_awake'>) {
  const { data: indicator } = useQuery({ ...barSnapshotOptions(), select: selectKeepAwake });
  const run = useBarSessionAction();

  if (!indicator) {
    return null;
  }

  return (
    <BarItem label={keepAwakeLabel(indicator)} onClick={() => run(keepAwakeAction(indicator))}>
      <Icon
        icon={Coffee02Icon}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon.root, styles.icon, indicator.active && styles.awake]}
      />
    </BarItem>
  );
}

export default KeepAwake;
