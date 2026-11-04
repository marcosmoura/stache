---
status: draft
---

# 03 — Ordinary and section-key adapters

Read when implementing parsing and owned Carbon registration. Depends on parts
01–02; no production Keybindings runtime is registered here.

## Scope and files

Add `modules/keybindings/src/{shortcut.rs,plan.rs}` and tests; safe planned
`macos/src/hotkeys.rs`/`keyboard_layout.rs` with fake backend and owner tests.
Extend `macos/src/lib.rs` and workspace dependency declarations only as needed.
All native Carbon/TIS registration, observation and removal lives in macOS.

## Requirements

Source-check legacy `normalize_shortcut`, `collect_planned_shortcuts`, Caps parser
and `section::parse_section_shortcut`. Preserve `Ctrl`→`Control`,
`Cmd`/`Super`/`Meta`→`Command`, `Alt`/`Opt`→`Option`, backtick→`Backquote` per
`+`-separated component; do not add broad trimming/case normalization that changes
ordinary grammar. Plan raw keys in lexical order, route Caps first, section next,
ordinary last. Last semantic duplicate in that order wins with both raw spellings
in diagnostics, independent of map insertion order. Invalid entries are skipped.

Use Carbon directly or a safe `global-hotkey`-based adapter **only after** a compile
gate proves the archived ordinary `Shortcut::parse` grammar/key aliases and safe
main ownership. Do not pick an incompatible parser just to avoid Tauri. Keep
parser-only dependencies workspace-declared; no Tauri in module/SDK. A corpus
derived from the archived parser's supported keys/modifiers (including bare keys,
function keys, arrows and Backquote) must establish equivalence/rejection, not
only the sample's seven shortcuts. Unsupported native mappings fail per binding.

Carbon objects/handlers and TIS data are main-owned `!Send` values in a main-local
store. Module code receives only opaque Send IDs/value outcomes scoped by service,
generation/revision. No `unsafe impl Send/Sync` workaround. Register each chord
individually; preserve successes and expose failures, never claim all bindings
active. Handle installation failure is distinct from one chord's conflict.
Publish every successful ordinary/section physical chord reservation to part 02's
central registry, including priority/filter overlap metadata; stage inert entries,
commit with routes, and remove only the matching owned reservation. Layout refresh
updates reservations at the same routing boundary. Failed registration is not an
active reservation.

This part owns the generic **yielding reservation** operation built on part 02's
registry: a higher-priority HID claimant (Hold-to-Quit for physical Command-Q) can
yield an overlapping Carbon reservation, and while yielded that handler must not
dispatch. Yield must persist across reclaim attempts, be reversible on claimant
release/pause/stop/disable/reload, keep unrelated bindings registered, and make
exactly one action run for one physical press. Hold-to-Quit part 04 consumes this;
it does not reimplement it. A mandatory measured manual gate (per-run approval)
records whether a suppressing head-insert HID tap already prevents Carbon
`RegisterEventHotKey` dispatch for the same chord. That result is evidence about
whether yielding is required at runtime; it never removes the primitive, and until
measured the spec claims no cross-backend priority.
Callbacks catch unwind where supported, enqueue bounded work only and ignore
releases for command dispatch. Forward foreign/malformed Carbon events through
`CallNextEventHandler`; late IDs cannot reach successor bindings. Native IDs must
not be reused in a way that reinterprets queued stale events.

`§` must be last, with only Command/Shift/Option/Control after normalization;
`CapsLock+§` stays the legacy physical ISO key 0x0A, not layout-aware section.
Resolve ordinary `§` from TIS/UCKeyTranslate on main, retain/copy layout data for
its actual lifetime, reset dead-key state and select fewest required modifiers,
then modifier bits, then keycode. OR required layout modifiers with configured
modifiers. If absent/unreadable/no candidate, use ISO 0x0A with a diagnostic: it
does not guarantee `§` on ANSI/nonmatching layouts. Report collisions with ordinary
physical chords and layout-expanded section chords deterministically; do not
silently double-dispatch or reinterpret duplicate raw spellings.

An owned input-source observer queues/coalesces layout-refresh hints, no inline
query/register/wait. Re-resolve/re-register on input-source change; superseded
refresh work rechecks generation/revision/layout identity before native mutation
and publication. Invalidate obsolete layout routes before replacements can fire.
Partial/failed replacement preserves a truthful last-good registration set or
explicit degraded subset; do not report the new layout fully active. Preserve
unaffected chords and surface per-binding failures.

Logical removal inhibits dispatch immediately; main-owner unregister/observer
removal uses part 02's **2 s acknowledgement / 10 ms cancellable wait quantum**,
retains failed IDs/inert guards and reports
pending cleanup. Drop alone is not proof. Prepare staged registrations inertly;
support unchanged resource reuse and transfer at an explicit commit without
unregistering all last-good shortcuts before a candidate is accepted. Existing
chords cannot be registered twice just to stage a reload.

## Acceptance criteria

- [ ] **KB-03-01** — Legacy ordinary parsing/normalization/routing and deterministic
      duplicates pass the compatibility corpus, including empty-command bindings.
- [ ] **KB-03-02** — Real Carbon/TIS ownership and safe Send IDs compile without
      Tauri or invented Send/Sync; foreign/release/stale events never dispatch.
- [ ] **KB-03-03** — Layout resolution, required modifiers, fallback and collisions
      produce deterministic registrations and honest per-binding diagnostics.
- [ ] **KB-03-04** — Input-source replacement and staged resource reuse reject
      stale work without losing or double-registering accepted chords; central
      Carbon reservations match actual routes across commit/layout/removal.
- [ ] **KB-03-05** — Registration/removal/observer failures retain exact ownership,
      expose partial/degraded outcomes and separate invalidation from cleanup.
- [ ] **KB-03-06** — Yielding a Carbon reservation suppresses its dispatch for
      exactly the yielded lifetime, restores on claimant release/pause/stop/disable/
      reload, keeps unrelated bindings, and cannot double-dispatch one physical press.

## Tests and gates

Pure/fake tests cover archived aliases/key tables, insertion-order permutations,
malformed syntax, § placement/unknown modifiers, Caps routing, quoted commands
remaining opaque to the planner, candidate tie breaks, required-modifier merges,
ANSI/ISO layout fixtures and missing layout data. Inject handler-install failure,
one occupied chord, ordinary/section physical collision and cleanup failure.
Barrier-test layout switch during reload/stop, coalesced refresh, a queued old
Carbon event after ID replacement, partial staging rollback and unchanged-chord
reuse, central reservation conflicts with HID filters, stale reservation removal
and failed-registration absence. Fake time verifies shared acknowledgement bounds
and late cancelled owner work. Fakes prove no per-callback native query/process spawn.
Prove yield/restore across claimant pause/stop/reload, double-yield idempotence,
yield of a non-existent/stale reservation, and that a yielded chord never executes
its configured commands while another binding on the same physical press does not
double-run.

Before part 04/05 approval, compile a safe adapter against actual Carbon/TIS and
the chosen parser; show owner-local destruction and cross-thread value handles.
Run crate/app tests, warnings-denied rustdoc and root checks from part 01. Do
not register real hotkeys or change layouts in default tests. Optional manual
ordinary/section/layout checks require approval for each run, a harmless owned
command fixture, conflict report and confirmed unregister. One **required measured
gate** before Hold-to-Quit part 04 relies on cross-backend priority: with a
suppressing HID tap installed, determine whether a Carbon `RegisterEventHotKey`
chord still fires; record the exact tap mask, chord, OS build and observed result.
Mark it unverified if not run. Update SDK/testing
docs and feature notice only for adapter availability, not shipped activation.
