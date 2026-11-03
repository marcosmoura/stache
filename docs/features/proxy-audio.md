# Audio device switching (proxyAudio)

Read when changing automatic audio-device selection, the `CoreAudio` watcher,
`stache audio list` or `proxyAudio.list`.

Rust paths are relative to `src/crates/`.

## Purpose

`proxyAudio` keeps the default input and output device on the best available
one: it watches `CoreAudio` for device and default-device changes and re-applies
an ordered configuration list. AirPlay and HDMI are special-cased above the
list. The module is inert unless `proxyAudio.jsonc` sets `enabled`.

## Entry points

| Caller                    | Entry                                                                   |
| ------------------------- | ----------------------------------------------------------------------- |
| Desktop host registration | `app/src/host.rs` `ProxyAudioFactory` / `ModuleFactories::production()` |
| Module runtime            | `proxy-audio/src/watcher.rs` `ProxyAudioModule`                         |
| CLI                       | `proxy-audio/src/cli.rs` contribution; `stache audio list`              |
| Socket                    | `proxy-audio/src/ipc.rs` `AudioIpc`; `proxyAudio.list`                  |

The app crate is the only place that knows both the module and `stache-macos`.
The module depends on `stache-sdk` and `stache-macos` only; the SDK has no audio
surface.

## Key types

- `config.rs`: `ProxyAudioConfig { enabled, input, output }`,
  `AudioDevicePriority { name, strategy, dependsOn }`, `AudioDeviceDependency`,
  `MatchStrategy::{Exact, Contains, StartsWith, Regex}`. This is the single
  authoritative type used by runtime, schema and migration.
- `macos/src/audio.rs`: the safe `AudioBackend` trait, `CoreAudioBackend`,
  `AudioDevice`, `AudioDirection`, `AudioWakeup` and `AudioChangeSubscription`.
- `device.rs`: policy `AudioDevice`, `AudioDeviceType::detect`, name matching,
  dependency checks and `devices_for`.
- `priority.rs`: `get_target_{output,input}_device` and the testable
  `resolve_*` variants.
- `list.rs`: `AudioDeviceInfo`, `DeviceFilter`, `list_devices`,
  `format_devices_table`.
- `watcher.rs`: `ProxyAudioModule`, one listener generation and one worker.

## Selection order

Output (`resolve_output_device`):

1. An AirPlay device when software screen mirroring is active
   (`macos/src/display.rs::is_screen_mirroring_active`, the legacy
   `is_in_mirror_set && !is_in_hw_mirror_set` check).
2. Keep the current device when it is AirPlay.
3. An HDMI device when the current device is built in. The `CoreAudio`
   transport is authoritative; a name containing `hdmi` is the fallback.
4. The first eligible configured `output` entry.
5. The first device whose name contains `MacBook Pro`.

Input follows the same shape without the HDMI rule, falls back to a `MacBook Pro`
microphone and finally keeps the current input device. Every list is filtered by
the device's input/output stream support.

Matching is case-insensitive for `Exact`/`Contains`/`StartsWith`. `Regex`
compiles the raw pattern and matches case-sensitively against the name. A
`dependsOn` device must appear in the same candidate list (input or output);
the dependency is never selected. `AudioDeviceType::detect` is name based and
never returns `Hdmi`.

## Data flow and lifecycle

`ProxyAudioModule::start` registers one listener generation, applies the rules
once, then spawns the named `audio` worker through `ModuleContext`. The
subscription owns the three `CoreAudio` property addresses
(`DefaultOutputDevice`, `DefaultInputDevice`, `Devices`) and the boxed
`AudioWakeup` used as callback data. Registration is rolled back if any address
fails. The callback only signals a bounded, coalescing channel; it never
enumerates, locks or allocates. The worker re-enumerates, resolves one target
per direction and calls `AudioBackend::set_default_device` only when the target
identifier differs from the current one.

`reconfigure` swaps the shared config snapshot and re-evaluates once under the
runtime apply lock. It never resubscribes or spawns, so a running module keeps
exactly one listener generation. `stop` drops the runtime; the subscription
removes the exact addresses/callback/client data, and the registry cancels and
joins the worker. Stop is safe after a partial start and is idempotent.

Config validation happens in the SDK before `configure`/`reconfigure`. Invalid
JSON or an invalid regex is rejected with a diagnostic; a running module keeps
its previous configuration and revision. A paused or disabled reload only
stages the next-start snapshot. Pause, disable, failure and permission rules
are the shared registry rules; the module declares no permission.

## Config

Root key `proxyAudio` in `proxyAudio.jsonc`; both lists are ordered. Defaults
and schema descriptions come from the typed `Default`; see
[architecture/config.md](../architecture/config.md).

## CLI

`stache audio list [-j|--json] [-i|--input] [-o|--output]`

The command runs entirely in the CLI process through `CoreAudioBackend`. It
never contacts the socket, starts the desktop host or registers a listener.
Neither flag, or both flags, selects every device. Results are sorted by name.
JSON keeps the legacy `name` / `type` / `input` / `output` keys and is styled
only when stdout is a terminal; the styling function is injected by the app so
the module does not depend on `stache-cli`. The table keeps the legacy columns,
padding and the `No audio devices found.` message.

## Socket

`proxyAudio.list` is a read-only method served from the same listing service:

```json
{"module":"proxyAudio","method":"list","params":{"kind":"output"}}
{"status":"ok","data":[{"name":"AirPods Pro","type":"bluetooth","input":false,"output":true}]}
```

Omitting `params`, `null`, or `{}` means `kind: "all"`; `input` and `output`
filter one direction. Unknown methods and invalid kinds are typed wire errors.
The handler is registered by the same factory that registers the module, so
the read-only path is available even when the module is disabled or paused.
There is no device-mutation method.

## Invariants and pitfalls

- Device identifiers are ephemeral; rules match names, never identifiers.
- Priority is applied once at start and on change events; a manual selection
  that matches no rule is left alone until the next event.
- The worker only sets a default when the identifier differs, so a re-evaluation
  caused by its own change does not loop.
- `AirPlay` stickiness applies after the mirroring rule; HDMI promotion applies
  only when the current device is built in.
- The module owns no globals, no permissions, no executables and no window
  surfaces.
- All `CoreAudio`/`CoreGraphics` unsafe code lives in `stache-macos`; each block
  states its pointer/ownership invariant.

## Tests

`cargo nextest run -p stache-proxy-audio` covers the ported legacy type,
matching, priority, dependency, list and CLI goldens, the fake-backend lifecycle
failpoints (listener rollback, worker-spawn failure, set-default failure,
callback during stop, repeated reload) and the socket method.
`cargo nextest run -p stache-macos` covers listener rollback for every
registration index. Real-device checks remain manual: no test changes the
system default audio device.
