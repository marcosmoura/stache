import type { EventCallback, UnlistenFn } from '@tauri-apps/api/event';

/** A generated event (`events.statusChanged`, `events.barChanged`, ...). */
export type HostEvent<TPayload> = {
  listen: (callback: EventCallback<TPayload>) => Promise<UnlistenFn>;
};

export type SubscribeOptions = {
  /** Called once the host confirmed the listener (and never after disposal). */
  onReady?: () => void;
  /** Called when `listen` rejects (and never after disposal). */
  onError?: (error: unknown) => void;
};

/**
 * Listens to a generated event and returns a synchronous dispose. When
 * `listen` resolves after disposal the listener is removed at once and
 * `onReady` is not called; the handler never runs after disposal.
 */
export function subscribe<TPayload>(
  event: HostEvent<TPayload>,
  handler: (payload: TPayload) => void,
  options: SubscribeOptions = {},
): () => void {
  let disposed = false;
  let unlisten: UnlistenFn | undefined;

  event
    .listen((message) => {
      if (!disposed) {
        handler(message.payload);
      }
    })
    .then(
      (stop) => {
        if (disposed) {
          stop();

          return;
        }

        unlisten = stop;
        options.onReady?.();
      },
      (error: unknown) => {
        if (!disposed) {
          options.onError?.(error);
        }
      },
    );

  return () => {
    if (disposed) {
      return;
    }

    disposed = true;
    unlisten?.();
    unlisten = undefined;
  };
}
