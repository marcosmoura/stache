import {
  mutationOptions,
  useMutation,
  useQueryClient,
  type MutationKey,
  type UseMutationOptions,
} from '@tanstack/react-query';

import {
  commands,
  type BarAction,
  type BarActionOutcome,
  type BarTilingOutcome,
  type BarTilingRequest,
  type SessionAction,
  type SessionActionOutcome,
} from '@/ipc';

/*
 * Bar click commands as mutations. Each resolves with the typed outcome (only
 * a rejected `invoke` is an error). The hooks log any outcome other than
 * success, and any rejection, with `console.debug` and draw nothing; a click
 * while the same action is still pending is ignored, never queued.
 */

export const barMutationKeys = {
  action: ['bar', 'action'] as const,
  session: ['bar', 'session'] as const,
  tiling: ['bar', 'tiling'] as const,
};

/** `bar_action(action)`: opens the fixed target of a closed action enum. */
export function barActionOptions() {
  return mutationOptions({
    mutationKey: barMutationKeys.action,
    mutationFn: (action: BarAction): Promise<BarActionOutcome> => commands.barAction(action),
  });
}

/** `bar_session_action("keepAwake", action)`: pauses or resumes Keep Awake for this session. */
export function barSessionActionOptions() {
  return mutationOptions({
    mutationKey: barMutationKeys.session,
    mutationFn: (action: SessionAction): Promise<SessionActionOutcome> =>
      commands.barSessionAction('keepAwake', action),
  });
}

/**
 * `bar_tiling_action(request)`: focuses a workspace or window, echoing the
 * rendered strip's `generation` and `sequence` so Tiling can refuse a stale target.
 */
export function barTilingActionOptions() {
  return mutationOptions({
    mutationKey: barMutationKeys.tiling,
    mutationFn: (request: BarTilingRequest): Promise<BarTilingOutcome> =>
      commands.barTilingAction(request),
  });
}

type Guard<V, R extends { kind: string }> = {
  key: MutationKey;
  /** Two calls with the same identity are the same action. */
  identify: (variables: V) => string;
  /** The outcome kind that is not logged. */
  success: R['kind'];
  /** The prefix of a diagnostic line. */
  describe: (variables: V) => string;
};

/** Runs a Bar command mutation: ignores a click while the same action is pending, logs non-success. */
function useGuardedMutation<V, R extends { kind: string }>(
  options: UseMutationOptions<R, Error, V>,
  guard: Guard<V, R>,
): (variables: V) => void {
  const client = useQueryClient();
  const { mutate } = useMutation({
    ...options,
    onSuccess: (outcome, variables) => {
      if (outcome.kind !== guard.success) {
        console.debug(`${guard.describe(variables)}: ${outcome.kind}`, outcome);
      }
    },
    onError: (error, variables) => {
      console.debug(`${guard.describe(variables)} failed`, error);
    },
  });

  return (variables) => {
    const identity = guard.identify(variables);
    const pending = client.getMutationCache().findAll({
      mutationKey: guard.key,
      exact: true,
      status: 'pending',
      predicate: (mutation) => guard.identify(mutation.state.variables as V) === identity,
    });

    if (pending.length === 0) {
      mutate(variables);
    }
  };
}

/** The click handler runner for `bar_action`; a pending identical action ignores the click. */
export function useBarAction(): (action: BarAction) => void {
  return useGuardedMutation(barActionOptions(), {
    key: barMutationKeys.action,
    identify: (action) => action,
    success: 'completed',
    describe: (action) => `bar action ${action}`,
  });
}

/** Keep Awake pause/resume; any Keep Awake action pending ignores the click. */
export function useBarSessionAction(): (action: SessionAction) => void {
  return useGuardedMutation(barSessionActionOptions(), {
    key: barMutationKeys.session,
    identify: () => 'keepAwake',
    success: 'applied',
    describe: (action) => `keep awake ${action}`,
  });
}

/** Focus a Tiling workspace or window; a pending focus of the same target ignores the click. */
export function useBarTilingAction(): (request: BarTilingRequest) => void {
  return useGuardedMutation(barTilingActionOptions(), {
    key: barMutationKeys.tiling,
    identify: ({ action }) =>
      action.kind === 'focusWorkspace' ? `workspace:${action.name}` : `window:${action.window}`,
    success: 'applied',
    describe: ({ action }) => `tiling ${action.kind}`,
  });
}
