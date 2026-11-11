//! The pure border model: which windows get a
//! border, where, and how it looks, computed from [`TilingState`] alone.
//!
//! Nothing here reads a clock, takes a lock or calls native code. The engine
//! diffs successive models into overlay batches (`engine.rs`).
//!
//! Approach studied, never code (decision 5 of the spec): the reference
//! border tool's `src/events.c` and `src/windows.c` decide per window, after the window
//! server reports it, whether a border exists. Stache decides from its own
//! state, before and around its own effects.
//!
//! # Who is bordered (decision 2)
//!
//! A tracked, managed window (tiled or floating) that is on a visible
//! workspace, not ignored (tiling `ignore`, `borders.ignore`, the system skip
//! lists `SKIP_TILING_BUNDLE_IDS` and `SKIP_TILING_APP_NAMES`, Stache itself),
//! not minimized, fullscreen or hidden, not an inactive tab or suspended slot
//! member, with a known `cg_window_id`, a frame with an area and a state that
//! draws something. Ignore entries are matched against the application's
//! bundle id and localized name (resolved OQ-6), never the executable name.
//! Each entry's `app_id` and `app_name` are independent alternatives (a title-only entry adds nothing), and the comparison is the
//! one the Tiling rules use (case-insensitive id, case-insensitive name
//! substring).
//!
//! A suspended window that still holds its slot (`slot_held`) is listed with
//! `held` set: the engine keeps an existing overlay in place for it and never
//! creates one.
//!
//! # Appearance
//!
//! Only the focused window uses the focused state of today's precedence
//! ([`focused_state`]: floating, then the layout override, then `focused`);
//! every other window uses `unfocused` (resolved OQ-3). Every border uses the
//! width of the active state (resolved OQ-1), which is the focused window's
//! state, or `focused` when no window is focused. A ping-pong motion is only
//! installed on the focused border; a `once` or `loop` rotation on every border
//! whose active state carries it ([`motion_to_install`]).
//!
//! # Reserved: parked windows
//!
//! The snapshot-animation spec parks a window in a display corner during its
//! animation. A parked window must not be bordered. Whichever spec lands second
//! adds a `parked` set input to [`border_model`] and a scenario test; nothing
//! exists for it yet.

use std::collections::{BTreeMap, BTreeSet};

use stache_macos::border_overlay::{BorderAppearance, Transition};
use stache_macos::windows::WindowInstance;
use stache_macos::{ApplicationId, DisplayId};

use super::appearance::{FocusInput, active_width, border_appearance, focused_state};
use super::motion::{motion_to_install, state_motion, transition_for};
use crate::config::{BorderStateConfig, BordersConfig, WindowRule};
use crate::geometry::Rect;
use crate::rules::{WindowMetadata, is_system_app_name, is_system_bundle};
use crate::state::{AttemptStatus, TilingState};

/// One bordered window.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts the engine reads separately"
)]
pub struct BorderEntry {
    /// The exact window.
    pub window: WindowInstance,
    /// Its application (bundle id and name resolve through this).
    pub application: ApplicationId,
    /// The `CGWindowID` the overlay is ordered against.
    pub cg_window_id: u32,
    /// The last confirmed frame, global top-left points.
    pub frame: Rect,
    /// This window is the focused one.
    pub focused: bool,
    /// The facts the focused state depends on (kept to recompute in place on a
    /// configuration reload).
    pub focus: FocusInput,
    /// What the border looks like, motion included.
    pub appearance: BorderAppearance,
    /// The window's current layout plan sequence: its newest frame attempt, else
    /// the global plan sequence. Frame events for an older one are stale.
    pub plan_sequence: u64,
    /// A suspended window still holding its slot: keep an existing overlay,
    /// never create one.
    pub held: bool,
    /// A frame write the border must wait for before it may show: `Some(seq)`
    /// is satisfied once a frame for plan `seq` or later is confirmed. Set for
    /// a window still marked revealed (its write is not issued yet, so the
    /// next plan sequence) and for one whose attempt is in flight.
    pub write_pending: Option<u64>,
    /// The application is hidden or being unhidden by the runtime. The border
    /// waits for the shown observation.
    pub app_hidden: bool,
    /// The display the window is on, to pace live follow.
    pub display: Option<DisplayId>,
    /// The owner process, so live follow drops Stache's own windows.
    pub owner_pid: i32,
    /// Part 09: an unfocused window kept only so its border can shrink, because
    /// `unfocused = false` and a transition is set ([`collapses_on_blur`]). It
    /// never shows a border; `appearance` is the look it would have focused.
    pub collapsed: bool,
}

/// The bordered windows, keyed by exact window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BorderModel {
    /// Bordered windows (and `held` ones).
    pub entries: BTreeMap<WindowInstance, BorderEntry>,
    /// The focused window's focus facts, when the focused window is known. It
    /// selects the active state whose width every border uses.
    pub active_focus: Option<FocusInput>,
    /// Windows that qualified in every other way but have no `cg_window_id`
    /// (reported as a diagnostic, never drawn).
    pub missing_cg_id: usize,
    /// Windows that qualify in every way except that they are fullscreen (on a
    /// visible workspace, not ignored, with a frame and a drawn state). The
    /// engine turns their flag transitions into `FullscreenChanged` facts.
    pub fullscreen: BTreeSet<WindowInstance>,
    /// Bordered windows under the mouse-down of a drag gesture that is in
    /// progress or settling (the public fallback hides them, part 06).
    pub pressed: BTreeSet<WindowInstance>,
}

impl BorderModel {
    /// Recomputes every appearance for `config` in place (a configuration
    /// reload). Entries whose state no longer draws are dropped.
    pub fn reappearance(&mut self, config: &BordersConfig) {
        let width = active_width(active_state(config, self.active_focus));
        self.entries.retain(|_, entry| {
            match entry_appearance(config, entry.focused, entry.focus, width) {
                Some((appearance, collapsed)) => {
                    entry.appearance = appearance;
                    entry.collapsed = collapsed;
                    true
                }
                None => false,
            }
        });
    }
}

fn active_state(config: &BordersConfig, focus: Option<FocusInput>) -> &BorderStateConfig {
    focus.map_or(&config.focused, |focus| focused_state(config, focus))
}

/// The appearance of one window, or `None` when its state draws nothing.
fn appearance_for(
    config: &BordersConfig,
    focused: bool,
    focus: FocusInput,
    width: u32,
) -> Option<BorderAppearance> {
    let state = if focused {
        focused_state(config, focus)
    } else {
        &config.unfocused
    };
    let mut appearance = border_appearance(config, state, width)?;
    appearance.motion = motion_to_install(state_motion(state), focused);
    Some(appearance)
}

/// Part 09: whether a window losing focus shrinks its border to zero width
/// instead of losing it at once. Only when unfocused windows draw no border
/// (`unfocused = false`) and the `transition` is not instant.
#[must_use]
pub fn collapses_on_blur(config: &BordersConfig) -> bool {
    matches!(config.unfocused, BorderStateConfig::Disabled(false))
        && transition_for(config).and_then(Transition::effective).is_some()
}

/// The appearance of one window and whether it is a collapsed entry (part
/// 09), or `None` when the window is not in the model at all.
fn entry_appearance(
    config: &BordersConfig,
    focused: bool,
    focus: FocusInput,
    width: u32,
) -> Option<(BorderAppearance, bool)> {
    if let Some(appearance) = appearance_for(config, focused, focus, width) {
        return Some((appearance, false));
    }
    if focused || !collapses_on_blur(config) {
        return None;
    }
    appearance_for(config, true, focus, width).map(|appearance| (appearance, true))
}

/// Whether an `ignore` entry names this application: its bundle id or its
/// localized name (alternatives, not both).
fn border_ignored(rules: &[WindowRule], bundle_id: &str, name: &str) -> bool {
    let bundle = bundle_id.to_lowercase();
    let name = name.to_lowercase();
    rules.iter().any(|rule| {
        rule.app_id.as_deref().is_some_and(|id| id.to_lowercase() == bundle)
            || rule
                .app_name
                .as_deref()
                .is_some_and(|wanted| !name.is_empty() && name.contains(&wanted.to_lowercase()))
    })
}

/// Computes the border model of `state` under `config`.
#[must_use]
pub fn border_model(state: &TilingState, config: &BordersConfig) -> BorderModel {
    let mut model = BorderModel::default();
    let focused_window = state.focus.window;
    model.active_focus = focused_window.and_then(|window| focus_input(state, window));
    let width = active_width(active_state(config, model.active_focus));
    let tiling_ignore = &state.ignore;

    for workspace in state.workspaces.iter().filter(|workspace| workspace.visible) {
        for token in &workspace.order {
            let Some(window) = state.windows.get(token) else {
                continue;
            };
            if window.desired.workspace != Some(workspace.id) {
                continue;
            }
            let flags = state.flags_of(window);
            if flags.minimized || flags.hidden || window.is_inactive_tab() {
                continue;
            }
            // A suspended window that lost its slot is gone; one still holding it
            // stays as `held`.
            let held = window.suspended && window.slot_held;
            if window.suspended && !held {
                continue;
            }
            let Some(record) = state.apps.get(&window.application) else {
                continue;
            };
            let info = &record.info;
            let own = !state.self_bundle_id.is_empty()
                && info.bundle_id.eq_ignore_ascii_case(&state.self_bundle_id);
            if own
                || record.skipped
                || is_system_bundle(&info.bundle_id)
                || is_system_app_name(&info.name)
                || tiling_ignore.any_match(&WindowMetadata {
                    app_id: &info.bundle_id,
                    app_name: &info.name,
                    title: &window.observed.title,
                })
                || border_ignored(&config.ignore, &info.bundle_id, &info.name)
            {
                continue;
            }
            let Some(frame) = window.observed.frame.filter(Rect::has_area) else {
                continue;
            };
            let focused = focused_window == Some(*token);
            let focus = FocusInput {
                layout: workspace.layout,
                floating: window.desired.floating,
            };
            let Some((appearance, collapsed)) = entry_appearance(config, focused, focus, width)
            else {
                continue;
            };
            if flags.fullscreen {
                model.fullscreen.insert(*token);
                continue;
            }
            let Some(cg_window_id) = window.cg_window_id else {
                model.missing_cg_id += 1;
                continue;
            };
            let attempt =
                state.frames.records.get(token).and_then(|record| record.attempt.as_ref());
            let write_pending = if state.frames.revealed.contains(token) {
                Some(state.frames.plan_sequence + 1)
            } else {
                attempt
                    .filter(|attempt| matches!(attempt.status, AttemptStatus::InFlight))
                    .map(|attempt| attempt.plan_sequence)
            };
            model.entries.insert(*token, BorderEntry {
                window: *token,
                application: window.application,
                cg_window_id,
                frame,
                focused,
                focus,
                appearance,
                plan_sequence: attempt.map_or(state.frames.plan_sequence, |a| a.plan_sequence),
                held,
                write_pending,
                app_hidden: record.hidden || state.owned_hidden.contains(&window.application),
                display: state.display_of(*token),
                owner_pid: info.pid,
                collapsed,
            });
        }
    }
    model.pressed = state
        .drag
        .pressed()
        .into_iter()
        .filter(|token| model.entries.contains_key(token))
        .collect();
    model
}

fn focus_input(state: &TilingState, window: WindowInstance) -> Option<FocusInput> {
    let tracked = state.windows.get(&window)?;
    let workspace = state
        .workspaces
        .iter()
        .find(|workspace| Some(workspace.id) == tracked.desired.workspace)?;
    Some(FocusInput {
        layout: workspace.layout,
        floating: tracked.desired.floating,
    })
}

#[cfg(test)]
mod tests {
    use stache_macos::border_overlay::{BorderColor, BorderMotion, BorderPaint, MotionRepeat};
    use stache_macos::windows::WindowEventKind;

    use super::*;
    use crate::config::{LayoutType, WindowRule};
    use crate::rules::PreparedRules;
    use crate::test_support::{Feed, app, info, ready, step};

    const EDITOR: u64 = 1;
    const OTHER: u64 = 2;

    fn config(value: serde_json::Value) -> BordersConfig {
        serde_json::from_value(value).expect("borders config")
    }

    /// A ready state with two applications and `n` windows of the editor.
    fn fixture(windows: u32) -> (TilingState, Feed, Vec<WindowInstance>) {
        let mut feed = Feed::new(1);
        let mut state = ready(
            "com.stache.app",
            &[
                info(EDITOR, "com.editor", "Editor"),
                info(OTHER, "com.other", "Other Tool"),
            ],
            &mut feed,
        );
        let mut tokens = Vec::new();
        for serial in 1..=windows {
            let token = feed.token(EDITOR, u64::from(serial));
            let (next, _) = step(state, feed.created(token, "w", 100 + serial));
            state = next;
            tokens.push(token);
        }
        (state, feed, tokens)
    }

    fn bordered(state: &TilingState, config: &BordersConfig) -> Vec<WindowInstance> {
        border_model(state, config).entries.keys().copied().collect()
    }

    fn solid(color: &str) -> BorderPaint {
        let rgba = super::super::appearance::parse_color(color).expect("color");
        BorderPaint::Solid(BorderColor {
            r: rgba.r,
            g: rgba.g,
            b: rgba.b,
            a: rgba.a,
        })
    }

    #[test]
    fn tiled_windows_are_bordered_with_their_frame_and_cg_id() {
        let (state, _feed, tokens) = fixture(2);
        let model = border_model(&state, &BordersConfig::default());
        assert_eq!(model.entries.len(), 2);
        let entry = &model.entries[&tokens[0]];
        assert_eq!(entry.cg_window_id, 101);
        assert_eq!(entry.frame, Rect::new(10.0, 10.0, 800.0, 600.0));
        assert_eq!(entry.application, app(EDITOR));
        assert!(!entry.held && entry.write_pending.is_none() && !entry.app_hidden);
    }

    #[test]
    fn floating_windows_are_bordered_too() {
        let (mut state, _feed, tokens) = fixture(1);
        state.windows.get_mut(&tokens[0]).expect("window").desired.floating = true;
        assert_eq!(bordered(&state, &BordersConfig::default()), tokens);
    }

    #[test]
    fn exclusions_follow_decision_two() {
        let config = BordersConfig::default();
        let (state, _feed, tokens) = fixture(1);
        let t = tokens[0];

        let mut s = state.clone();
        s.windows.get_mut(&t).expect("window").observed.minimized = true;
        assert!(bordered(&s, &config).is_empty(), "minimized");

        let mut s = state.clone();
        s.windows.get_mut(&t).expect("window").observed.fullscreen = true;
        assert!(bordered(&s, &config).is_empty(), "fullscreen");
        let model = border_model(&s, &config);
        assert_eq!(
            model.fullscreen.into_iter().collect::<Vec<_>>(),
            [t],
            "reported to the gate"
        );
        assert!(border_model(&state, &config).fullscreen.is_empty());

        let mut s = state.clone();
        s.windows.get_mut(&t).expect("window").observed.hidden = true;
        assert!(bordered(&s, &config).is_empty(), "hidden by someone else");

        let mut s = state.clone();
        s.windows.get_mut(&t).expect("window").inactive_tab = true;
        assert!(bordered(&s, &config).is_empty(), "inactive tab");

        let mut s = state.clone();
        {
            let window = s.windows.get_mut(&t).expect("window");
            window.suspended = true;
            window.slot_held = false;
        }
        assert!(bordered(&s, &config).is_empty(), "suspended slot member");

        let mut s = state.clone();
        for workspace in &mut s.workspaces {
            workspace.visible = false;
        }
        assert!(bordered(&s, &config).is_empty(), "invisible workspace");

        let mut s = state.clone();
        s.windows.get_mut(&t).expect("window").cg_window_id = None;
        let model = border_model(&s, &config);
        assert!(
            model.entries.is_empty() && model.missing_cg_id == 1,
            "unknown cg id"
        );

        let mut s = state.clone();
        s.windows.get_mut(&t).expect("window").observed.frame = None;
        assert!(bordered(&s, &config).is_empty(), "no frame");

        let mut s = state.clone();
        s.self_bundle_id = "com.editor".to_owned();
        assert!(bordered(&s, &config).is_empty(), "Stache's own app");

        let mut s = state;
        s.apps.get_mut(&app(EDITOR)).expect("app").info.bundle_id = "com.apple.dock".to_owned();
        assert!(bordered(&s, &config).is_empty(), "system bundle");
    }

    #[test]
    fn system_names_and_tiling_ignore_rules_are_excluded() {
        let config = BordersConfig::default();
        let (state, _feed, _tokens) = fixture(1);

        let mut s = state.clone();
        s.apps.get_mut(&app(EDITOR)).expect("app").info.name = "Control Center".to_owned();
        assert!(bordered(&s, &config).is_empty(), "system app name");

        let mut s = state;
        let rule = WindowRule {
            app_id: Some("com.editor".to_owned()),
            ..WindowRule::default()
        };
        s.ignore = PreparedRules::new(&[rule]);
        assert!(bordered(&s, &config).is_empty(), "tiling ignore");
    }

    #[test]
    fn a_suspended_window_holding_its_slot_is_kept_as_held() {
        let (mut state, _feed, tokens) = fixture(1);
        {
            let window = state.windows.get_mut(&tokens[0]).expect("window");
            window.suspended = true;
            window.slot_held = true;
        }
        let model = border_model(&state, &BordersConfig::default());
        assert!(model.entries[&tokens[0]].held);
    }

    #[test]
    fn border_ignore_matches_bundle_id_or_localized_name_never_the_executable() {
        let (state, _feed, _tokens) = fixture(1);
        let ignore = |rule: serde_json::Value| config(serde_json::json!({"ignore": [rule]}));

        // Only the bundle id matches (the name differs).
        assert!(
            bordered(
                &state,
                &ignore(serde_json::json!({"app_id": "com.editor", "app_name": "Nope"}))
            )
            .is_empty()
        );
        assert!(bordered(&state, &ignore(serde_json::json!({"app_id": "COM.EDITOR"}))).is_empty());
        // Only the localized name matches.
        assert!(
            bordered(
                &state,
                &ignore(serde_json::json!({"app_id": "com.nope", "app_name": "editor"}))
            )
            .is_empty()
        );
        assert!(bordered(&state, &ignore(serde_json::json!({"app_name": "Edit"}))).is_empty());
        // An executable-like name that matches neither stays bordered.
        assert_eq!(
            bordered(&state, &ignore(serde_json::json!({"app_id": "editor-bin"}))).len(),
            1
        );
        assert_eq!(
            bordered(&state, &ignore(serde_json::json!({"app_name": "editor-bin"}))).len(),
            1
        );
        // A title-only entry adds nothing.
        assert_eq!(
            bordered(&state, &ignore(serde_json::json!({"title": "w"}))).len(),
            1
        );
        // The other application is untouched.
        assert_eq!(
            bordered(&state, &ignore(serde_json::json!({"app_id": "com.other"}))).len(),
            1
        );
    }

    #[test]
    fn only_the_focused_window_gets_the_focused_state_and_overrides() {
        let config = config(serde_json::json!({
            "focused": {"width": 6, "color": "#112233"},
            "unfocused": {"width": 9, "color": "#445566"},
            "monocle": {"width": 7, "color": "#778899"},
            "floating": {"width": 2, "color": "#aabbcc"},
        }));
        let (mut state, _feed, tokens) = fixture(2);
        state.focus.window = Some(tokens[0]);
        state.workspaces[0].layout = LayoutType::Dwindle;
        let model = border_model(&state, &config);
        assert_eq!(model.entries[&tokens[0]].appearance.paint, solid("#112233"));
        assert_eq!(model.entries[&tokens[1]].appearance.paint, solid("#445566"));
        // OQ-1: one width, the active state's, for every border.
        assert!(model.entries.values().all(|e| (e.appearance.width - 6.0).abs() < f64::EPSILON));

        // The layout override colors only the focused window.
        state.workspaces[0].layout = LayoutType::Monocle;
        let model = border_model(&state, &config);
        assert_eq!(model.entries[&tokens[0]].appearance.paint, solid("#778899"));
        assert_eq!(model.entries[&tokens[1]].appearance.paint, solid("#445566"));
        assert!(model.entries.values().all(|e| (e.appearance.width - 7.0).abs() < f64::EPSILON));

        // The floating override (window floats) colors only the focused window.
        state.workspaces[0].layout = LayoutType::Dwindle;
        state.windows.get_mut(&tokens[1]).expect("window").desired.floating = true;
        state.focus.window = Some(tokens[1]);
        let model = border_model(&state, &config);
        assert_eq!(model.entries[&tokens[1]].appearance.paint, solid("#aabbcc"));
        assert_eq!(model.entries[&tokens[0]].appearance.paint, solid("#445566"));
        assert!(model.entries.values().all(|e| (e.appearance.width - 2.0).abs() < f64::EPSILON));
    }

    #[test]
    fn a_disabled_state_draws_no_overlay_for_its_windows() {
        let config = config(serde_json::json!({"unfocused": false}));
        let (mut state, _feed, tokens) = fixture(2);
        state.focus.window = Some(tokens[0]);
        assert_eq!(bordered(&state, &config), vec![tokens[0]]);
    }

    #[test]
    fn ping_pong_is_installed_only_on_the_focused_border() {
        let animated = |color: &str| {
            serde_json::json!({
                "width": 4,
                "gradient": {"from": color, "to": "#ffffff", "angle": 45.0},
                "animation": {"duration": 400, "easing": "linear"},
            })
        };
        let config = config(serde_json::json!({
            "focused": animated("#ff0000"),
            "unfocused": animated("#0000ff"),
        }));
        let (mut state, _feed, tokens) = fixture(2);
        state.focus.window = Some(tokens[0]);
        let model = border_model(&state, &config);
        assert!(matches!(
            model.entries[&tokens[0]].appearance.motion,
            Some(BorderMotion::PingPong { .. })
        ));
        assert_eq!(model.entries[&tokens[1]].appearance.motion, None);
    }

    #[test]
    fn a_loop_rotates_every_border_whose_active_state_carries_it() {
        let animated = |color: &str, style: &str| {
            serde_json::json!({
                "width": 4,
                "gradient": {"from": color, "to": "#ffffff", "angle": 45.0},
                "animation": {"style": style, "duration": 4000, "easing": "linear"},
            })
        };
        let looping = config(serde_json::json!({
            "focused": animated("#ff0000", "loop"),
            "unfocused": animated("#0000ff", "loop"),
        }));
        let (mut state, _feed, tokens) = fixture(3);
        state.focus.window = Some(tokens[0]);
        let model = border_model(&state, &looping);
        for token in &tokens {
            assert!(
                matches!(
                    model.entries[token].appearance.motion,
                    Some(BorderMotion::Rotate { repeat: MotionRepeat::Loop, .. })
                ),
                "{token:?}"
            );
        }
        // A static unfocused state does not rotate.
        let once = config(serde_json::json!({
            "focused": animated("#ff0000", "once"),
            "unfocused": {"width": 4, "color": "#0000ff"},
        }));
        let model = border_model(&state, &once);
        assert!(matches!(
            model.entries[&tokens[0]].appearance.motion,
            Some(BorderMotion::Rotate { repeat: MotionRepeat::Once, .. })
        ));
        assert_eq!(model.entries[&tokens[1]].appearance.motion, None);
    }

    #[test]
    fn reappearance_recomputes_in_place_and_drops_disabled_states() {
        let (mut state, _feed, tokens) = fixture(2);
        state.focus.window = Some(tokens[0]);
        let mut model = border_model(&state, &BordersConfig::default());
        model.reappearance(&config(serde_json::json!({
            "focused": {"width": 8, "color": "#010203"},
            "unfocused": false,
        })));
        assert_eq!(model.entries.len(), 1);
        assert_eq!(model.entries[&tokens[0]].appearance.paint, solid("#010203"));
    }

    #[test]
    fn writes_and_hidden_applications_are_reported_for_the_show_rule() {
        let (mut state, _feed, tokens) = fixture(2);
        state.frames.plan_sequence = 4;
        state.frames.revealed.insert(tokens[0]);
        state.owned_hidden.insert(app(EDITOR));
        let model = border_model(&state, &BordersConfig::default());
        assert_eq!(model.entries[&tokens[0]].write_pending, Some(5));
        assert!(model.entries[&tokens[0]].app_hidden);
        assert_eq!(model.entries[&tokens[0]].plan_sequence, 4);
    }

    #[test]
    fn external_hide_removes_the_border_until_the_application_returns() {
        let (state, mut feed, _tokens) = fixture(1);
        let (state, _) = step(
            state,
            feed.input(WindowEventKind::Application {
                application: app(EDITOR),
                change: stache_macos::windows::ApplicationChange::Hidden,
            }),
        );
        assert!(bordered(&state, &BordersConfig::default()).is_empty());
        let (state, _) = step(state, crate::state::Input::Lifecycle {
            kind: stache_macos::applications::LifecycleKind::Unhidden,
            info: info(EDITOR, "com.editor", "Editor"),
        });
        assert_eq!(bordered(&state, &BordersConfig::default()).len(), 1);
    }
}
