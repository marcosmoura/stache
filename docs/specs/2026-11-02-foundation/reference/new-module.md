# Adding a module

Read when reviewing the extension recipe; planned docs/guides/new-module.md, refined by parts 2–6.

## Minimum extension

1. Add src/crates/modules/<crate-id> to root workspace with SDK/macOS dependencies plus external crates.
   Pick stable config/runtime ID (pilot: crate proxy-audio, ID proxyAudio). Never import another module.
2. Define serde/schemars/default/validated config and descriptor; defaults/templates/schemas share types.
   Declare enabled state, permissions, thread contract and required executable/framework dependencies.
3. Implement configure/start/reconfigure/stop with owned resources. Prepare before applying reload;
   cancel/quiesce/join on stop, including partial-start failure. No statics or detached workers.
4. Register factory/config/command descriptors in src/crates/app composition root, not SDK dispatch switches.
   Standalone CLI contributions must operate without starting host/lifecycle. Decode typed IPC params.
5. Add UI only when needed under `src/ui/src/shell/modules/moduleName/`, with `moduleName.tsx`,
   `moduleName.test.tsx`, `moduleName.styles.tsx`, `index.ts`. Shared components use sibling
   `src/ui/src/shell/components/ComponentName/` with the same named-file convention; utilities use
   `src/ui/src/utils/{utilName.ts,index.ts}` only as needed. Declare/request surfaces through SDK;
   pilot has no UI. Use generated `src/ui/src/sdk/bindings.gen.ts`, not mirrored event strings.
6. Regenerate schema/bindings, add lifecycle/policy/output tests and a short feature doc; route via index.

## Pilot example

ProxyAudio declares no Accessibility requirement/UI/external binary; worker performs policy and safe
macOS adapter owns CoreAudio listeners. Audio list uses read-only enumeration with no worker start.
Only module factory registration and generated metadata integrate it; SDK/CLI never switch on audio ID.

## Review checklist

- Permission wait, temporary pause and bad config are distinct; reload cannot silently resume a pause.
- Registration is pure; no platform resources are acquired before configure/start/explicit command.
- Native callbacks enqueue bounded events and cannot access freed state after stop.
- Cross-module needs are narrow explicit services/events, not a module-to-module import.
- Test policy with fake native services; opt into guarded hardware tests, never mutate devices in CI.
- Run Bun lint/tests, schema/binding drift checks and documentation updates required by the part.
- Colocate all actual suites; `src/ui/tests/` is setup/fixtures/mocks only. Verify `.styles.tsx`
  imports are extracted by wyw and checked by stylelint; never introduce a React tray/bar as scaffolding.
