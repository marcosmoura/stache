import type { KeepAwakeSession } from '../Bar.data';

export type KeepAwakeState = {
  /** `undefined` until the confirmed Keep Awake state is known. */
  isSystemAwake: boolean | undefined;
  /** The session facts the click is based on; `undefined` while unresolved. */
  session: KeepAwakeSession | undefined;
};
