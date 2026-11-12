import type { Page } from '@playwright/test';

import type {
  BarChanged_Serialize,
  BarSnapshotOutcome_Serialize,
  HostStatus_Serialize,
} from '../src/ipc/bindings.gen';

/** One recorded IPC call, in call order. Event-plugin calls are recorded too. */
export type HostCall = { command: string; args: unknown };

/** Answers one command: a value, or a function of the call arguments. */
export type CommandAnswer = unknown | ((args: unknown) => unknown | Promise<unknown>);

type Reply = { ok: unknown } | { error: string } | { event: true };

type FakeHostWindow = Window & {
  __TAURI_INTERNALS__?: unknown;
  __TAURI_EVENT_PLUGIN_INTERNALS__?: unknown;
  __stacheFakeHostInvoke?: (command: string, args: unknown) => Promise<Reply>;
  __stacheFakeHostEmit?: (event: string, payload: unknown) => number;
};

const INVOKE_BINDING = '__stacheFakeHostInvoke';

/**
 * Page side of the fake host. Runs as an init script before any page script,
 * so `@tauri-apps/api` finds its internals on first use. Self-contained: it is
 * serialized into the page and cannot reference module scope.
 */
function installTauriInternals() {
  const page = window as FakeHostWindow;
  const callbacks = new Map<number, (data: unknown) => void>();
  const listeners = new Map<string, number[]>();
  let nextId = 1;

  const removeListener = (event: string, id: number) => {
    const ids = listeners.get(event) ?? [];

    listeners.set(
      event,
      ids.filter((candidate) => candidate !== id),
    );
    callbacks.delete(id);
  };

  // The window label follows the surface, as the native host labels its windows.
  const label = new URLSearchParams(window.location.search).get('surface') || 'main';

  page.__TAURI_INTERNALS__ = {
    metadata: {
      currentWindow: { label },
      currentWebview: { windowLabel: label, label },
    },
    plugins: { path: { sep: '/', delimiter: ':' } },
    callbacks,
    transformCallback(callback?: (data: unknown) => void, once = false) {
      const id = nextId++;

      callbacks.set(id, (data) => {
        if (once) {
          callbacks.delete(id);
        }

        callback?.(data);
      });

      return id;
    },
    unregisterCallback(id: number) {
      callbacks.delete(id);
    },
    runCallback(id: number, data: unknown) {
      callbacks.get(id)?.(data);
    },
    convertFileSrc(filePath: string, protocol = 'asset') {
      return `${protocol}://localhost/${encodeURIComponent(filePath)}`;
    },
    async invoke(command: string, args: Record<string, unknown> = {}) {
      const bridge = page.__stacheFakeHostInvoke;

      if (!bridge) {
        throw new Error('fake host: the Node bridge is not installed');
      }

      const reply = await bridge(command, args);

      if ('error' in reply) {
        throw reply.error;
      }

      if ('ok' in reply) {
        return reply.ok;
      }

      // Event plugin: listeners live in the page; the Node side only records.
      if (command === 'plugin:event|listen') {
        const event = String(args.event);
        const handler = Number(args.handler);

        listeners.set(event, [...(listeners.get(event) ?? []), handler]);

        return handler;
      }

      if (command === 'plugin:event|unlisten') {
        removeListener(String(args.event), Number(args.eventId));
      }

      return null;
    },
  };

  page.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: removeListener,
  };

  page.__stacheFakeHostEmit = (event, payload) => {
    const ids = listeners.get(event) ?? [];

    for (const id of ids) {
      callbacks.get(id)?.({ event, id, payload });
    }

    return ids.length;
  };
}

/**
 * Scripted fake of the native host for end-to-end scenarios. A test scripts
 * answers with {@link answer}/{@link fail} (before or after navigation), emits
 * events with {@link emit} and reads every call from {@link calls}. A command
 * with no scripted answer rejects and is listed in {@link unanswered}.
 */
export class FakeHost {
  readonly calls: HostCall[] = [];
  readonly unanswered: string[] = [];
  private readonly answers = new Map<string, CommandAnswer>();
  private readonly failures = new Map<string, string>();

  private readonly page: Page;

  private constructor(page: Page) {
    this.page = page;
  }

  /** Installs the bridge and the init script on `page`. Call before the first `goto`. */
  static async install(page: Page) {
    const host = new FakeHost(page);

    await page.exposeFunction(INVOKE_BINDING, (command: string, args: unknown) =>
      host.reply(command, args),
    );
    await page.addInitScript(installTauriInternals);

    return host;
  }

  /** Scripts the answer to `command`. A function receives the call arguments. */
  answer(command: string, value: CommandAnswer) {
    this.failures.delete(command);
    this.answers.set(command, value);

    return this;
  }

  /** Scripts `command` to reject with `message`. */
  fail(command: string, message: string) {
    this.answers.delete(command);
    this.failures.set(command, message);

    return this;
  }

  /** Scripts `host_status` with a typed status. */
  answerHostStatus(status: HostStatus_Serialize) {
    return this.answer('host_status', status);
  }

  /** Scripts `bar_snapshot` with a typed outcome. */
  answerBarSnapshot(outcome: BarSnapshotOutcome_Serialize) {
    return this.answer('bar_snapshot', outcome);
  }

  /** Emits a typed `status-changed`. */
  emitStatusChanged(status: HostStatus_Serialize) {
    return this.emit('status-changed', { status });
  }

  /** Emits a typed `bar-changed`. */
  emitBarChanged(payload: BarChanged_Serialize) {
    return this.emit('bar-changed', payload);
  }

  /** Command names in call order, without the event plugin's own calls. */
  commands() {
    return this.calls
      .map((call) => call.command)
      .filter((command) => !command.startsWith('plugin:event|'));
  }

  /** Emits `event` to every page listener; resolves to the number of listeners reached. */
  emit(event: string, payload: unknown) {
    return this.page.evaluate(
      ([name, data]) => {
        const emit = (window as FakeHostWindow).__stacheFakeHostEmit;

        if (!emit) {
          throw new Error('fake host: the page side is not installed');
        }

        return emit(name, data);
      },
      [event, payload] as const,
    );
  }

  private async reply(command: string, args: unknown): Promise<Reply> {
    this.calls.push({ command, args });

    if (command.startsWith('plugin:event|')) {
      return { event: true };
    }

    const failure = this.failures.get(command);

    if (failure !== undefined) {
      return { error: failure };
    }

    if (!this.answers.has(command)) {
      this.unanswered.push(command);

      return { error: `fake host: no scripted answer for "${command}"` };
    }

    const answer = this.answers.get(command);

    return { ok: typeof answer === 'function' ? await answer(args) : answer };
  }
}
