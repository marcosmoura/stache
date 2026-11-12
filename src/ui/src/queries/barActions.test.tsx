import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  BarAction,
  BarActionOutcome_Serialize,
  BarTilingOutcome,
  BarTilingRequest,
  SessionAction,
  SessionActionOutcome,
} from '@/ipc';

import { flush, mockHost } from '../../tests/ipc';
import { render } from '../../tests/render';

import { useBarAction, useBarSessionAction, useBarTilingAction } from './barActions';

type Runner = (value: never) => void;

/** Renders a component exposing a command runner; returns it and the rendered DOM. */
async function mount(useRunner: () => Runner) {
  const runner: { current: Runner | null } = { current: null };

  function Harness() {
    runner.current = useRunner();

    return <p data-testid="probe">static</p>;
  }

  const screen = await render(<Harness />);
  const html = () => screen.container.innerHTML;

  return { run: (value: unknown) => runner.current!(value as never), html };
}

/** A deferred answer, so a call can be held pending. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });

  return { promise, resolve };
}

const barOutcomes: BarActionOutcome_Serialize[] = [
  { kind: 'completed' },
  { kind: 'stale' },
  { kind: 'notIssued' },
  { kind: 'notAttempted' },
  { kind: 'refused', detail: 'no' },
  { kind: 'notInstalled' },
  { kind: 'timedOutBeforeIssuance' },
  { kind: 'uncertainAfterIssuance' },
  { kind: 'busy' },
  { kind: 'unauthorized' },
  { kind: 'unavailable', reason: 'stopped', detail: null },
];

const sessionOutcomes: SessionActionOutcome[] = [
  { kind: 'applied' },
  { kind: 'notApplied', reason: 'refused' },
  { kind: 'stale' },
  { kind: 'disabled' },
  { kind: 'unavailable', reason: 'notRunning' } as SessionActionOutcome,
  { kind: 'busy' },
  { kind: 'uncertain' },
  { kind: 'unauthorized' },
];

const tilingOutcomes: BarTilingOutcome[] = [
  { kind: 'applied' },
  { kind: 'noOp' },
  { kind: 'stale' },
  { kind: 'unavailable', reason: 'paused' },
  { kind: 'busy' },
  { kind: 'uncertain' },
  { kind: 'refused' },
  { kind: 'failed' },
  { kind: 'invalid' },
  { kind: 'unauthorized' },
];

const workspace = (name: string): BarTilingRequest => ({
  generation: '3',
  sequence: '41',
  action: { kind: 'focusWorkspace', name },
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('bar_action', () => {
  for (const outcome of barOutcomes) {
    it(`sends the typed request and handles ${outcome.kind}`, async () => {
      const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});
      const host = mockHost().answer('bar_action', outcome);
      const { run, html } = await mount(useBarAction);
      const before = html();

      run('cpu' satisfies BarAction);
      await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
      await flush(10);

      expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'cpu' });
      expect(debug).toHaveBeenCalledTimes(outcome.kind === 'completed' ? 0 : 1);
      expect(html()).toBe(before);
    });
  }

  it('ignores a second click of the same action while pending, not another action', async () => {
    const held = deferred<BarActionOutcome_Serialize>();
    const host = mockHost().answer('bar_action', () => held.promise);
    const { run } = await mount(useBarAction);

    run('clock');
    run('clock');
    run('wifi');
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(2));
    expect(host.callsOf('bar_action').map((call) => call.args)).toEqual([
      { action: 'clock' },
      { action: 'wifi' },
    ]);

    held.resolve({ kind: 'completed' });
    await flush(10);
    run('clock');
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(3));
  });

  it('logs a rejected invoke with console.debug', async () => {
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});

    mockHost();
    const { run } = await mount(useBarAction);

    run('battery');
    await vi.waitFor(() =>
      expect(debug).toHaveBeenCalledWith('bar action battery failed', expect.anything()),
    );
  });
});

describe('bar_session_action', () => {
  for (const outcome of sessionOutcomes) {
    it(`sends the typed request and handles ${outcome.kind}`, async () => {
      const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});
      const host = mockHost().answer('bar_session_action', outcome);
      const { run, html } = await mount(useBarSessionAction);
      const before = html();

      run('pause' satisfies SessionAction);
      await vi.waitFor(() => expect(host.callsOf('bar_session_action')).toHaveLength(1));
      await flush(10);

      expect(host.callsOf('bar_session_action')[0]!.args).toEqual({
        module: 'keepAwake',
        action: 'pause',
      });
      expect(debug).toHaveBeenCalledTimes(outcome.kind === 'applied' ? 0 : 1);
      expect(html()).toBe(before);
    });
  }

  it('ignores any Keep Awake click while one is pending', async () => {
    const held = deferred<SessionActionOutcome>();
    const host = mockHost().answer('bar_session_action', () => held.promise);
    const { run } = await mount(useBarSessionAction);

    run('pause');
    run('resume');
    await flush(10);
    expect(host.callsOf('bar_session_action')).toHaveLength(1);

    held.resolve({ kind: 'applied' });
    await flush(10);
    run('resume');
    await vi.waitFor(() => expect(host.callsOf('bar_session_action')).toHaveLength(2));
  });
});

describe('bar_tiling_action', () => {
  for (const outcome of tilingOutcomes) {
    it(`sends the typed request and handles ${outcome.kind}`, async () => {
      const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});
      const host = mockHost().answer('bar_tiling_action', outcome);
      const { run, html } = await mount(useBarTilingAction);
      const before = html();
      const request: BarTilingRequest = {
        generation: '7',
        sequence: '18446744073709551615',
        action: { kind: 'focusWindow', window: '42' },
      };

      run(request);
      await vi.waitFor(() => expect(host.callsOf('bar_tiling_action')).toHaveLength(1));
      await flush(10);

      expect(host.callsOf('bar_tiling_action')[0]!.args).toEqual({ request });
      expect(debug).toHaveBeenCalledTimes(outcome.kind === 'applied' ? 0 : 1);
      expect(html()).toBe(before);
    });
  }

  it('ignores a pending focus of the same target, not of another one', async () => {
    const held = deferred<BarTilingOutcome>();
    const host = mockHost().answer('bar_tiling_action', () => held.promise);
    const { run } = await mount(useBarTilingAction);

    run(workspace('code'));
    run(workspace('code'));
    run(workspace('web'));
    await vi.waitFor(() => expect(host.callsOf('bar_tiling_action')).toHaveLength(2));
    held.resolve({ kind: 'applied' });
  });
});
