# Bar (planned)

Read when locating the proposed Bar rewrite. **NOT SHIPPED:** all eight [spec parts](../specs/2026-11-03-bar/00-overview.md) are draft and require separate implementation approval.

The plan is a primary-display nonactivating panel with **no visual changes from legacy Bar**: identical layout, fonts, colors, icons, hover/animation behavior, fixed-centered media and responsive/empty/error states. Maple Mono NF remains system-installed with the exact legacy fallback stack. Tiling's workspace/application strip and icon maps wait for its final spec; its empty layout slot stays reserved.

Planned defaults are `enabled: false`, height28/padding12 with legacy weather keys. Parts cover config/migration, generic SDK state/reservations, native panel/data bridge, static UI parity, system data/fixed clicks, weather, media and optional Keep Awake observation. Disabled Bar must own no worker or webview. Keep Awake absence must not stop Bar; its click is session Pause/Resume only and inert while disabled. Widgets and Settings CLI are not planned.

Independent source-pinned pixel/computed-style parity and explicitly approved native feasibility/performance gates block acceptance. Tests must never launch the legacy app/build/automation or perform real panels, network, prompts, media processes or system mutations. See [part 04](../specs/2026-11-03-bar/04-ui-shell-parity.md) for the proposed baseline process; no baseline has been implemented by this draft.

Main already ships noTunes and its application/migration adapters; Keep Awake and Wallpapers are separate unmerged drafts, not dependencies that can be imported today. Implementation rebases onto current main and reuses landed generic facilities. This notice will be replaced incrementally with shipped facts as parts land; legacy documentation is unchanged.
