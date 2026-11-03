# proxyAudio

Read when reviewing the pilot's target behavior; planned docs/features/proxy-audio.md for part 6.

## Entry points and types

src/crates/modules/proxy-audio owns config, matching/priority policy, worker and CLI/IPC descriptors.
src/crates/macos/src/audio.rs owns safe CoreAudio enumeration/defaults/listeners; display.rs detects
software mirroring. ProxyAudioConfig is enabled/input/output; priorities carry name/strategy/dependsOn.
Typed client/DTO exports target `src/ui/src/sdk/bindings.gen.ts`; pilot adds no UI surface directory.

## Data flow

Start selects once and registers device/default input/default output listeners. Callback enqueues
coalesced wakeup; named worker reenumerates and changes defaults only when target differs.
Config acceptance swaps prepared snapshot and reevaluates once, without listener duplication.
Stop/pause removes listeners, quiesces callback access and joins worker. No Accessibility or main thread
needed for audio calls; host tray/window operations still require main thread.

## Selection and CLI

Output prefers mirrored AirPlay, current AirPlay, HDMI when current is built-in, configured ordered
match, then MacBook Pro fallback. Input omits HDMI, microphone fallback then current. Match exact,
contains/startsWith case-insensitively; regex is case-sensitive. Dependencies use same-direction
candidate list; they are conditions, not selected alternatives. Device IDs are not stable identifiers.

audio list [-j/--json] [-i/--input] [-o/--output] enumerates without desktop/listener startup. None/both
direction flags mean all. Preserve sorted names, type JSON key and existing rounded human table.
Piped JSON is ANSI-free. proxyAudio.list IPC shares the read-only service.

## Config and tests

proxyAudio.jsonc defaults disabled, empty lists. Valid live reload applies without restart; invalid
pattern/type/parse retains last-good operation and shows diagnostic. Pause is temporary and survives
accepted enabled-config reload; disable stops, reenable starts subject to pause/reset rules.

Policy/list tests port legacy cases; fake backend tests listener rollback and in-flight callback stop.
Hardware tests require explicit single-app guard: never run new and legacy automation together.
Record unavailable AirPlay/HDMI hardware as gaps. Performance protocol counts host and child processes.
