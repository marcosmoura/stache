# MenuAnywhere — planned

Read when locating the MenuAnywhere rewrite plan. **Not shipped:** this notice describes a draft, not available behavior or implementation approval.

The planned module will rebuild the captured frontmost application's menu bar at a configured mouse click using public Accessibility and native menus. It will use the host's shared input service, not a private event tap. Each part needs separate approval; the runtime is blocked until the Keybindings shared service lands on `main`.

The [draft spec](../specs/2026-11-03-menu-anywhere/00-overview.md) defines three parts:

1. Typed `menuAnywhere.jsonc`, catalog/schema/bindings and migration, with no runtime.
2. Dedicated AX-owner reader, main-thread NSMenu presentation and exact-instance invocation adapters, with compile and per-run approved native gates.
3. Shared-service runtime, lifecycle, accepted-config coherence and truthful cleanup/outcome diagnostics.

Planned defaults remain disabled, modifiers `["control", "command"]` and `mouseButton: "rightClick"`. Modifier matching is exact. Empty modifiers remain valid and would capture unmodified matching clicks system-wide: use with care. `middleClick` will mean button number 2 only, correcting the legacy back/forward-button match. AX shortcut display will correct the legacy NoCommand inversion.

The plan also fixes busy-trigger stacking, display-coordinate placement, AX element lifetime, stale app targeting and unbounded menu reads. In-flight native calls cannot be instantly cancelled; cleanup failures remain visible and an uncertain AXPress outcome is never automatically retried. Enabled reconfigure may leave a captured popup live; pause, disable and stop invalidate it.

Current `main` already ships noTunes and its catalog migration/applications identity adapter. Keep Awake and Wallpapers remain separate-branch drafts, not dependencies available in this checkout. The old application is archived under `_legacy/`; its behavior is not the new runtime contract. No CLI, Settings UI, private menu API, search or menu cache is planned here.
