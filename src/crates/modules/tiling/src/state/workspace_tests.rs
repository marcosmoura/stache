//! Virtual-workspace tests: per-display workspaces, assignment, intents,
//! the visibility requirement and structural reload. Pure transitions only.

use std::sync::Arc;

use stache_macos::applications::LifecycleKind;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{
    IssuedKind, NativeStatus, SnapshotOrigin, SnapshotOutcome, WindowActionOutcome, WindowChange,
    WindowEventKind, WindowInstance,
};
use stache_macos::{DisplayId, DisplayLayout};

use super::*;
use crate::config::{LayoutType, TilingConfig};
use crate::geometry::Rect;
use crate::intent::{
    Cycle, FocusDirection, FocusTarget, Intent, IntentNoOp, IntentOutcome, IntentRejection,
    ResizeDimension, ScreenTarget, SwapDirection,
};
use crate::presets::{PresetNoOp, center_frame};
use crate::test_support::{Feed, app, display, info, ready, snapshot, step};
use crate::workspaces::{
    ConflictKind, ReloadError, prepare_reload, prepare_reload_from_snapshot, visibility_requirement,
};

fn two_displays() -> DisplayLayout {
    DisplayLayout::new(vec![
        display(1, true, Some(60.0), 0.0),
        display(2, false, Some(60.0), 1920.0),
    ])
}

fn cfg(value: serde_json::Value) -> Arc<TilingConfig> {
    Arc::new(serde_json::from_value(value).expect("config"))
}

/// A started, enumerated state with `config` and (optionally) two displays.
struct World {
    state: TilingState,
    feed: Feed,
    revision: u64,
}

impl World {
    fn new(config: Option<serde_json::Value>, two: bool) -> Self {
        let mut feed = Feed::new(1);
        let apps = [
            info(1, "com.editor", "Editor"),
            info(2, "com.browser", "Browser"),
            info(3, "com.term", "Terminal"),
        ];
        let state = ready("com.stache.app", &apps, &mut feed);
        let mut world = Self { state, feed, revision: 0 };
        if two {
            world.send(Input::Displays(Arc::new(two_displays())));
        }
        if let Some(value) = config {
            world.reload(value);
        }
        world
    }

    fn send(&mut self, input: Input) -> Vec<Effect> {
        let state = std::mem::replace(&mut self.state, TilingState::new(""));
        let (next, effects) = step(state, input);
        self.state = next;
        effects
    }

    fn reload(&mut self, value: serde_json::Value) -> Vec<Effect> {
        self.revision += 1;
        self.send(Input::Config {
            revision: self.revision,
            config: cfg(value),
        })
    }

    fn enable(&mut self) -> Vec<Effect> { self.send(Input::FrameWrites(true)) }

    /// Adds a window of application `n` with a frame at `x` (display 2 starts at 1920).
    fn add(&mut self, n: u64, serial: u64, title: &str, x: f64) -> WindowInstance {
        self.add_with(n, serial, title, x, SnapshotOrigin::Created)
    }

    fn add_with(
        &mut self,
        n: u64,
        serial: u64,
        title: &str,
        x: f64,
        origin: SnapshotOrigin,
    ) -> WindowInstance {
        let token = self.feed.token(n, serial);
        let mut snap = snapshot(token, title, u32::try_from(n * 100 + serial).unwrap_or(1));
        snap.frame = frame_read(x + 10.0, 10.0, 800.0, 600.0);
        let input = self.feed.input(WindowEventKind::Snapshot { snapshot: snap, origin });
        self.send(input);
        token
    }

    fn focus(&mut self, token: WindowInstance) {
        let input = self.feed.change(token, WindowChange::Focused);
        self.send(input);
    }

    fn intent(&mut self, intent: Intent) -> Vec<Effect> { self.send(Input::Intent(intent)) }

    fn outcome(&self) -> IntentOutcome {
        self.state.last_intent().expect("an intent ran").outcome.clone()
    }

    fn ws(&self, name: &str) -> &WorkspaceState {
        self.state
            .workspaces
            .iter()
            .find(|w| w.name == name)
            .unwrap_or_else(|| panic!("workspace {name}"))
    }

    fn order(&self, name: &str) -> Vec<WindowInstance> { self.ws(name).order.clone() }

    fn requirement(&self) -> crate::workspaces::VisibilityRequirement {
        visibility_requirement(&self.state)
    }

    /// The current usable area of display `id`.
    fn area(&mut self, id: u32) -> Rect {
        frames::display_area(&mut self.state, DisplayId::new(id)).expect("area").0
    }

    /// A snapshot completion that changes `token`'s title.
    fn retitle(&mut self, token: WindowInstance, title: &str) {
        let stamp = self.feed.stamp;
        let mut snap = snapshot(token, title, 1);
        snap.frame = frame_read(10.0, 10.0, 800.0, 600.0);
        self.send(Input::Completion {
            stamp,
            result: CompletionResult::Snapshot(token, SnapshotOutcome::Snapshot(Box::new(snap))),
        });
    }

    fn ack(&mut self, window: WindowInstance, plan: u64, target: Rect) -> Vec<Effect> {
        let stamp = self.feed.stamp;
        let revision = self.state.config_revision;
        self.send(Input::Completion {
            stamp,
            result: CompletionResult::Frame {
                window,
                plan_sequence: plan,
                revision,
                outcome: WindowActionOutcome::Issued {
                    kind: IssuedKind::Applied,
                    native: NativeStatus::Frame {
                        position: WriteStatus::Written,
                        size: WriteStatus::Written,
                    },
                    observed: Some(frame_read(target.x, target.y, target.width, target.height)),
                },
            },
        })
    }
}

const fn frame_read(x: f64, y: f64, w: f64, h: f64) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x, y }),
        size: AxReading::Value(AxSize { width: w, height: h }),
    }
}

fn writes(effects: &[Effect]) -> Vec<(WindowInstance, u64, Rect)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::WriteFrame {
                window, plan_sequence, target, ..
            } => Some((*window, *plan_sequence, *target)),
            _ => None,
        })
        .collect()
}

fn focuses(effects: &[Effect]) -> Vec<WindowInstance> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Focus { window, .. } => Some(*window),
            _ => None,
        })
        .collect()
}

fn d(id: u32) -> DisplayId { DisplayId::new(id) }

// ================================================================ TI-06-01

#[test]
fn empty_config_gives_one_default_workspace_per_display() {
    for config in [None, Some(serde_json::json!({}))] {
        let world = World::new(config, true);
        let names: Vec<_> = world.state.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["workspace-1", "workspace-2"]);
        assert_eq!(world.ws("workspace-1").screen, d(1));
        assert_eq!(world.ws("workspace-2").screen, d(2));
        assert!(world.state.workspaces.iter().all(|w| w.visible));
        assert_eq!(world.state.workspaces.iter().filter(|w| w.focused).count(), 1);
        assert!(
            world.ws("workspace-1").focused,
            "the main display holds the focus"
        );
    }
}

#[test]
fn configured_assignments_match_main_primary_secondary_and_display_name() {
    let world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "a", "screen": "main"},
            {"name": "b", "screen": "primary"},
            {"name": "c", "screen": "secondary"},
            {"name": "d", "screen": "Display 2"},
            {"name": "e", "screen": "no such display"},
        ]})),
        true,
    );
    assert_eq!(world.ws("a").screen, d(1));
    assert_eq!(world.ws("b").screen, d(1));
    assert_eq!(world.ws("c").screen, d(2));
    assert_eq!(world.ws("d").screen, d(2));
    // An unknown display falls back to main and is remembered as displaced.
    assert_eq!(world.ws("e").screen, d(1));
    assert!(world.ws("e").displaced && !world.ws("a").displaced);
    // One visible workspace per display, one focused workspace overall.
    for display in [d(1), d(2)] {
        let on: Vec<_> = world.state.workspaces.iter().filter(|w| w.screen == display).collect();
        assert_eq!(on.iter().filter(|w| w.visible).count(), 1);
    }
    assert_eq!(world.state.workspaces.iter().filter(|w| w.focused).count(), 1);
}

#[test]
fn a_missing_display_falls_back_to_main_and_the_workspace_returns() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "m", "screen": "main"},
            {"name": "s", "screen": "secondary"},
        ]})),
        true,
    );
    let on_secondary = world.add(1, 1, "w", 1920.0);
    assert_eq!(world.order("s"), vec![on_secondary]);

    world.send(Input::Displays(Arc::new(crate::test_support::one_display())));
    assert_eq!(world.ws("s").screen, d(1));
    assert!(world.ws("s").displaced);
    assert_eq!(world.order("s"), vec![on_secondary], "membership is stable");
    for ws in &world.state.workspaces {
        assert_eq!(
            world
                .state
                .workspaces
                .iter()
                .filter(|o| o.screen == ws.screen && o.visible)
                .count(),
            1
        );
    }

    world.send(Input::Displays(Arc::new(two_displays())));
    assert_eq!(world.ws("s").screen, d(2), "returns when the display returns");
    assert!(!world.ws("s").displaced);
    assert_eq!(world.order("s"), vec![on_secondary]);
}

#[test]
fn a_user_relocated_workspace_is_not_pulled_back_by_a_later_display_event() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "m", "screen": "main"},
            {"name": "s", "screen": "secondary"},
        ]})),
        true,
    );
    world.intent(Intent::FocusWorkspace("s".into()));
    world.intent(Intent::SendWorkspaceToScreen(ScreenTarget::Main));
    assert_eq!(world.outcome(), IntentOutcome::Applied);
    assert_eq!(world.ws("s").screen, d(1));
    assert!(world.ws("s").visible && world.ws("s").focused);
    assert!(
        !world.ws("m").visible,
        "the target display shows exactly one workspace"
    );
    // The vacated display keeps a visible workspace (a generated default).
    assert_eq!(
        world.state.workspaces.iter().filter(|w| w.screen == d(2) && w.visible).count(),
        1
    );
    world.send(Input::Displays(Arc::new(two_displays())));
    assert_eq!(world.ws("s").screen, d(1));
}

#[test]
fn no_displays_means_no_frame_effects_and_an_explicit_incomplete_topology() {
    // Never any display.
    let mut feed = Feed::new(1);
    let state = TilingState::new("");
    let (state, _) = step(state, Input::Start(feed.stamp));
    let (state, _) = step(state, Input::Applications {
        stamp: feed.stamp,
        apps: vec![info(1, "com.editor", "Editor")],
    });
    let (state, effects) = step(state, Input::FrameWrites(true));
    assert!(writes(&effects).is_empty() && !state.snapshot().no_screens);
    assert_ne!(state.phase(), Phase::Complete);
    let token = feed.token(1, 1);
    let (state, _) = step(state, feed.created(token, "w", 1));
    assert_eq!(
        state.windows[&token].desired.workspace, None,
        "no guessed display"
    );

    // Displays that go away: workspaces stay, nothing is written, never Complete.
    let mut world = World::new(None, false);
    world.add(1, 1, "w", 0.0);
    world.enable();
    world.send(Input::Displays(Arc::new(DisplayLayout::new(Vec::new()))));
    let snapshot = world.state.snapshot();
    assert!(snapshot.no_screens);
    assert_eq!(snapshot.phase, Phase::Incomplete);
    assert!(!snapshot.actions_allowed);
    let effects = world.send(Input::Relayout);
    assert!(writes(&effects).is_empty());
}

#[test]
fn focus_history_remembers_workspaces_and_each_workspaces_window() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}, {"name": "c"}]})),
        false,
    );
    world.enable();
    let w1 = world.add(1, 1, "one", 0.0);
    world.focus(w1);
    let a = world.ws("a").id;
    let effects = world.intent(Intent::FocusWorkspace("b".into()));
    assert_eq!(world.state.snapshot().workspace_history.last(), Some(&a));
    assert!(world.ws("b").visible && world.ws("b").focused && !world.ws("a").visible);
    assert!(
        focuses(&effects).is_empty(),
        "an empty workspace focuses nothing"
    );
    let w2 = world.add(2, 1, "two", 0.0);
    assert_eq!(
        world.order("b"),
        vec![w2],
        "new windows join the focused workspace"
    );
    world.focus(w2);
    let effects = world.intent(Intent::FocusWorkspace("a".into()));
    assert_eq!(
        world.state.focus.window,
        Some(w1),
        "a workspace remembers its focused window"
    );
    assert_eq!(focuses(&effects), vec![w1]);
    assert_eq!(
        world.state.snapshot().workspace_history.last(),
        Some(&world.ws("b").id)
    );
    // Switching to the current workspace changes nothing.
    world.intent(Intent::FocusWorkspace("a".into()));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    world.intent(Intent::FocusWorkspace("missing".into()));
    assert_eq!(
        world.outcome(),
        IntentOutcome::Rejected(IntentRejection::UnknownWorkspace)
    );
    world.intent(Intent::CycleWorkspace(Cycle::Previous));
    assert!(world.ws("c").focused, "cycling wraps backwards");
    world.intent(Intent::CycleWorkspace(Cycle::Next));
    assert!(world.ws("a").focused);
}

#[test]
fn focus_without_authority_is_recorded_but_emits_no_effect() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}]})),
        false,
    );
    let w = world.add(1, 1, "w", 0.0);
    world.intent(Intent::FocusWorkspace("b".into()));
    let effects = world.intent(Intent::FocusWorkspace("a".into()));
    assert_eq!(world.state.focus.window, Some(w));
    assert!(focuses(&effects).is_empty());
    assert!(world.state.diagnostics.focus_unauthorized >= 1);
}

// ------------------------------------------------------------- assignment

#[test]
fn assignment_prefers_rules_then_the_windows_display_then_that_displays_default() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "main-a", "screen": "main"},
            {"name": "main-b", "screen": "main"},
            {"name": "side-a", "screen": "secondary"},
            {"name": "docs", "screen": "main", "rules": [{"title": "docs"}]},
        ]})),
        true,
    );
    // The focused workspace is on the main display.
    let on_side = world.add(2, 1, "plain", 1920.0);
    assert_eq!(
        world.order("side-a"),
        vec![on_side],
        "the window's own display wins"
    );
    let on_main = world.add(2, 2, "plain", 0.0);
    assert_eq!(world.order("main-a"), vec![on_main]);
    let rule = world.add(2, 3, "My docs", 1920.0);
    assert_eq!(world.order("docs"), vec![rule], "a rule outranks the display");
    // The visible workspace of the display, not the first one, when it is not focused.
    world.intent(Intent::FocusWorkspace("main-b".into()));
    let again = world.add(2, 4, "plain", 0.0);
    assert_eq!(world.order("main-b"), vec![again]);
}

#[test]
fn a_title_change_updates_metadata_and_never_moves_a_user_managed_window() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "home"}, {"name": "elsewhere"},
            {"name": "docs", "rules": [{"title": "docs"}]},
        ]})),
        false,
    );
    let w = world.add(1, 1, "plain", 0.0);
    assert_eq!(world.order("home"), vec![w]);
    world.focus(w);
    world.intent(Intent::SendWindowToWorkspace {
        window: None,
        workspace: "elsewhere".into(),
    });
    assert_eq!(world.order("elsewhere"), vec![w]);
    for title in ["docs", "more docs", "docs"] {
        world.retitle(w, title);
    }
    assert_eq!(world.state.windows[&w].observed.title, "docs");
    assert_eq!(
        world.order("elsewhere"),
        vec![w],
        "rules are creation/adoption assignment"
    );
    assert!(world.order("docs").is_empty());
}

#[test]
fn non_grid_inserts_after_the_focused_window_in_the_assigned_workspace() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "main"},
            {"name": "docs", "rules": [{"title": "docs"}]},
        ]})),
        false,
    );
    let (a, b) = (world.add(1, 1, "a", 0.0), world.add(1, 2, "b", 0.0));
    world.focus(a);
    let c = world.add(1, 3, "c", 0.0);
    assert_eq!(world.order("main"), vec![a, c, b]);
    // Another workspace's focus is no anchor: append there.
    let d1 = world.add(1, 4, "docs one", 0.0);
    let d2 = world.add(1, 5, "docs two", 0.0);
    assert_eq!(world.order("docs"), vec![d1, d2]);
}

fn grid_world(count: u64) -> (World, Vec<WindowInstance>) {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "g", "layout": "grid"}]})),
        false,
    );
    let tokens: Vec<_> = (1..=count).map(|n| world.add(1, n, "w", 0.0)).collect();
    world.enable();
    (world, tokens)
}

#[test]
fn grid_appends_overflow_promotes_in_order_and_focus_never_displaces_a_tile() {
    let (mut world, w) = grid_world(11);
    assert_eq!(world.order("g"), w, "Grid appends the eleventh window");
    for tile in &w[..10] {
        assert!(world.state.frames.desired.contains_key(tile));
    }
    assert!(!world.state.frames.desired.contains_key(&w[10]));
    // Focusing the overflow window displaces nobody.
    world.focus(w[10]);
    assert!(!world.state.frames.desired.contains_key(&w[10]));
    assert!(world.state.frames.desired.contains_key(&w[0]));
    // A tile leaving promotes the earliest extra.
    let destroyed = world.feed.change(w[3], WindowChange::Destroyed);
    world.send(destroyed);
    assert!(world.state.frames.desired.contains_key(&w[10]), "promoted");
    assert_eq!(world.state.frames.desired.len(), 10);
}

#[test]
fn an_explicit_swap_may_promote_in_a_grid_but_focus_cannot() {
    let (mut world, w) = grid_world(11);
    world.focus(w[9]);
    world.intent(Intent::SwapWindow(SwapDirection::Next));
    assert_eq!(world.outcome(), IntentOutcome::Applied);
    assert_eq!(world.order("g")[9], w[10]);
    assert_eq!(world.order("g")[10], w[9]);
    assert!(world.state.frames.desired.contains_key(&w[10]));
    assert!(!world.state.frames.desired.contains_key(&w[9]));
    assert_eq!(
        world.state.focus.window,
        Some(w[9]),
        "focus stays on the swapped window"
    );
}

#[test]
fn grid_overflow_never_gets_automatic_centering_or_a_preset() {
    let mut world = World::new(
        Some(serde_json::json!({
            "workspaces": [{"name": "g", "layout": "grid"}],
            "floating": {"presets": [{"name": "half", "width": "50%", "height": "50%"}]}
        })),
        false,
    );
    let tokens: Vec<_> = (1..=11).map(|n| world.add(1, n, "w", 0.0)).collect();
    let effects = world.enable();
    let overflow = tokens[10];
    assert!(writes(&effects).iter().all(|(token, _, _)| *token != overflow));
    assert!(
        world.state.frames.pinned.is_empty(),
        "no automatic centering for overflow"
    );
    world.focus(overflow);
    world.intent(Intent::Preset("half".into()));
    assert_eq!(
        world.outcome(),
        IntentOutcome::NoOp(IntentNoOp::Preset(PresetNoOp::NotFloatingLayout))
    );
    assert!(world.state.frames.pinned.is_empty());
    assert!(
        !world.state.windows[&overflow].desired.floating,
        "overflow is not user floating"
    );
}

// ------------------------------------------------------------ focus / swap

#[test]
fn the_intent_grammar_is_case_insensitive_for_focus_and_exact_for_swap() {
    assert_eq!(FocusDirection::parse("PREV"), Some(FocusDirection::Previous));
    assert_eq!(FocusDirection::parse("Previous"), Some(FocusDirection::Previous));
    assert_eq!(FocusDirection::parse("Up"), Some(FocusDirection::Up));
    assert_eq!(FocusDirection::parse("sideways"), None);
    for token in ["up", "down", "left", "right", "next", "previous"] {
        assert!(SwapDirection::parse(token).is_some());
    }
    for token in ["prev", "Up", "NEXT", "", "previous "] {
        assert_eq!(SwapDirection::parse(token), None, "{token}");
    }
    assert_eq!(ScreenTarget::parse("Primary"), ScreenTarget::Main);
    assert_eq!(ScreenTarget::parse("secondary"), ScreenTarget::Secondary);
}

fn row(world: &mut World) -> [WindowInstance; 3] {
    let a = world.add(1, 1, "a", 0.0);
    let b = world.add(1, 2, "b", 900.0);
    let c = world.add(1, 3, "c", 1000.0);
    // Insert-after-focus would reorder: focus the last so each appends.
    world.focus(a);
    world.focus(b);
    world.focus(c);
    [a, b, c]
}

#[test]
fn focus_by_direction_cycle_and_exact_token() {
    let mut world = World::new(None, false);
    world.enable();
    let [a, b, c] = row(&mut world);
    assert_eq!(world.order("workspace-1"), vec![a, b, c]);
    world.focus(a);
    let effects = world.intent(Intent::FocusWindow(FocusTarget::Direction(
        FocusDirection::Right,
    )));
    assert_eq!(world.state.focus.window, Some(b));
    assert_eq!(focuses(&effects), vec![b]);
    world.intent(Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Left)));
    assert_eq!(world.state.focus.window, Some(a));
    world.intent(Intent::FocusWindow(FocusTarget::Direction(
        FocusDirection::Previous,
    )));
    assert_eq!(world.state.focus.window, Some(c), "previous wraps");
    world.intent(Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Next)));
    assert_eq!(world.state.focus.window, Some(a), "next wraps");
    world.intent(Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Up)));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::NoCandidate));
    let effects = world.intent(Intent::FocusWindow(FocusTarget::Window(c)));
    assert_eq!((world.state.focus.window, focuses(&effects)), (Some(c), vec![c]));
    world.intent(Intent::FocusWindow(FocusTarget::Window(c)));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    let stranger = world.feed.token(9, 9);
    world.intent(Intent::FocusWindow(FocusTarget::Window(stranger)));
    assert_eq!(
        world.outcome(),
        IntentOutcome::Rejected(IntentRejection::UnknownWindow)
    );
}

#[test]
fn focusing_a_window_on_a_hidden_workspace_shows_that_workspace() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}]})),
        false,
    );
    let w1 = world.add(1, 1, "one", 0.0);
    world.intent(Intent::FocusWorkspace("b".into()));
    let w2 = world.add(2, 1, "two", 0.0);
    world.intent(Intent::FocusWorkspace("a".into()));
    assert!(!world.ws("b").visible);
    world.intent(Intent::FocusWindow(FocusTarget::Window(w2)));
    assert!(world.ws("b").visible && world.ws("b").focused && !world.ws("a").visible);
    assert_eq!(world.state.focus.window, Some(w2));
    let _ = w1;
}

#[test]
fn swap_exchanges_order_in_all_six_directions_and_keeps_focus() {
    let mut world = World::new(None, false);
    let [a, b, c] = row(&mut world);
    world.focus(a);
    world.intent(Intent::SwapWindow(SwapDirection::Next));
    assert_eq!(world.order("workspace-1"), vec![b, a, c]);
    assert_eq!(world.state.focus.window, Some(a));
    world.intent(Intent::SwapWindow(SwapDirection::Previous));
    assert_eq!(world.order("workspace-1"), vec![a, b, c]);
    world.intent(Intent::SwapWindow(SwapDirection::Right));
    assert_eq!(world.order("workspace-1"), vec![b, a, c]);
    // Spatial swaps follow the observed frames: `b` sits to the right of `a`.
    world.focus(b);
    world.intent(Intent::SwapWindow(SwapDirection::Left));
    assert_eq!(world.order("workspace-1"), vec![a, b, c]);
    world.intent(Intent::SwapWindow(SwapDirection::Up));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::NoCandidate));
    world.intent(Intent::SwapWindow(SwapDirection::Down));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::NoCandidate));
}

// -------------------------------------------------------- floating / preset

fn presets() -> serde_json::Value {
    serde_json::json!([{"name": "Half", "width": "50%", "height": "50%", "center": true}])
}

#[test]
fn center_places_a_newly_floating_window_once_on_its_own_display() {
    let mut world = World::new(
        Some(
            serde_json::json!({"workspaces": [{"name": "f", "layout": "floating", "screen": "main"}]}),
        ),
        false,
    );
    world.enable();
    let usable = world.area(1);
    let observed = Rect::new(10.0, 10.0, 800.0, 600.0);
    let want = center_frame(&observed, &usable).expect("center");
    let before = world.feed.token(1, 1);
    let effects = world.add_effects(before);
    let [(window, plan, target)] = writes(&effects)[..] else {
        panic!("one centering write, got {:?}", writes(&effects));
    };
    assert_eq!((window, target), (before, want));
    // The window's acknowledgement spends the pin; nothing re-centers later.
    world.ack(window, plan, want);
    for effect_run in 0..3 {
        let input = world.feed.change(window, WindowChange::Focused);
        assert!(writes(&world.send(input)).is_empty(), "focus {effect_run}");
        world.retitle(window, "renamed");
    }
    assert!(world.state.frames.pinned.is_empty());
    let effects = world.reload(serde_json::json!({
        "workspaces": [{"name": "f", "layout": "floating", "screen": "main"}], "gaps": {"inner": 1}
    }));
    assert!(writes(&effects).is_empty(), "reload never re-centers");
}

impl World {
    /// Adds a created window and returns the effects of its creation turn.
    fn add_effects(&mut self, token: WindowInstance) -> Vec<Effect> {
        let mut snap = snapshot(token, "w", 101);
        snap.frame = frame_read(10.0, 10.0, 800.0, 600.0);
        let input = self.feed.input(WindowEventKind::Snapshot {
            snapshot: snap,
            origin: SnapshotOrigin::Created,
        });
        self.send(input)
    }
}

#[test]
fn default_position_keeps_the_application_frame_and_adoption_never_centers() {
    let mut world = World::new(
        Some(serde_json::json!({
            "workspaces": [{"name": "f", "layout": "floating"}],
            "floating": {"defaultPosition": "default"}
        })),
        false,
    );
    world.enable();
    world.add(1, 1, "w", 0.0);
    assert!(world.state.frames.pinned.is_empty());

    let mut centered = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "f", "layout": "floating"}]})),
        false,
    );
    centered.enable();
    // An enumerated (pre-existing) window is adopted, not new.
    centered.add_with(1, 1, "w", 0.0, SnapshotOrigin::Enumeration);
    assert!(centered.state.frames.pinned.is_empty());
}

#[test]
fn toggling_floating_centers_once_per_toggle_and_tiled_windows_are_untouched() {
    let mut world = World::new(Some(serde_json::json!({})), false);
    world.enable();
    let w = world.add(1, 1, "w", 0.0);
    world.focus(w);
    assert!(
        world.state.frames.pinned.is_empty(),
        "tiled windows are not centered"
    );
    let usable = world.area(1);
    let want = center_frame(&Rect::new(10.0, 10.0, 800.0, 600.0), &usable).expect("center");
    let effects = world.intent(Intent::ToggleFloating);
    assert!(world.state.windows[&w].desired.floating);
    assert_eq!(writes(&effects).iter().map(|x| x.2).collect::<Vec<_>>(), vec![
        want
    ]);
    // Floating windows are never re-planned by layout.
    let (_, plan, target) = writes(&effects)[0];
    world.ack(w, plan, target);
    let effects = world.intent(Intent::Balance);
    assert!(writes(&effects).iter().all(|x| x.0 != w));
    // Un-floating and floating again is a new explicit event.
    world.intent(Intent::ToggleFloating);
    assert!(!world.state.windows[&w].desired.floating);
    assert!(!world.state.windows[&w].desired.positioned);
}

fn preset_world(layout: &str) -> World {
    World::new(
        Some(serde_json::json!({
            "workspaces": [{"name": "w", "layout": layout}],
            "floating": {"presets": presets(), "defaultPosition": "center"}
        })),
        false,
    )
}

#[test]
fn an_explicit_preset_in_a_floating_workspace_outranks_default_centering() {
    let mut world = preset_world("floating");
    world.enable();
    let w = world.feed.token(1, 1);
    let effects = world.add_effects(w);
    let center = writes(&effects)[0].2;
    world.focus(w);
    let effects = world.intent(Intent::Preset("half".into()));
    assert_eq!(
        world.outcome(),
        IntentOutcome::Applied,
        "preset names are case-insensitive"
    );
    let target = writes(&effects).last().expect("preset write").2;
    assert_ne!(target, center);
    let usable = world.area(1);
    assert!((target.width - usable.width / 2.0).abs() < 1.0);
    assert!((target.height - usable.height / 2.0).abs() < 1.0);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CancelFrame { window } if *window == w))
    );
    assert!(world.state.windows[&w].desired.positioned);
}

#[test]
fn a_preset_outside_a_floating_workspace_is_a_silent_admitted_no_op() {
    let mut world = preset_world("dwindle");
    world.enable();
    let w = world.add(1, 1, "w", 0.0);
    world.focus(w);
    let writes_before = world.state.diagnostics.frame_attempts;
    for floating_first in [false, true] {
        if floating_first {
            // An individually floating window in a tiled workspace is still a no-op.
            world.intent(Intent::ToggleFloating);
            assert!(world.state.windows[&w].desired.floating);
        }
        let effects = world.intent(Intent::Preset("half".into()));
        assert_eq!(
            world.outcome(),
            IntentOutcome::NoOp(IntentNoOp::Preset(PresetNoOp::NotFloatingLayout))
        );
        assert!(writes(&effects).is_empty(), "no Applied frame is invented");
        assert!(effects.iter().all(|e| !matches!(e, Effect::CancelFrame { .. })));
    }
    // The toggle's own centering is the only attempt made.
    assert!(world.state.diagnostics.frame_attempts <= writes_before + 1);
    assert_eq!(world.state.diagnostics.frames_applied, 0);
    world.intent(Intent::Preset("unknown".into()));
    assert_eq!(
        world.outcome(),
        IntentOutcome::NoOp(IntentNoOp::Preset(PresetNoOp::UnknownPreset))
    );
}

#[test]
fn preset_on_open_is_inert() {
    let mut world = World::new(
        Some(serde_json::json!({
            "workspaces": [{"name": "w", "layout": "floating", "preset-on-open": "half"}],
            "floating": {"presets": presets(), "defaultPosition": "default"}
        })),
        false,
    );
    world.enable();
    let effects = world.add(1, 1, "w", 0.0);
    let _ = effects;
    assert!(world.state.frames.pinned.is_empty(), "no automatic preset");
    assert_eq!(world.state.diagnostics.frame_attempts, 0);
}

// ----------------------------------------------------------- layout commands

#[test]
fn layout_commands_manage_ratios_and_balance() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "w", "layout": "dwindle"}]})),
        false,
    );
    let a = world.add(1, 1, "a", 0.0);
    let _b = world.add(1, 2, "b", 0.0);
    world.focus(a);
    world.intent(Intent::Resize {
        dimension: ResizeDimension::Width,
        amount: 100,
    });
    assert_eq!(world.outcome(), IntentOutcome::Applied);
    let ratios = world.ws("w").ratios.split_ratios.clone();
    assert_eq!(ratios.len(), 1);
    assert!(ratios[0] > 0.5);
    world.intent(Intent::Resize {
        dimension: ResizeDimension::Height,
        amount: 100,
    });
    assert_eq!(
        world.outcome(),
        IntentOutcome::NoOp(IntentNoOp::LayoutIgnoresCommand)
    );
    world.intent(Intent::Resize {
        dimension: ResizeDimension::Width,
        amount: 0,
    });
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    // Re-selecting the layout keeps ratios; switching clears them.
    world.intent(Intent::SetLayout(LayoutType::Dwindle));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    assert_eq!(world.ws("w").ratios.split_ratios, ratios);
    world.intent(Intent::Balance);
    assert_eq!(world.outcome(), IntentOutcome::Applied);
    assert!(world.ws("w").ratios.split_ratios.is_empty());
    world.intent(Intent::Balance);
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    world.intent(Intent::SetLayout(LayoutType::Master));
    assert!(world.ws("w").layout_overridden);
    world.intent(Intent::Resize {
        dimension: ResizeDimension::Width,
        amount: 50,
    });
    assert!(world.ws("w").ratios.master_ratio.is_some_and(|r| r > 0.6));
    world.intent(Intent::SetLayout(LayoutType::Dwindle));
    assert!(!world.ws("w").layout_overridden, "back at the configured layout");
    assert!(world.ws("w").ratios.master_ratio.is_none());
    world.intent(Intent::CycleLayout);
    assert_eq!(world.ws("w").layout, LayoutType::Monocle);
}

#[test]
fn resize_drives_the_planned_ratios_of_the_visible_layout() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "w", "layout": "dwindle"}]})),
        false,
    );
    let a = world.add(1, 1, "a", 0.0);
    let b = world.add(1, 2, "b", 0.0);
    world.enable();
    let before = world.state.frames.desired[&a].width;
    world.focus(a);
    world.intent(Intent::Resize {
        dimension: ResizeDimension::Width,
        amount: 200,
    });
    let after = world.state.frames.desired[&a].width;
    assert!(after > before, "{before} -> {after}");
    assert!(world.state.frames.desired[&b].width < before);
}

// ----------------------------------------------------------------- send

#[test]
fn send_window_to_workspace_and_screen_keeps_exact_tokens_and_focus_in_place() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "a", "screen": "main"}, {"name": "b", "screen": "main"},
            {"name": "c", "screen": "secondary"},
        ]})),
        true,
    );
    world.enable();
    let (w1, w2) = (world.add(1, 1, "one", 0.0), world.add(1, 2, "two", 0.0));
    world.focus(w1);
    let effects = world.intent(Intent::SendWindowToWorkspace {
        window: None,
        workspace: "b".into(),
    });
    assert_eq!(world.order("b"), vec![w1]);
    assert_eq!(
        world.state.windows[&w1].desired.workspace,
        Some(world.ws("b").id)
    );
    assert_eq!(
        world.state.focus.window,
        Some(w2),
        "focus stays in the workspace"
    );
    assert_eq!(focuses(&effects), vec![w2]);
    // An exact token other than the focused window.
    world.intent(Intent::SendWindowToWorkspace {
        window: Some(w2),
        workspace: "b".into(),
    });
    assert_eq!(world.order("b"), vec![w1, w2], "appended in order");
    world.intent(Intent::SendWindowToWorkspace {
        window: Some(w2),
        workspace: "b".into(),
    });
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    world.intent(Intent::SendWindowToWorkspace {
        window: Some(w2),
        workspace: "zzz".into(),
    });
    assert_eq!(
        world.outcome(),
        IntentOutcome::Rejected(IntentRejection::UnknownWorkspace)
    );

    world.focus(w2);
    world.intent(Intent::SendWindowToScreen(ScreenTarget::Secondary));
    assert_eq!(world.order("c"), vec![w2]);
    world.intent(Intent::SendWindowToScreen(ScreenTarget::Named(
        "DISPLAY 9".into(),
    )));
    assert_eq!(
        world.outcome(),
        IntentOutcome::Rejected(IntentRejection::UnknownScreen)
    );
}

// ================================================================ TI-06-02

#[test]
fn a_needed_app_is_never_hide_wanted_and_the_conflict_is_published() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}]})),
        false,
    );
    let editor_a = world.add(1, 1, "a", 0.0);
    let editor_b = world.add(1, 2, "b", 0.0);
    let only_b = world.add(2, 1, "browser", 0.0);
    world.intent(Intent::SendWindowToWorkspace {
        window: Some(editor_b),
        workspace: "b".into(),
    });
    world.intent(Intent::SendWindowToWorkspace {
        window: Some(only_b),
        workspace: "b".into(),
    });
    let (a, b) = (world.ws("a").id, world.ws("b").id);
    assert_eq!(world.order("a"), vec![editor_a]);

    let requirement = world.requirement();
    let editor = requirement.app(app(1)).expect("editor");
    assert_eq!(editor.needed_by, vec![a]);
    assert_eq!(editor.inactive_in, vec![b]);
    assert!(!editor.hide_wanted, "needed by a visible workspace");
    assert!(requirement.needed(app(1)) && !requirement.hide_wanted(app(1)));
    assert!(
        requirement.hide_wanted(app(2)),
        "only an inactive workspace wants the browser"
    );
    assert!(requirement.conflicts.iter().any(|c| {
        c.application == app(1)
            && c.kind == ConflictKind::InactiveWindowsStayVisible
            && c.affected == vec![b]
    }));
    // Logical assignments are kept even though the window stays on screen.
    assert_eq!(world.state.windows[&editor_b].desired.workspace, Some(b));

    // Repeated refresh and unrelated transitions never change the requirement.
    for _ in 0..4 {
        let again = world.feed.change(editor_a, WindowChange::Moved);
        world.send(again);
        world.send(Input::Relayout);
        world.retitle(editor_a, "a");
        assert_eq!(world.requirement(), requirement);
    }

    // Switching flips the roles exactly once, and the shared app stays needed.
    world.intent(Intent::FocusWorkspace("b".into()));
    let switched = world.requirement();
    assert!(!switched.hide_wanted(app(1)) && switched.needed(app(1)));
    assert!(!switched.hide_wanted(app(2)));
    assert_eq!(switched.app(app(1)).expect("editor").needed_by, vec![b]);
    assert_eq!(world.state.snapshot().visibility, switched);
}

#[test]
fn the_same_app_on_two_displays_is_needed_by_both_and_reported_as_shared() {
    let mut world = World::new(None, true);
    let left = world.add(1, 1, "l", 0.0);
    let right = world.add(1, 2, "r", 1920.0);
    assert_eq!(
        world.state.windows[&left].desired.workspace,
        Some(world.ws("workspace-1").id)
    );
    assert_eq!(
        world.state.windows[&right].desired.workspace,
        Some(world.ws("workspace-2").id)
    );
    let requirement = world.requirement();
    let editor = requirement.app(app(1)).expect("editor");
    assert_eq!(editor.needed_by.len(), 2);
    assert_eq!(editor.displays, vec![d(1), d(2)]);
    assert!(!editor.hide_wanted);
    assert!(
        requirement
            .conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::SharedAcrossDisplays)
    );
    // Sending the right window to the main display leaves one need and no conflict.
    world.focus(right);
    world.intent(Intent::SendWindowToScreen(ScreenTarget::Main));
    let requirement = world.requirement();
    assert_eq!(requirement.app(app(1)).expect("editor").displays, vec![d(1)]);
    assert!(requirement.conflicts.is_empty());
}

#[test]
fn external_hidden_and_minimized_state_is_respected_and_never_cleared_by_refresh() {
    let mut world = World::new(None, false);
    let w = world.add(1, 1, "w", 0.0);
    let info = crate::test_support::info(1, "com.editor", "Editor");
    world.send(Input::Lifecycle {
        kind: LifecycleKind::Hidden,
        info,
    });
    assert!(world.state.windows[&w].observed.hidden);
    assert!(world.requirement().app(app(1)).expect("editor").externally_hidden);
    let input = world.feed.change(w, WindowChange::Miniaturized);
    world.send(input);
    for _ in 0..3 {
        world.retitle(w, "w");
        world.send(Input::Relayout);
        let again = world.feed.change(w, WindowChange::Moved);
        world.send(again);
    }
    let window = &world.state.windows[&w];
    assert!(window.observed.hidden, "refresh never unhides");
    assert!(world.requirement().app(app(1)).expect("editor").externally_hidden);
    assert!(
        world.requirement().needed(app(1)),
        "a hidden window still needs its workspace"
    );
}

// ================================================================ reload

fn two_apps_two_workspaces() -> serde_json::Value {
    serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}]})
}

#[test]
fn reload_keeps_matching_workspaces_and_adds_with_the_configured_layout() {
    let mut world = World::new(Some(two_apps_two_workspaces()), false);
    let w = world.add(1, 1, "w", 0.0);
    let (a_id, b_id) = (world.ws("a").id, world.ws("b").id);
    world.reload(serde_json::json!({"workspaces": [
        {"name": "a"}, {"name": "b"}, {"name": "c", "layout": "grid"}
    ]}));
    assert_eq!((world.ws("a").id, world.ws("b").id), (a_id, b_id));
    assert_eq!(world.order("a"), vec![w]);
    assert_eq!(world.ws("c").layout, LayoutType::Grid);
    assert!(world.ws("c").id.0 > b_id.0);
    assert_eq!(world.state.workspaces.iter().filter(|x| x.visible).count(), 1);
    assert!(world.ws("a").focused);
}

#[test]
fn reload_moves_removed_workspace_windows_in_stable_order_to_the_visible_workspace() {
    let mut world = World::new(Some(two_apps_two_workspaces()), false);
    let w0 = world.add(1, 1, "w0", 0.0);
    world.intent(Intent::FocusWorkspace("b".into()));
    let (w1, w2) = (world.add(1, 2, "w1", 0.0), world.add(1, 3, "w2", 0.0));
    world.intent(Intent::FocusWorkspace("a".into()));
    assert_eq!(world.order("b"), vec![w1, w2]);
    world.reload(serde_json::json!({"workspaces": [{"name": "a"}]}));
    assert_eq!(world.order("a"), vec![w0, w1, w2], "stable order, appended");
    for token in [w0, w1, w2] {
        assert_eq!(
            world.state.windows[&token].desired.workspace,
            Some(world.ws("a").id)
        );
    }
    assert!(world.state.workspaces.iter().all(|w| w.name != "b"));
    assert!(world.ws("a").visible && world.ws("a").focused);
}

#[test]
fn reload_treats_a_rename_as_remove_plus_add_and_keeps_the_windows() {
    let mut world = World::new(Some(two_apps_two_workspaces()), false);
    let w = world.add(1, 1, "w", 0.0);
    world.reload(serde_json::json!({"workspaces": [{"name": "a2"}, {"name": "b"}]}));
    assert_eq!(world.order("a2"), vec![w]);
    assert!(
        world.ws("a2").visible,
        "the visible/default workspace of the display"
    );
    assert_eq!(world.state.focus.workspace, Some(world.ws("a2").id));
}

#[test]
fn reload_keeps_a_user_layout_until_the_configured_layout_changes() {
    let mut world = World::new(
        Some(serde_json::json!({"defaultLayout": "dwindle", "workspaces": [
            {"name": "a"}, {"name": "b"}, {"name": "m", "layout": "monocle"}
        ]})),
        false,
    );
    let first = world.add(1, 1, "w", 0.0);
    world.add(1, 2, "w", 0.0);
    world.focus(first);
    world.intent(Intent::SetLayout(LayoutType::Master));
    world.intent(Intent::Resize {
        dimension: ResizeDimension::Width,
        amount: 50,
    });
    let master = world.ws("a").ratios.master_ratio;
    assert!(master.is_some());

    // Unchanged layout configuration (other edits): the override and ratios stay.
    world.reload(
        serde_json::json!({"defaultLayout": "dwindle", "gaps": {"inner": 3}, "workspaces": [
            {"name": "a"}, {"name": "b"}, {"name": "m", "layout": "monocle"}
        ]}),
    );
    assert_eq!(world.ws("a").layout, LayoutType::Master);
    assert_eq!(world.ws("a").ratios.master_ratio, master);

    // A default-layout edit reaches followers only, not the user override or explicit layouts.
    world.reload(serde_json::json!({"defaultLayout": "split", "workspaces": [
        {"name": "a"}, {"name": "b"}, {"name": "m", "layout": "monocle"}
    ]}));
    assert_eq!(
        world.ws("a").layout,
        LayoutType::Master,
        "explicit user override"
    );
    assert_eq!(world.ws("b").layout, LayoutType::Split, "default-following");
    assert_eq!(world.ws("m").layout, LayoutType::Monocle);

    // A changed configured layout replaces the layout and clears incompatible ratios.
    world.reload(serde_json::json!({"defaultLayout": "split", "workspaces": [
        {"name": "a", "layout": "grid"}, {"name": "b"}, {"name": "m", "layout": "monocle"}
    ]}));
    assert_eq!(world.ws("a").layout, LayoutType::Grid);
    assert!(!world.ws("a").layout_overridden);
    assert!(
        world.ws("a").ratios.master_ratio.is_none() && world.ws("a").ratios.split_ratios.is_empty()
    );
}

#[test]
fn reload_moves_a_workspace_when_its_screen_assignment_changes() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "m", "screen": "main"}, {"name": "s", "screen": "secondary"}
        ]})),
        true,
    );
    let w = world.add(1, 1, "w", 1920.0);
    assert_eq!(world.ws("s").screen, d(2));
    world.reload(serde_json::json!({"workspaces": [
        {"name": "m", "screen": "main"}, {"name": "s", "screen": "main"}
    ]}));
    assert_eq!(world.ws("s").screen, d(1));
    assert_eq!(world.order("s"), vec![w], "the window goes with its workspace");
    for display in [d(1), d(2)] {
        assert_eq!(
            world
                .state
                .workspaces
                .iter()
                .filter(|x| x.screen == display && x.visible)
                .count(),
            1
        );
    }
    assert_eq!(world.state.workspaces.iter().filter(|x| x.focused).count(), 1);
}

#[test]
fn reload_rule_edits_change_eligibility_but_reassign_no_existing_window() {
    let mut world = World::new(
        Some(serde_json::json!({"workspaces": [
            {"name": "a"}, {"name": "docs", "rules": [{"title": "docs"}]}
        ]})),
        false,
    );
    let first = world.add(1, 1, "docs", 0.0);
    assert_eq!(world.order("docs"), vec![first]);
    world.reload(serde_json::json!({"workspaces": [
        {"name": "a", "rules": [{"title": "docs"}]}, {"name": "docs"}
    ]}));
    assert_eq!(
        world.order("docs"),
        vec![first],
        "no rule edit reassigns an existing window"
    );
    let second = world.add(1, 2, "docs", 0.0);
    assert_eq!(
        world.order("a"),
        vec![second],
        "new windows follow the new rules"
    );
}

#[test]
fn newly_ignored_windows_release_tiling_and_visibility_needs_and_are_read_again_later() {
    let mut world = World::new(None, false);
    world.enable();
    let editor = world.add(1, 1, "w", 0.0);
    let browser = world.add(2, 1, "w", 0.0);
    assert!(world.requirement().app(app(1)).is_some());
    world.reload(serde_json::json!({"ignore": [{"appName": "Editor"}]}));
    assert!(!world.state.windows.contains_key(&editor));
    assert!(world.state.windows.contains_key(&browser));
    assert!(
        world.requirement().app(app(1)).is_none(),
        "no visibility need remains"
    );
    assert!(world.state.excluded.contains_key(&editor));
    assert!(world.state.workspaces.iter().all(|w| !w.order.contains(&editor)));
    assert!(!world.state.frames.desired.contains_key(&editor));

    let effects = world.reload(serde_json::json!({}));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Snapshot { window, .. } if *window == editor)),
        "windows no longer ignored are read again"
    );
}

#[test]
fn a_rejected_reload_candidate_leaves_the_old_state_and_publication() {
    let mut world = World::new(Some(two_apps_two_workspaces()), false);
    let w = world.add(1, 1, "w", 0.0);
    let before = (
        world.state.workspaces.clone(),
        world.state.config_revision,
        world.state.windows.clone(),
        world.state.focus,
    );
    let bad: TilingConfig = serde_json::from_value(
        serde_json::json!({"workspaces": [{"name": "x"}, {"name": "x"}, {"name": "y"}]}),
    )
    .expect("parses; validation is the host's job");
    assert!(matches!(
        prepare_reload(&world.state, &bad),
        Err(ReloadError::DuplicateWorkspace(name)) if name == "x"
    ));
    let revision = world.revision + 1;
    world.send(Input::Config {
        revision,
        config: Arc::new(bad.clone()),
    });
    assert_eq!(
        (
            world.state.workspaces.clone(),
            world.state.config_revision,
            world.state.windows.clone(),
            world.state.focus
        ),
        before
    );
    assert_eq!(world.state.diagnostics.rejected_reloads, 1);
    assert!(world.state.snapshot().last_reload_error.is_some());
    assert_eq!(world.order("a"), vec![w]);
    // The module's staging check refuses the same candidate from the snapshot.
    assert_eq!(
        prepare_reload_from_snapshot(&world.state.snapshot(), &bad),
        Err(ReloadError::DuplicateWorkspace("x".into()))
    );
    let empty_name: TilingConfig =
        serde_json::from_value(serde_json::json!({"workspaces": [{"name": ""}]})).expect("config");
    assert_eq!(
        prepare_reload(&world.state, &empty_name).err(),
        Some(ReloadError::EmptyWorkspaceName)
    );
    // A good candidate is accepted afterwards with the next revision.
    world.reload(serde_json::json!({"workspaces": [{"name": "a"}]}));
    assert_eq!(world.state.diagnostics.rejected_reloads, 1);
    assert_eq!(world.state.config_revision, world.revision);
}

#[test]
fn reload_preparation_is_pure_and_deterministic() {
    let mut world = World::new(Some(two_apps_two_workspaces()), true);
    world.add(1, 1, "w", 0.0);
    let candidate: TilingConfig = serde_json::from_value(
        serde_json::json!({"workspaces": [{"name": "a"}, {"name": "z", "screen": "secondary"}]}),
    )
    .expect("config");
    let snapshot_before = world.state.snapshot();
    let first = prepare_reload(&world.state, &candidate).expect("mapping");
    let second = prepare_reload(&world.state, &candidate).expect("mapping");
    assert_eq!(
        world.state.snapshot(),
        snapshot_before,
        "preparation changes nothing"
    );
    assert_eq!(
        first.workspaces.iter().map(|w| (w.id, &w.name, w.screen)).collect::<Vec<_>>(),
        second.workspaces.iter().map(|w| (w.id, &w.name, w.screen)).collect::<Vec<_>>()
    );
    assert_eq!(first.moved, second.moved);
    assert_eq!(first.next_workspace, second.next_workspace);
}
