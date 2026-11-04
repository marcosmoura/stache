# Review lessons applied to Bar

Read when auditing the draft against C01–C20. This is a planning applicability audit, not runtime verification or a change to sibling criteria/statuses.

Sources inspected read-only with `git show`: `f13cd3a:docs/specs/2026-11-03-wallpapers/review-findings.md` and `2ddd0e3:docs/specs/2026-11-03-keep-awake/review-findings.md`. [Keep Awake](../2026-11-03-keep-awake/review-findings.md)/[Wallpapers](../2026-11-03-wallpapers/review-findings.md) are draft records now on `main`, not shipped implementations. Main `ad34b46` already includes shipped noTunes `ccae030`; the older audit's noTunes in-progress attribution is historical, not current status.

| Lesson | Bar application / owning part                                                                                                                                                                          |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| C01    | 08 preserves Keep Awake's key-absent compatibility-unlocked versus failed/nonboolean unknown; no Bar probe or guessed active state.                                                                    |
| C02    | 03/07 invalidate logically first; removal/close/reap acknowledged or truthful timeout with retained exact-generation owner and bounded cleanup retries.                                                |
| C03    | 03/08 app projection overlays held file diagnostics before registry diagnostics, read-only; tests identical-good repair.                                                                               |
| C04    | 08 never infers active/inactive from enable/stop; Keep Awake owns acquiring/releasePending/failed release and retained IDs. Bar has no lease.                                                          |
| C05    | 03/05–07 stage bounded provider jobs off-main; no cold native/network/decode job blocks host/main readiness. Window loaded readiness is separately acknowledged, never inferred from queue acceptance. |
| C06    | 02/03 reuse same-cell typed accepted watch, publication-before-wake, cancellable waits and actual revision/equality; test reconfigure acceptance barrier.                                              |
| C07    | 03/07 inert registration, begin-shutdown invalidation before IPC joins, registry stop before factory finish/drain, including partial starts and disabled resources.                                    |
| C08    | 00 requires current-main rebase/reuse and additive catalog/bindings/router/index tests; no unmerged API claims.                                                                                        |
| C09    | 05/08 concrete operation outcomes live in generated command/Response::Ok data; issued/handled is not confirmed success, uncertainty is not JSON text in wire errors.                                   |
| C10    | 02 touches existing app host factory context minimally, compiles existing consumers, no tool-specific service framework.                                                                               |
| C11    | Wallpapers exclusive-clear lease/5s exact stderr is not applicable: Bar exposes no cache-clear command. 07 still bounds cache/native waits and cancellation without importing that command policy.     |
| C12    | Wallpapers coordination lock is not introduced. 07 uses private/atomic bounded cache access and avoids unsafe path/symlink assumptions; never borrows/unlinks another module's lock.                   |
| C13    | Wallpapers `generate-all` empty text is not applicable; no generator command. 04/07 explicitly test genuine absent media/battery/provider states rather than fabricated data.                          |
| C14    | Wallpapers list/argument grammar is not applicable; Bar adds no CLI grammar/widgets/settings. 01 and 00 name intentional migration/default/click corrections explicitly.                               |
| C15    | Wallpapers partial desktop application errors/text stay owned there. 05/08 distinguish pre-issuance cancellation from issued uncertain actions and never retry uncertain launches.                     |
| C16    | 01/06 trim config strings before home expansion, config-directory base; no arbitrary CLI path argument added; migration warns on unprovable old base.                                                  |
| C17    | Wallpapers standalone-set/no-socket race is not applicable: Bar exposes no standalone mutation fallback. 05/08 reject unavailable host/actions, never retry locally after uncertainty.                 |
| C18    | 03/05 native callbacks enqueue/coalesce signals only with panic containment; query/decoding/policy/wait stays off callback, native commit rechecks generation/permit.                                  |
| C19    | 00 and planned feature/index label all work draft, distinguish shipped noTunes and separate unmerged plans, preserve existing routes.                                                                  |
| C20    | 08 Keep Awake owns runtime DTO/codec; app owns envelope/ModuleStatusView projection and generated bindings, SDK/Bar do not copy or depend on its module crate.                                         |

Acceptance IDs remain owned exactly once in the eight parts; this audit adds no duplicate criteria. Native feasibility, baseline evidence, performance and compile-proof are future implementation gates, not satisfied by this document.
