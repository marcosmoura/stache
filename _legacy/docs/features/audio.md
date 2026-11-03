# Audio device switching (proxyAudio)

Read when changing automatic audio device selection, device matching/priority rules, the CoreAudio watcher, or `stache audio`.

Rust paths are relative to `app/native/src/`.

## Purpose

`proxyAudio` keeps the default input and output device on the best available one: it watches CoreAudio for device changes and re-applies a configurable priority list. AirPlay is special-cased above the list.

## Entry points

| Caller                                     | Entry                                                                              |
| ------------------------------------------ | ---------------------------------------------------------------------------------- |
| App startup (background, `spawn_blocking`) | `audio::init()` in `lib.rs` → `watcher::start(&config.proxy_audio)` when enabled   |
| CLI                                        | `cli/commands/audio.rs` `execute` → `audio::list_devices` / `format_devices_table` |
| Tray                                       | `audio::ProxyAudioLifecycle` (registered in `lib.rs`)                              |

`audio` is re-exported at the crate root (`pub use modules::{audio, tiling}`), which is what the CLI uses.

## Key types

- `config/types/audio.rs`: `ProxyAudioConfig { enabled, input, output }`, `AudioDevicePriority { name, strategy, depends_on }`, `AudioDeviceDependency`, `MatchStrategy::{Exact, Contains, StartsWith, Regex}`.
- `modules/audio/device.rs`: `AudioDevice { id: AudioDeviceID, name }`, `AudioDeviceType`, `find_device_by_priority`, `is_dependency_satisfied`, `get_{input,output,default_*}_devices`.
- `modules/audio/priority.rs`: `get_target_output_device`, `get_target_input_device` (thin wrappers over `resolve_*`, which take the screen-mirroring flag for testability).
- `modules/audio/watcher.rs`: `start`, `ProxyAudioLifecycle`, the `RUNTIME: Mutex<Option<RetainedListeners>>` slot.
- `modules/audio/list.rs`: `AudioDeviceInfo`, `DeviceFilter`, `list_devices`, `format_devices_table`.

## Selection order

Output (`resolve_output_device`):

1. An AirPlay device, when screen mirroring is active (`platform/display.rs::is_screen_mirroring_active`).
2. Keep the current device if it is AirPlay.
3. An HDMI device, when the current device is built-in.
4. The first config `output` entry that matches an available device.
5. A device whose name contains `MacBook Pro`.

Input follows the same shape without the HDMI rule, falls back to a `MacBook Pro` microphone, then keeps the current device.

Matching is case-insensitive for `Exact`/`Contains`/`StartsWith`. `Regex` compiles the raw pattern per comparison and matches case-sensitively against the device name. A `dependsOn` device must appear in the same candidate list (input or output); the dependency itself is never selected. Device type is inferred from the name (`AudioDeviceType::detect`); `is_hdmi` additionally asks CoreAudio for the transport type and falls back to a name check, so `detect` never returns `Hdmi`.

## Data flow

`start` applies the priority rules once immediately, then registers three property listeners on `kAudioObjectSystemObject` (`DefaultOutputDevice`, `DefaultInputDevice`, `Devices`). The C callback only sends `()` on an mpsc channel; a named worker thread (`stache-audio-device-watcher`) re-reads the device list and calls `AudioDeviceSetPropertyData` when the target differs. Listener registration is rolled back if any of the three fails.

## Config

Root key `proxyAudio`. Both lists are ordered; see [config](../architecture/config.md) and `docs/samples/config.jsonc`.

## CLI

`stache audio list [--json] [--input|--output]` runs entirely in the CLI process (no socket round-trip, see [ipc](../architecture/ipc.md)). Passing both `--input` and `--output` yields `DeviceFilter::All`. JSON field for the type is `"type"`; results are sorted by name.

## Invariants and pitfalls

- The module is inert unless `proxyAudio.enabled` is true; `start` is also idempotent while a generation is live.
- Priority is only applied on events and once at start. Manually selecting a device that matches no rule is left alone until the next device event.
- `pause` unregisters the exact three addresses with the same callback and `client_data` pointer, drops the boxed `Sender` so the worker's `recv` fails, then joins the worker. Keeping a `RetainedListeners` alive is what makes pause/resume safe.
- Device IDs are not stable across reconnects; rules match names, never IDs.
- Nothing here needs Accessibility or main-thread dispatch; all CoreAudio calls are thread-agnostic.

## Lifecycle and hot reload

`ProxyAudioLifecycle` id `proxyAudio`: `ConfiguredOff` when disabled, `Running` when listeners are live, `Paused` otherwise. `resume` re-applies the priority list and registers a fresh generation. Config changes restart the app, which re-runs `audio::init`; the config snapshot is cloned into the worker, so in-process reload is not supported.

## Tests

`modules/audio/device.rs` (type detection, every `MatchStrategy`, dependency satisfied/not), `priority.rs` (mirroring, AirPlay stickiness, HDMI promotion, dependency skipping, fallbacks), `watcher.rs` (`proxy_audio_status_maps_states`), `config/types/audio.rs` (defaults, serialization), `cli/commands/audio.rs` (clap parsing).
