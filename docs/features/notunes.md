# noTunes

Read when changing the Apple Music/iTunes blocker, its replacement target or its
lifecycle.

noTunes is the shipped blocker in `src/crates/modules/notunes`: it watches
application launches, terminates Apple Music and iTunes and can open one
configured replacement player. `NoTunesFactory` in `src/crates/app/src/host.rs`
only composes it into the desktop host; all policy lives in the module and every
native call goes through `stache_macos::ApplicationControl`.

## Configuration

`notunes.jsonc` is the authoritative document (`NoTunesConfig`):

| Key         | Values                                           | Default |
| ----------- | ------------------------------------------------ | ------- |
| `enabled`   | `true` / `false`                                 | `false` |
| `targetApp` | optional; an absolute `.app` path or a bundle ID | absent  |

Nothing about the replacement is hardcoded in the policy, defaults, template or
schema: there is no preset list, no player discovery and no cached fallback. A
target is either an absolute path such as `/Applications/Spotify.app` or a bundle
identifier such as `com.spotify.client`. Omitting `targetApp` blocks without
launching anything. The legacy `spotify`, `tidal`, `feishin` and `none` spellings
are rejected by the module; only `config migrate` still knows them, to translate
them into a bundle identifier or an absent key.

A path target derives its bundle identifier from the bundle to decide whether the
player is already running; a bundle-identifier target resolves its path before
opening it. A target that cannot be resolved — no application bundle at that
path, or the identifier is not installed — warns while blocking stays `Running`,
so an uninstalled player never takes the blocker down. A value that is neither an
absolute `.app` path nor a well-formed bundle identifier is a catalog rejection:
the last accepted configuration and revision stay live. Unknown keys are
non-fatal warnings.

## Blocking

- Only the exact bundle identifiers `com.apple.Music` and `com.apple.iTunes`
  count (`MUSIC_BUNDLE_ID`, `ITUNES_BUNDLE_ID`). Case variants, other players and
  unrelated applications are ignored, and a notification without a bundle
  identifier is dropped.
- This is interception after launch, not an OS veto: a player can appear briefly
  before it is terminated, and a media-key launch cannot be distinguished from a
  manual one.
- The module requests force-termination and reports the outcome; it never
  confirms that the process exited. `Requested` means macOS accepted the
  request, `Refused` and transport errors are logged, and a refused request still
  proceeds to the replacement decision.

## Lifecycle

`start` covers enable, resume, retry and host bootstrap. It subscribes on the
workspace notification center before taking the running-application snapshot,
then sweeps both blocked identifiers. The initial sweep only terminates
instances that were already running; it never opens a replacement.

A later will-launch notification terminates that instance and then evaluates the
current accepted target:

- an absent target only blocks.
- A replacement that is already running is not opened again.
- An unresolvable target warns and blocking stays `Running`, not `Unavailable`.
- Otherwise the resolved application is opened once; a repeated notification for
  the same instance, a pending open or a still-running player cannot race a
  duplicate launch.

A target change applies live: `reconfigure` publishes the new accepted snapshot
and wakes the policy worker without resubscribing, resweeping or restarting
observation. Queued work re-reads the accepted target at the native commit point,
so it never launches a stale replacement. An unchanged file is a no-op and a
rejected reload keeps the previous revision, target and runtime. A reload while
paused stages the next-start configuration only.

Migrating from the legacy app is not a byte copy: `spotify`, `tidal` and `feishin`
become their bundle identifiers, an explicit `none` becomes an absent key, and an
enabled section with no target — which used to mean Spotify — becomes block-only
with a warning.

`stop` covers pause, disable and registry shutdown: it invalidates the runtime
generation, wakes the policy worker so the registry can join it, and removes the
observation. A removal failure is reported and retried, never reported as a
successful pause.
A termination or open request macOS already accepted may still complete; it must
not trigger follow-on work.

## Host surface

There is no dedicated UI, Settings screen, CLI command or socket method. Generic
tray rows, `host_status` snapshots, config diagnostics and the shared
Enable/Disable, Pause/Resume and Retry actions are the whole surface. The
descriptor declares no permissions, services, executables or window surfaces, so
a permission grant or refresh never prompts for it and enabling it creates no
window or webview.

## Tests and known gaps

Unit tests in the module and host integration tests in `src/crates/app/tests`
use `stache_macos::FakeApplicationBackend` and `ManualExecutor`. They never
observe, terminate or launch a real application, and no automated check prompts
for a permission. Real observation and force-termination remain an explicit,
opt-in check.

Host termination stops the module through the coordinated teardown
([overview](../architecture/overview.md)). Logout, restart and shutdown use that route
but have not been run for real.
