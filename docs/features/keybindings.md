# Keybindings — not shipped

Read when looking for Keybindings, Caps Lock or `§` shortcuts in Stache Next.

**Not shipped.** The [five-part draft](../specs/2026-11-03-keybindings/00-overview.md)
describes planned configuration, shared command/input boundaries, safe native
adapters and owned runtime/recovery. It is not implementation approval or a
description of available shortcuts.

The approved planned file is `keybindings.jsonc`, with `enabled: false` and
`bindings: {}` by default; values map shortcuts to a string or ordered string
array. Migration will wrap the legacy map and enable only a nonempty map. That
typed catalog entry and runtime do not exist on this worktree's baseline.
Current main already ships noTunes and generic catalog migration; Keybindings
implementation must rebase and reuse those facilities.

Current migration copies legacy `keybindings` verbatim as an unregistered file;
copying it does not activate shortcuts. See the
[archived behavior](../../_legacy/docs/features/keybindings.md) for the independent
old application. Do not apply legacy Caps remap commands/state files to Next.

No new Keybindings CLI, Settings or other module is included. Real global-input,
permission and remap verification requires explicit approval for each run;
default future tests use fakes. Implementation parts will replace this notice
with shipped facts as they land, distinguishing catalog/adapters from activation.
