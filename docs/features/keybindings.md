# Keybindings

Read when configuring, running or debugging global shortcuts, Caps Lock chords or `§` shortcuts in Stache Next.

**Behavior.** With `keybindings.toml` `enabled = true`, each valid binding becomes a global shortcut
that runs its commands. Nothing is registered, subscribed or spawned until the module starts:
registration, a disabled file and a pause create no hotkey, tap, remap or worker, and an intentionally
empty `bindings` map is valid and inactive. Commands run directly (no shell) through the shared
runner, sequentially inside one accepted press and stopping at the first failure; an empty command
list captures the shortcut without spawning. Ordinary and `§` shortcuts use Carbon hotkeys on the main
thread and need no Accessibility permission of their own (the app-wide gate still holds them until it is granted). Only `CapsLock+<key>` bindings use the shared
suppressing input tap and the global Caps Lock to F18 remap, and only they require Accessibility; that
permission is **user-prompt-only** (the host never prompts on startup, enable, reload, tick or
activation; the tray request prompts). Accessibility is also a launch requirement of the whole app,
so while it is missing the Keybindings module waits too and ordinary shortcuts register only after
the grant; see the launch gate in [overview.md](../architecture/overview.md). A configuration that
newly needs it is accepted and waits;
a trusted but failed tap is a start failure, not a missing permission. Granting starts it; revoking
stops all Caps activity and restores the mapping.

**Trust.** Bindings run as the user; see the configuration [trust model](../architecture/config.md#trust-model).

**Registration is honest.** One bad shortcut spelling is skipped with a diagnostic; a duplicate (after
alias normalization) is replaced by the later one in lexical order. A chord another application owns,
an unsupported key or a failed Caps start is reported per binding. The module runs with the exact active
subset (state `degraded`); if every requested usable binding fails, start fails and everything acquired
is released. Retry (enable again) repairs a failed start.

**Runtime status.** The host status carries an optional `keybindings` object on the `keybindings`
module view (generated TypeScript type `KeybindingsRuntimeStatus`): the accepted `revision` the runtime
adopted as a decimal string, `state` (`inactive`, `active`, `degraded`), per-binding `state`/`route`/
`detail`, `tap` (`notRequired`, `ready`, `absent`, `disabled`, `reenableFailed`, `retained`), the Caps
`remap.phase` (`inactive`, `applying`, `owned`, `cleanupPending`, `uncertain`, `ownershipLost`, with a
`detail` and `f18Exposed`), dispatch counters (active/waiting, overflow, window, unrouted and lost
presses, completed/failed/cancelled, generation) and retained cleanup (inert hotkey IDs, a pending
Carbon handler, unreaped children, unresolved Caps restore) plus recent runtime diagnostics. It is
published separately from the lifecycle status and the held-file diagnostic: reading it takes no module
lock, waits for no native work and reads no file. `diagnostic` stays the rejected-file channel. It is
overlaid when the host publishes, so it is a point-in-time observation. There is no new CLI, socket or
Settings surface.

**Reload.** Editing the file stages the candidate without side effects (no command can run and no
route exists for it; Carbon candidates are prepared inertly and the Caps remap is untouched). A file
the registry rejects, or an unchanged one, changes nothing and cancels nothing. Once an accepted
revision is published the runtime observes it, then invalidates the old dispatcher generation (queued
sequences are discarded, only directly spawned active children are terminated), commits the staged
shortcuts and finally adds, removes or retains the Caps remap. Between publication and observation no
press from either binding set runs (a press in that window is counted and dropped). A sequence already
queued or running from the old set starts no further command once the new revision is published, even
before the runtime observes it; a command that had already started is not undone. Failures after
publication (a chord that cannot be registered at commit, a Caps start or restore failure) are an
accepted configuration with a degraded or cleanup outcome in the status, never a rejection. Changing the
set of Caps keys briefly restores and re-applies the remap.

**Stop.** Pause, disable, stop and shutdown clear the routes and invalidate the dispatcher first,
then release Carbon and Caps with fresh bounded waits. A stop is repeatable. A cleanup that cannot be
confirmed (a hotkey that will not unregister, a Caps restore that is pending or uncertain) is retained
by the host-lifetime Carbon store and Caps owner, shown in the status and returned as a stop error; it
never blocks unrelated shutdown and is retried by the next stop. Caps restoration after a crash is the
host's startup recovery below.

**Caps recovery.** On every desktop start the host runs one bounded
step that restores a Caps Lock to F18 mapping a crashed Stache Next left behind, if and only if
Next's own journal under `~/Library/Application Support/com.marcosmoura.stache.next/keybindings/`
exists. It runs whether or not the module is enabled or its file is valid, never touches a
foreign or legacy mapping, never retries, and a failure is shown once as a host condition
without delaying startup. The module's first Caps apply waits (bounded by 20 s, cancellable) for
that step to finish, so recovery never restores a fresh mapping; a recovery that has not finished
in time is a start failure with nothing changed. With no journal it does nothing and never starts `hidutil`. A crash
needs a relaunch to recover; until then physical Caps Lock may send F18. The remap is global
(no per-keyboard identity), so a physical F18 key is swallowed too while a mapping is active,
and `hidutil` has no compare-and-swap, so another program can race a restore. The real remap
and crash recovery are **unverified** against a live system.

**Planning.** `Plan::from_config` normalizes aliases per `+` component (`Ctrl`,
`Cmd`/`Super`/`Meta`, `Alt`/`Opt`, backtick) and routes each raw key: `CapsLock+<key>`
first, then `§` (last component, only Command/Shift/Option/Control; `CapsLock+§` is the
physical ISO key `0x0A`), then the ordinary grammar of `global-hotkey` 0.10.0, the parser
the archived application used at 0.8.0 (parsing only, no native manager). 0.10.0 also parses
key names 0.8.0 rejected (`F25` to `F35`, international, launch and media keys); none has a
Carbon virtual key, so registration reports them unsupported. Raw keys are planned in
lexical order; the last semantic duplicate wins and the diagnostic names both spellings.
Invalid entries are skipped with a per-binding diagnostic. Commands are trimmed and blank
ones dropped; an empty list captures without spawning. Section entries carry the
configured modifiers and wait for native layout resolution.

`keybindings.toml` is registered (`stache config init`, `stache schema keybindings`)
with `enabled = false` and `bindings: {}` by default. Values map a shortcut to a string or
an ordered string array; a malformed shortcut spelling is not a file rejection.
`config migrate` wraps the legacy `keybindings` map as `bindings` and enables the file
only when the map is nonempty; shortcut keys are written in lexical order and each
command list keeps its order.

**Command dispatcher ([part 05](../specs/2026-11-03-keybindings/05-runtime-integration.md)).**
`stache_keybindings::Dispatcher` runs accepted command sequences through the shared runner. At most 16 sequences run at once and 64 more wait in FIFO
order; these are intentional safety limits. Tasks are created only when work is admitted.
Independent presses, including repeated presses of one shortcut, run concurrently; only the
steps inside one sequence are sequential and stop at the first failure. At saturation the
newest submission is rejected and counted (accepted work is never evicted and new work never
bypasses a waiter); a binding with no commands bypasses the limit. A long-running child
occupies only its own slot and has no duration timeout. Tap reservation and dispatcher
acceptance are one budget. A Carbon press that finds the dispatcher full is a diagnosed lost
invocation, never run late.
Pause, disable, stop and an accepted changed reload invalidate the generation: queued
sequences are discarded and only directly spawned active children are killed and reaped,
and no process starts for the old generation afterwards. A rejected or unchanged reload
cancels nothing. A child that cannot be confirmed gone stays owned by a bounded reaper; when
that ownership is exhausted new dispatch is refused with a diagnostic.

**Known interaction: `stache reload`.** A binding that runs `stache reload` is terminated
by the accepted Keybindings reload its own command causes, because that reload invalidates
the generation and cancels the running sequence. Later steps of the same sequence never run,
so do not rely on steps after a reload command.

See the
[archived behavior](../../_legacy/docs/features/keybindings.md) for the independent
old application. Do not apply legacy Caps remap commands/state files to Next.

**Unverified.** Everything above is exercised against fakes only. Real Carbon registration, the real
HID-tap versus Carbon priority (whether a suppressing tap already prevents Carbon dispatch), layout
switching, a live Caps remap, crash recovery and process lifecycle have not been measured; each manual
run needs separate approval. The Cmd-Q and logout teardown gap is separate. A tap-callback press submits
to the dispatcher inline (it may reap a finished task and create a thread), which is bounded but not
allocation-free. No new Keybindings CLI, Settings or other module is included.
