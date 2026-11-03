export type KeepAwakeState = {
  isSystemAwake: boolean;
  onKeepAwakeClick: () => Promise<void>;
};
