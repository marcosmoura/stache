# Wallpapers (planned)

Read when planning the Wallpapers port. **Not shipped:** this page summarizes
the proposal, not an available runtime or CLI API.

The [draft spec](../specs/2026-11-03-wallpapers/00-overview.md) contains four
approval-gated implementation parts:

1. [Config and sources](../specs/2026-11-03-wallpapers/01-config-sources.md): typed
   `wallpapers.jsonc`, source resolution, migration and a generic accepted-state watch.
2. [Native processing/cache](../specs/2026-11-03-wallpapers/02-native-processing-cache.md):
   safe per-display adapter, image effects and cross-process cache coordination.
3. [Runtime](../specs/2026-11-03-wallpapers/03-runtime.md): owned lifecycle/timer,
   serialized application and same-config host `wallpapers.set`.
4. [CLI integration](../specs/2026-11-03-wallpapers/04-cli-integration.md): existing
   `wallpaper set`, `list`, `generate-all` contracts and integration verification.

## Planned behavior

- Defaults remain disabled, empty directory/list, interval/effects zero and
  random mode. A directory overrides the list; sources use config-relative paths.
- Explicit list/process/set works while disabled without starting rotation.
  Explicit CLI file paths use invocation CWD, not the config directory.
- Set uses a same-config host's accepted state or runs standalone only when
  the host is absent before sending a mutation. Different config directories
  and uncertain responses fail rather than applying twice or to the wrong host.
- Actual start enqueues initial selection asynchronously and returns Running
  before processing/native application. Initial errors log and keep Running;
  Running does not guarantee an image. Interval zero has one initial job and no
  retry timer. Source/effects reload stages without changing the visible image.
  Pause/disable/stop leaves it. Accepted publication wakes the owned timer.
- Factory-owned host-lifetime application handles disabled/paused manual jobs;
  shutdown cancels them before connection joins. A typed handled IPC outcome is
  not necessarily image success: partial/unknown results never silently retry.
- A starting host may have no socket: an absent-only local set is allowed, but
  the host's later initial job may override it. No new startup coordination.
- Processing targets each display's pixels. Main means primary, numeric screens
  are 1-based with legacy 0 retained as primary alias. Public native application
  needs no Automation prompt or required Accessibility permission; Space coverage
  must be verified rather than assumed.
- Cache publication is immutable and coordinated with `cache clear`. Clearing
  waits at most five seconds for its exclusive lease, failing without mutation
  if busy. Participants never unlink the runtime lock; detected replacement
  fails closed/reacquires safely. Uncooperating OS/temp deletion is outside an
  advisory lock's universal guarantees. Clearing does not guarantee indefinite
  OS retention of an assigned image URL.
- No new UI, commands beyond the legacy wallpaper tree, selection modes or
  scheduling features beyond its rotation interval (implemented with an owned
  wakeable timer). No dependency on another unimplemented tool.
  Default automated tests never change the desktop; real application/restoration
  requires explicit approval for each run.

Implementation parts will replace this planned summary with verified shipped
behavior incrementally. The [review audit](../specs/2026-11-03-wallpapers/review-findings.md)
tracks spec remediation only. The independent [legacy page](../../_legacy/docs/features/wallpapers.md)
is historical and contains outdated claims; its source, not its prose, defines
the compatibility baseline.
