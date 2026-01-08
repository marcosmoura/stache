import { useEffect, useRef } from 'react';

import { listen, type EventCallback, type UnlistenFn } from '@tauri-apps/api/event';

export function useTauriEvent<TPayload>(
  eventName: string,
  onEvent: EventCallback<TPayload>,
  targetWindowLabel?: string,
) {
  // Use a ref to store the callback so we don't re-subscribe on every render
  const callbackRef = useRef(onEvent);

  // Keep the ref up to date with the latest callback
  useEffect(() => {
    callbackRef.current = onEvent;
  });

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let isSubscribed = true;

    (async () => {
      const target = targetWindowLabel ? { target: targetWindowLabel } : undefined;

      // Use a stable wrapper that delegates to the ref
      const stableCallback: EventCallback<TPayload> = (event) => {
        callbackRef.current(event);
      };

      const unlistenFn = await listen<TPayload>(eventName, stableCallback, target);

      // Only store unlisten if we're still subscribed (component hasn't unmounted)
      if (isSubscribed) {
        unlisten = unlistenFn;
      } else {
        // Component unmounted while we were setting up - clean up immediately
        unlistenFn();
      }
    })();

    return () => {
      isSubscribed = false;
      unlisten?.();
    };
  }, [eventName, targetWindowLabel]);
}
