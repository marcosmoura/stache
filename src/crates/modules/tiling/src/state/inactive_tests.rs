//! Inactive workspaces: the reachability rule, the refused focus, the cycle and
//! the routes that stay open. Pure transitions only.

use std::sync::Arc;

use stache_macos::DisplayLayout;
use stache_macos::windows::{SnapshotOrigin, WindowChange, WindowEventKind, WindowInstance};

use super::*;
use crate::config::TilingConfig;
use crate::intent::{Cycle, Intent, IntentNoOp, IntentOutcome, IntentRejection};
use crate::published::project;
use crate::test_support::{Feed, display, info, one_display, ready, run, snapshot, step};

const EDITOR: u64 = 1;
const FINDER: u64 = 2;
const TERM: u64 = 3;
const BROWSER: u64 = 4;

/// `a` to `d` on the main display; with two displays, `x` on the other one.
fn config(two: bool) -> Arc<TilingConfig> {
    let mut workspaces = serde_json::json!([
        { "name": "a", "screen": "main", "rules": [{ "app_id": "com.editor" }] },
        { "name": "b", "screen": "main", "rules": [{ "app_id": "com.apple.finder" }] },
        { "name": "c", "screen": "main", "rules": [{ "app_id": "com.term" }] },
        { "name": "d", "screen": "main" },
    ]);
    if two {
        workspaces.as_array_mut().expect("array").push(serde_json::json!(
            { "name": "x", "screen": "secondary", "rules": [{ "app_id": "com.browser" }] }
        ));
    }
    Arc::new(
        serde_json::from_value(serde_json::json!({ "enabled": true, "workspaces": workspaces }))
            .expect("config"),
    )
}

fn app_info(n: u64) -> crate::state::AppInfo {
    match n {
        EDITOR => info(EDITOR, "com.editor", "Editor"),
        FINDER => info(FINDER, "com.apple.finder", "Finder"),
        TERM => info(TERM, "com.term", "Terminal"),
        _ => info(BROWSER, "com.browser", "Browser"),
    }
}

fn two_displays() -> DisplayLayout {
    DisplayLayout::new(vec![
        display(1, true, Some(60.0), 0.0),
        display(2, false, Some(60.0), 1920.0),
    ])
}

struct World {
    state: TilingState,
    feed: Feed,
}

impl World {
    /// Started and enumerated with `apps` running and none of them holding a
    /// window yet.
    fn new(apps: &[u64], two: bool) -> Self {
        let mut feed = Feed::new(1);
        let infos: Vec<_> = apps.iter().map(|n| app_info(*n)).collect();
        let state = ready("com.stache.app", &infos, &mut feed);
        let mut world = Self { state, feed };
        world.boot(two);
        world
    }

    /// Started, but no application has finished its window listing.
    fn starting(apps: &[u64], two: bool) -> Self {
        let feed = Feed::new(1);
        let infos: Vec<_> = apps.iter().map(|n| app_info(*n)).collect();
        let (state, _) = run(TilingState::new("com.stache.app"), vec![
            Input::Displays(Arc::new(one_display())),
            Input::Start(feed.stamp),
            Input::Applications { stamp: feed.stamp, apps: infos },
        ]);
        let mut world = Self { state, feed };
        world.boot(two);
        assert!(!world.state.completed_once);
        world
    }

    fn boot(&mut self, two: bool) {
        if two {
            self.send(Input::Displays(Arc::new(two_displays())));
        }
        self.send(Input::Config {
            revision: 1,
            config: config(two),
        });
        self.send(Input::FrameWrites(true));
    }

    fn send(&mut self, input: Input) -> Vec<Effect> {
        let state = std::mem::replace(&mut self.state, TilingState::new(""));
        let (next, effects) = step(state, input);
        self.state = next;
        effects
    }

    /// A new window of application `n` (its rule places it).
    fn add(&mut self, n: u64, serial: u64, x: f64) -> WindowInstance {
        let token = self.feed.token(n, serial);
        let mut snap = snapshot(token, "w", u32::try_from(n * 100 + serial).unwrap_or(1));
        snap.frame.position =
            stache_macos::ax::AxReading::Value(stache_macos::ax::AxPoint { x: x + 10.0, y: 10.0 });
        let input = self.feed.input(WindowEventKind::Snapshot {
            snapshot: snap,
            origin: SnapshotOrigin::Created,
        });
        self.send(input);
        token
    }

    fn minimize(&mut self, token: WindowInstance) {
        let input = self.feed.change(token, WindowChange::Miniaturized);
        self.send(input);
    }

    fn intent(&mut self, intent: Intent) -> Vec<Effect> { self.send(Input::Intent(intent)) }

    fn focus(&mut self, name: &str) -> Vec<Effect> {
        self.intent(Intent::FocusWorkspace(name.to_owned()))
    }

    fn outcome(&self) -> IntentOutcome {
        self.state.last_intent().expect("an intent ran").outcome.clone()
    }

    fn id(&self, name: &str) -> WorkspaceId {
        self.state.workspaces.iter().find(|ws| ws.name == name).expect("workspace").id
    }

    fn unreachable(&self) -> Vec<String> {
        let snapshot = self.state.snapshot();
        snapshot
            .workspaces
            .iter()
            .filter(|ws| snapshot.unreachable.contains(&ws.id))
            .map(|ws| ws.name.clone())
            .collect()
    }

    fn visible(&self) -> Vec<String> {
        self.state
            .workspaces
            .iter()
            .filter(|ws| ws.visible)
            .map(|ws| ws.name.clone())
            .collect()
    }
}

/// One state of the IW-01-01 matrix and whether `target` is inactive in it.
struct Case {
    name: &'static str,
    world: World,
    target: &'static str,
    inactive: bool,
}

/// The rows where Finder is not running: the planner never picks a
/// windowless host then, so only the other-display rule applies.
fn without_finder() -> Vec<Case> {
    let mut out = Vec::new();
    let mut world = World::new(&[EDITOR, TERM], false);
    world.add(EDITOR, 1, 0.0);
    out.push(Case {
        name: "Finder not running, a windowless application running",
        world,
        target: "d",
        inactive: true,
    });

    let mut world = World::new(&[EDITOR, TERM, BROWSER], true);
    world.add(EDITOR, 1, 0.0);
    world.add(BROWSER, 1, 1920.0);
    out.push(Case {
        name: "Finder not running, a window on the other display",
        world,
        target: "d",
        inactive: false,
    });
    out
}

/// Every state the acceptance criteria name. The focused workspace is `a`.
fn cases() -> Vec<Case> {
    let mut out = Vec::new();

    out.push(Case {
        name: "no windows and no application anywhere",
        world: World::new(&[], false),
        target: "c",
        inactive: true,
    });

    let mut world = World::new(&[EDITOR, FINDER], false);
    world.add(EDITOR, 1, 0.0);
    out.push(Case {
        name: "Finder without a window hosts the desktop",
        world,
        target: "c",
        inactive: false,
    });

    let mut world = World::new(&[EDITOR, FINDER], false);
    world.add(EDITOR, 1, 0.0);
    world.add(FINDER, 1, 0.0);
    out.push(Case {
        name: "Finder holds a window on a hidden workspace, nothing is windowless",
        world,
        target: "d",
        inactive: true,
    });

    let mut world = World::new(&[EDITOR, FINDER, TERM], false);
    world.add(EDITOR, 1, 0.0);
    world.add(FINDER, 1, 0.0);
    out.push(Case {
        name: "the same with a windowless application",
        world,
        target: "d",
        inactive: false,
    });

    out.extend(without_finder());

    let mut world = World::new(&[EDITOR, FINDER, BROWSER], true);
    world.add(EDITOR, 1, 0.0);
    world.add(FINDER, 1, 0.0);
    world.add(BROWSER, 1, 1920.0);
    out.push(Case {
        name: "two displays, a window on the other display's visible workspace",
        world,
        target: "d",
        inactive: false,
    });

    let mut world = World::new(&[EDITOR, FINDER], true);
    world.add(EDITOR, 1, 0.0);
    world.add(FINDER, 1, 0.0);
    out.push(Case {
        name: "two displays, the other display's visible workspace is empty",
        world,
        target: "d",
        inactive: true,
    });

    out.push(Case {
        name: "the target is visible (and empty)",
        world: World::new(&[], false),
        target: "a",
        inactive: false,
    });

    let mut world = World::new(&[EDITOR, FINDER, TERM], false);
    world.add(EDITOR, 1, 0.0);
    world.add(FINDER, 1, 0.0);
    let term = world.add(TERM, 1, 0.0);
    world.minimize(term);
    out.push(Case {
        name: "the target holds only a minimized window",
        world,
        target: "c",
        inactive: false,
    });

    out.push(Case {
        name: "startup not complete, Finder without a window",
        world: World::starting(&[FINDER, EDITOR], false),
        target: "c",
        inactive: true,
    });

    let mut world = World::starting(&[FINDER, BROWSER], true);
    world.add(BROWSER, 1, 1920.0);
    out.push(Case {
        name: "startup not complete, a window on the other display",
        world,
        target: "c",
        inactive: false,
    });

    out
}

/// IW-01-01 and IW-01-03: each state is built once; the rule, the published
/// flag and the focus outcome agree.
#[test]
fn the_published_flag_and_the_focus_outcome_follow_one_rule() {
    for Case {
        name,
        mut world,
        target,
        inactive,
    } in cases()
    {
        let published = project(&world.state.snapshot());
        let flag = published
            .workspaces
            .iter()
            .find(|ws| ws.name == target)
            .expect("published workspace")
            .inactive;
        assert_eq!(flag, inactive, "{name}: published flag");
        assert_eq!(
            world.unreachable().contains(&target.to_owned()),
            inactive,
            "{name}: the rule"
        );
        world.focus(target);
        let rejected =
            world.outcome() == IntentOutcome::Rejected(IntentRejection::InactiveWorkspace);
        assert_eq!(
            rejected,
            flag,
            "{name}: the focus outcome: {:?}",
            world.outcome()
        );
    }
}

/// D5: a visible workspace is never inactive, and focusing it is unchanged.
#[test]
fn the_focused_empty_workspace_is_not_inactive_and_its_focus_is_a_no_op() {
    let mut world = World::new(&[], false);
    assert_eq!(world.unreachable(), ["b", "c", "d"]);
    world.focus("a");
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::Unchanged));
    assert_eq!(world.state.refused_focus, None);
}

fn finder_held() -> World {
    let mut world = World::new(&[EDITOR, FINDER], false);
    world.add(EDITOR, 1, 0.0);
    world.add(FINDER, 1, 0.0);
    world
}

/// IW-01-02: a refusal changes nothing but the record, sends nothing and adds
/// no history; the counter increases on every refusal.
#[test]
fn a_refused_focus_changes_nothing_but_the_refusal_record() {
    let mut world = finder_held();
    let before = world.state.snapshot();
    let effects = world.focus("d");
    assert_eq!(
        world.outcome(),
        IntentOutcome::Rejected(IntentRejection::InactiveWorkspace)
    );
    assert!(
        effects.iter().all(|effect| matches!(effect, Effect::Publish)),
        "nothing native: {effects:?}"
    );
    let after = world.state.snapshot();
    assert_eq!(after.workspaces, before.workspaces);
    assert_eq!(after.focus, before.focus);
    assert_eq!(after.workspace_history, before.workspace_history);
    assert_eq!(after.visibility, before.visibility);
    assert_eq!(
        world.state.refused_focus,
        Some(RefusedFocus {
            workspace: world.id("d"),
            serial: 1,
        })
    );
    world.focus("c");
    assert_eq!(
        world.state.refused_focus,
        Some(RefusedFocus {
            workspace: world.id("c"),
            serial: 2,
        })
    );
    world.focus("d");
    assert_eq!(world.state.refused_focus.map(|r| r.serial), Some(3));
    // An ordinary switch keeps the record.
    world.focus("b");
    assert_eq!(world.outcome(), IntentOutcome::Applied);
    assert_eq!(world.state.refused_focus.map(|r| r.serial), Some(3));
}

#[test]
fn the_refusal_counter_wraps() {
    let mut world = finder_held();
    world.state.refused_focus = Some(RefusedFocus {
        workspace: world.id("c"),
        serial: u32::MAX,
    });
    world.focus("d");
    assert_eq!(
        world.state.refused_focus,
        Some(RefusedFocus {
            workspace: world.id("d"),
            serial: 0,
        })
    );
}

#[test]
fn a_new_generation_starts_without_a_refusal_record() {
    let state = TilingState::new("com.stache.app");
    assert_eq!(state.snapshot().refused_focus, None);
    assert_eq!(project(&state.snapshot()).refused_focus, None);
}

/// IW-01-05: the cycle steps over inactive workspaces.
#[test]
fn the_cycle_skips_inactive_workspaces() {
    let mut world = finder_held();
    assert_eq!(world.unreachable(), ["c", "d"]);
    world.intent(Intent::CycleWorkspace(Cycle::Next));
    assert_eq!(world.visible(), ["b"]);
    world.intent(Intent::CycleWorkspace(Cycle::Next));
    assert_eq!(world.visible(), ["a"], "c and d are stepped over");
    world.intent(Intent::CycleWorkspace(Cycle::Previous));
    assert_eq!(world.visible(), ["b"]);
}

#[test]
fn the_cycle_is_a_no_op_with_fewer_than_two_workspaces_left() {
    let mut world = World::new(&[], false);
    world.intent(Intent::CycleWorkspace(Cycle::Next));
    assert_eq!(world.outcome(), IntentOutcome::NoOp(IntentNoOp::TooFewWindows));
    assert_eq!(world.visible(), ["a"]);
}

/// IW-01-05 and D4: sending a window to an inactive workspace works and makes
/// it active; so does a rule-assigned new window.
#[test]
fn a_sent_or_rule_assigned_window_makes_an_inactive_workspace_active() {
    let mut world = finder_held();
    let editor = world.add(EDITOR, 2, 0.0);
    assert_eq!(world.unreachable(), ["c", "d"]);
    world.intent(Intent::SendWindowToWorkspace {
        window: Some(editor),
        workspace: "d".to_owned(),
    });
    assert_eq!(world.outcome(), IntentOutcome::Applied);
    assert_eq!(world.unreachable(), ["c"], "d holds a window now");
    world.focus("d");
    assert_eq!(world.outcome(), IntentOutcome::Applied);

    // Terminal launches; its first window lands on the inactive `c` by rule.
    world.focus("a");
    assert_eq!(world.unreachable(), ["c"]);
    world.send(Input::Lifecycle {
        kind: stache_macos::applications::LifecycleKind::Launched,
        info: app_info(TERM),
    });
    assert_eq!(world.unreachable(), ["c"], "its listing has not finished");
    world.send(Input::Completion {
        stamp: world.feed.stamp,
        result: CompletionResult::Track(
            crate::test_support::app(TERM),
            stache_macos::windows::TrackOutcome::Tracking,
        ),
    });
    assert_eq!(world.unreachable(), ["c"]);
    world.add(TERM, 1, 0.0);
    assert!(world.unreachable().is_empty());
    world.focus("c");
    assert_eq!(world.outcome(), IntentOutcome::Applied);
}

/// The published projection copies the refusal; a removed workspace clears it.
#[test]
fn the_refused_focus_is_published_and_cleared_with_its_workspace() {
    let mut world = finder_held();
    world.focus("d");
    let published = project(&world.state.snapshot());
    assert_eq!(
        published.refused_focus,
        Some(crate::published::PublishedRefusedFocus {
            workspace: "d".to_owned(),
            serial: 1,
        })
    );
    let flags: Vec<_> =
        published.workspaces.iter().map(|ws| (ws.name.as_str(), ws.inactive)).collect();
    assert_eq!(flags, [("a", false), ("b", false), ("c", true), ("d", true)]);
    world.send(Input::Config {
        revision: 2,
        config: Arc::new(
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "workspaces": [
                    { "name": "a", "screen": "main", "rules": [{ "app_id": "com.editor" }] },
                    { "name": "b", "screen": "main", "rules": [{ "app_id": "com.apple.finder" }] },
                ],
            }))
            .expect("config"),
        ),
    });
    assert_eq!(project(&world.state.snapshot()).refused_focus, None);
}
