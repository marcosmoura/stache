# Planned CLI compatibility contract

Read when implementing part 07. **Draft, not shipped.** Source pin `ad34b46` legacy files; this appendix owns no acceptance IDs. Table/render bodies are evidence requirements, not permission to execute legacy automation.

## Grammar and ordering

```text
stache [--config DIR] tiling pause|resume
stache tiling query [-j|--json] [-d|--detailed] [screens|apps]
stache tiling query [-j|--json] [-d|--detailed] workspaces [--focused-screen|--screen NAME]
stache tiling query [-j|--json] [-d|--detailed] windows [--focused-screen|--focused-workspace|--screen NAME] [--workspace NAME]
stache tiling window [--focus DIRECTION|WINDOW_ID] [--swap DIRECTION] [--toggle-floating] [--preset NAME] [--resize DIMENSION AMOUNT]... [--send-to-screen NAME] [--send-to-workspace NAME]
stache tiling workspace [--focus NAME] [--layout LAYOUT] [--balance] [--send-to-screen NAME]
```

Query with no subcommand prints query help + newline, exit0, **not** all results (legacy comment is wrong; `execute_query:259–264` is authority). JSON/detailed flags are global within query, before or after nested command. `--config`/`-c`/`--config=DIR` remain main CLI directory syntax before/after subcommands; never legacy file-path semantics or a desktop launch.

Workspace focused-screen conflicts with screen. Windows focused-screen conflicts with screen/workspace/focused-workspace; focused-workspace conflicts with screen/workspace/focused-screen. Screen + workspace is allowed together. Screens ignores detailed; apps detailed currently changes nothing. Preserve these facts, not advertised-but-unimplemented columns.

Focus and swap both accept up/down/left/right/previous/next. Focus is a free-form string: case-insensitive directions, the `previous`/`prev` alias, and a numeric current window ID (approved correction; legacy help advertised it but the handler rejected it). Swap and `--layout` are clap value enums: exact lowercase spellings only, six values, no `prev` alias. Layout names are the eight overview names. Resize is repeated two-value width|height + signed i32, dimension case-insensitive at semantic validation; negative amounts allowed. Invalid swap/layout enum, arity and flag conflicts use clap exit2. Invalid focus direction/target and invalid resize dimension/amount use semantic exit1 at the JSON/IPC phase, not a new clap enum that changes exit status. Window no flags and workspace no flags are semantic exit1.

Window actions execute focus → swap → toggle → preset → resize pairs in supplied order → send screen → send workspace, regardless flag input order. Workspace: focus → layout → balance → send screen. Stop at first reported failure; earlier admitted actions remain. `--preset` applies only in a Floating workspace; elsewhere it stays a silent admitted no-op with exit0 as legacy already behaves, and no frame is reported applied. Legacy validates each resize pair at its execution point, after preceding actions; preserve and document this nontransactional boundary. No aggregate all-or-nothing promise.

## Successful query wire shapes

All arrays preserve actor snapshot order (filter only, no new name sort); JSON pretty-prints whole legacy shape regardless detailed. No refreshRate/native opaque token/diagnostic added to these legacy arrays; richer status is a separately typed host/provider DTO.

| Query      | Fields (camelCase; source `init.rs`)                                                        |
| ---------- | ------------------------------------------------------------------------------------------- |
| screens    | `id,name,isMain,isBuiltin,scaleFactor,frame,visibleFrame`; frame objects `x,y,width,height` |
| workspaces | `id` string, `name,screenName,layout,isVisible,isFocused,windowCount`                       |
| windows    | `id,pid,appId,appName,title,workspace,frame,isMinimized,isFullscreen,isFloating,isFocused`  |
| apps       | `pid,name,bundleId,isHidden`                                                                |

Numeric CG IDs/PIDs remain legacy display/selector fields, not action authority. Grid `isFloating` reflects effective floating including overflow; explicit user flag and overflow reason stay in richer diagnostics. Screens come from actor topology, apps from confirmed owner catalog. Query apps source actually filters system skip rules via `should_tile_window`, not arbitrary title-ignore rules despite its comment. Preserve source behavior; no new AX walk in handler. Unknown fields/malformed DTO fail, never `unwrap_or_default` empty success.

Query screen aliases are exact `main`/`primary`/`secondary`; named screens use exact name. Secondary query includes all nonmain displays. Workspace filter is exact name. Focus filters with no focus yield empty results. Source: `init.rs:1535–1572,1650–1711`.

## Human output

Rounded table borders, legacy alignment and `✓`/`✗`; ANSI only injected terminal policy, never pipe escapes. Headings have newline: `Screens (N)`, `Workspaces (N)`, `Windows (N)`, `Running Apps (N)`. Empty bodies exactly `No screens detected.`, `No workspaces found.`, `No windows found.`, `No running apps found.` + newline, exit0. JSON empties are `[]` pretty rendering + newline.

| Table                | Columns in exact order                                              | Truncation/alignment                                                     |
| -------------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| screens              | ID, Name, Resolution, Position, Scale, Main, Built-in               | ID/resolution/position/scale right; booleans center; `WxH`, `x, y`, `Sx` |
| workspaces           | Name, Screen, Layout, Windows, Visible, Focused                     | Screen15 chars; Windows right; booleans center                           |
| windows              | ID, App, Title, Workspace, Frame, Focused                           | App20/title35 chars; ID/frame right; focused center                      |
| windows detailed     | ID, PID, App, Bundle ID, Title, Workspace, Frame, Min, Float, Focus | App15/bundle25/title25; ID/PID right; final3 center                      |
| apps (also detailed) | PID, Name, Bundle ID, Visible                                       | Name25/bundle35; PID right; visible center = !isHidden                   |

Frame text: `WxH @ x, y`, integer truncation as legacy for ordinary finite supported coordinates; source primitive casts do not authorize undefined/nonfinite geometry. Title empty→`(no title)`; missing required DTO fields is an error, not `?` fabricated success. Truncate by Unicode scalar count to max−1 + `…`, source `cli/output.rs:125–141`. UI truncation is different (part10); do not conflate. Goldens fix every table glyph, padding, row order, rounding, title escape and newline, plus ANSI-stripped equality and JSON key rendering order from source fixtures.

## Outcomes, streams and errors

Legacy `bar/ipc_listener.rs:39–50` returns queued acknowledgement. Keep silent stdout/stderr exit0 for Enqueued or Applied mutation response; documentation calls this admission, never native success. `pause` cannot return successful Enqueued cleanup: waits for lifecycle acknowledgement. Module-owned `Response::Ok` operation DTO encodes tagged Enqueued/Applied/Partial/Uncertain and structured target/component reasons. Transport/query failures are actual wire/client errors, not JSON-encoded error strings inside data.

Query failures retain **stdout** legacy bodies and exit1 (intentional exit correction): offline table `Stache app is not running.\n`; offline JSON `{"error":"Stache app is not running"}\n`; other error table `Error: {body}\n`, JSON `{"error":<serde-escaped body>}\n`. No duplicate stderr. Typed renderer supplies body; quotes/control characters are escaped by serializer, never interpolation. Remote unavailable/disabled/malformed/decode failure is not empty exit0.

Semantic/action errors use existing main CLI stderr `stache: {body}\n`, exit1; IPC body prefix is `IPC error: `. Preserve source bodies when applicable:

- `No window operation specified. Use --help for available options.`
- `No workspace operation specified. Use --help for available options.`
- `Invalid resize dimension '{dimension}'. Must be 'width' or 'height'.`
- `Invalid resize amount '{amount}'. Must be an integer.`
- `Tiling is not initialized`, `Tiling handle is unavailable`, `No focused workspace`, `No focused window`, `Workspace not found`.
- `Invalid focus direction: {target}`, `Invalid swap direction: {direction}`.
- Actor channel bodies `Failed to send message to actor: channel closed`, `Actor overflow mailbox is full`, `Failed to receive response from actor: channel closed`; timeout body includes deterministic duration rendering, not elapsed wall-clock jitter.

New typed physical failure adapters (stderr, exit1, never appended success text): `stache: IPC error: Tiling operation partially applied.\n` and `stache: IPC error: Tiling operation outcome is uncertain; it may already have taken effect.\n`. Structured per-target details remain Response::Ok/status data. Proven pre-send rejection uses its typed reason, not uncertainty. Unknown numeric focus target uses `Window {id} not found`; never retarget by reused ID after resolution. New lifecycle bodies are `Tiling is disabled`, `Tiling is paused`, `Tiling cleanup is pending`; unavailable/uninitialized keeps `Tiling is not initialized`. Use existing disabled/unavailable/handler wire categories, not new wire variants. These explicitly diagnosed lifecycle/physical failures are the necessary error-semantics differences, not changed successful output. No raw Debug dump or automatic retry after uncertainty.

Map typed SDK client errors through this module's compatibility adapter, not SDK lowercase Display: NotRunning→`Stache app is not running`, Timeout→`Connection timed out`, Io→`IO error: {source}`, InvalidResponse→`Invalid response: {detail}`, Remote→its frozen typed body. Serialization failure uses `Cannot serialize tiling request`. Query errors use stdout forms above; action errors wrap with stderr `stache: IPC error: {body}\n`. OS/decoder source details can vary; goldens inject fixed sources and freeze prefix/escaping, not an invented universal platform error. A decode failure must not echo sensitive payloads or be silently suppressed.

## Golden inventory and intentional differences

Part07 must inventory source error adapters `cli/commands/tiling.rs`, `cli/commands/types.rs`, `cli/output.rs`, `error.rs`, `modules/bar/ipc_listener.rs`, `modules/tiling/{init.rs,actor/handle.rs}` and `platform/ipc_socket.rs` before accepting fixtures. Compare actual CLI→socket→router→DTO serde, not just hand-written renderer units. Keep all successful commands/flags/JSON/tables/order unchanged. Intentional changes are query failures exit1, safely escaped failure JSON, typed partial/uncertain physical errors, truthful admission docs and safe selector identity; no additional output decision is implied. Host absence does not start desktop or make mutation succeed. Parse exit2 and semantic exit1 remain distinct.
