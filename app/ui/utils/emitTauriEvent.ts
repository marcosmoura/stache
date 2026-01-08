import { emitTo } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

type EmitOptions<T extends object> = {
  eventName: string;
  target?: string;
  payload?: T;
};

export const emitTauriEvent = <T extends object>({
  eventName,
  target,
  payload,
}: EmitOptions<T>) => {
  if (!target) {
    const window = getCurrentWindow();

    emitTo(window.label, eventName, payload);
  } else {
    emitTo(target, eventName, payload);
  }
};
