import { useBarData } from '../Bar.data';

import type { KeepAwakeState } from './KeepAwake.types';

export function useKeepAwake(): KeepAwakeState {
  const { keepAwake: isSystemAwake, keepAwakeSession: session } = useBarData();

  return { isSystemAwake, session };
}
