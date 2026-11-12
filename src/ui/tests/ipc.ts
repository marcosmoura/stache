import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';

export type IpcCall = { command: string; args: unknown };

/**
 * Replaces the Tauri IPC with a recorder. Every command is recorded and
 * answered by `handler` (default: `undefined`). `setup.ts` clears the mock
 * after each test.
 */
export function recordIpc(handler: (command: string, args: unknown) => unknown = () => undefined) {
  const calls: IpcCall[] = [];

  mockIPC((command, args) => {
    calls.push({ command, args });

    return handler(command, args);
  });

  return calls;
}

type Answer = unknown | ((args: unknown) => unknown);

type Internals = { runCallback: (id: number, data: unknown) => void };

/**
 * A scripted fake of the host over `mockIPC` with a live event registry.
 * Commands answer from {@link MockHost.answer} (a value, or a function that
 * may return a promise) and reject when unscripted. `listen` resolves at once
 * unless held with {@link MockHost.holdListen} or failed with
 * {@link MockHost.failListen}.
 */
export type MockHost = ReturnType<typeof mockHost>;

export function mockHost() {
  const calls: IpcCall[] = [];
  const answers = new Map<string, Answer>();
  const listeners = new Map<string, Set<number>>();
  const held: Array<() => void> = [];
  let holding = false;
  let listenFailure: string | null = null;

  const register = (event: string, handler: number) => {
    listeners.set(event, (listeners.get(event) ?? new Set()).add(handler));

    return handler;
  };

  mockIPC((command, rawArgs) => {
    const args = rawArgs as Record<string, unknown>;

    calls.push({ command, args });

    if (command === 'plugin:event|listen') {
      const event = String(args.event);
      const handler = Number(args.handler);

      if (listenFailure !== null) {
        throw new Error(listenFailure);
      }

      if (holding) {
        return new Promise((resolve) => {
          held.push(() => resolve(register(event, handler)));
        });
      }

      return register(event, handler);
    }

    if (command === 'plugin:event|unlisten') {
      listeners.get(String(args.event))?.delete(Number(args.eventId));

      return null;
    }

    if (!answers.has(command)) {
      throw new Error(`mock host: no scripted answer for "${command}"`);
    }

    const answer = answers.get(command);

    return typeof answer === 'function' ? (answer as (args: unknown) => unknown)(args) : answer;
  });

  return {
    calls,
    /** Scripts `command` (snake_case IPC name). */
    answer(command: string, value: Answer) {
      answers.set(command, value);

      return this;
    },
    /** Calls of `command`, in order. */
    callsOf(command: string) {
      return calls.filter((call) => call.command === command);
    },
    /** Keeps later `listen` calls pending until {@link releaseListen}. */
    holdListen() {
      holding = true;
    },
    /** Resolves every held `listen` and stops holding. */
    releaseListen() {
      holding = false;
      held.splice(0).forEach((resolve) => resolve());
    },
    /** Makes later `listen` calls reject with `message` (`null` restores). */
    failListen(message: string | null) {
      listenFailure = message;
    },
    /** Live listeners for `event`. */
    listenerCount(event: string) {
      return listeners.get(event)?.size ?? 0;
    },
    /** Delivers `payload` synchronously to every live listener of `event`. */
    emit(event: string, payload: unknown) {
      const internals = (window as unknown as { __TAURI_INTERNALS__: Internals })
        .__TAURI_INTERNALS__;

      for (const id of listeners.get(event) ?? []) {
        internals.runCallback(id, { event, id, payload });
      }
    },
  };
}

/** Lets pending promise callbacks run. */
export async function flush(times = 5) {
  for (let index = 0; index < times; index += 1) {
    await Promise.resolve();
  }
}

/** Gives the page a current Tauri window labelled `label` (for window-level APIs). */
export function mockCurrentWindow(label: string) {
  mockWindows(label);
}
