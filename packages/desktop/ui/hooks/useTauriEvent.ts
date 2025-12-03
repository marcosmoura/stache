import { useEffect } from 'react';

import { listen, type EventCallback, type UnlistenFn } from '@tauri-apps/api/event';

export function useTauriEvent<TPayload>(eventName: string, onEvent: EventCallback<TPayload>) {
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;

    (async () => {
      unlisten = await listen<TPayload>(eventName, onEvent);
    })();

    return () => unlisten?.();
  }, [eventName, onEvent]);
}
