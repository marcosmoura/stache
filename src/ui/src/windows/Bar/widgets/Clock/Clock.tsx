import { Time03Icon } from '@hugeicons/core-free-icons';

import { Icon } from '@/components/Icon';
import { useBarAction } from '@/queries/barActions';
import { useTicker, type TickerState } from '@/stores';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { formatClock, minuteOf, secondOf } from './Clock.format';

const selectSecond = ({ now }: TickerState) => secondOf(now);
const selectMinute = ({ now }: TickerState) => minuteOf(now);

/**
 * The UI-local time from the shared ticker. With `show_seconds = false` it
 * selects the minute, so it re-renders once a minute. Click: `bar_action("clock")`.
 */
export function Clock({ options }: WidgetProps<'clock'>) {
  const moment = useTicker(options.show_seconds ? selectSecond : selectMinute);
  const run = useBarAction();

  return (
    <BarItem onClick={() => run('clock')}>
      <Icon icon={Time03Icon} strokeWidth={WIDGET_ICON_STROKE} style={widgetIcon.root} />
      <span>{formatClock(new Date(moment), options)}</span>
    </BarItem>
  );
}

export default Clock;
