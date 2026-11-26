//! Display changes over the real sharded adapter and the fake AX arena: every
//! workspace, hidden ones included, is laid out again for the displays that
//! remain, and a later reveal has nothing left to write.

use stache_macos::DisplayInfo;
use stache_macos::applications::DisplayRect;

use super::*;

const BUILTIN: u32 = 1;
const EXTERNAL: u32 = 2;

/// The built-in 1920x1200 panel (primary) with a 30 pt menu bar.
fn builtin(width: f64, height: f64) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId::new(BUILTIN),
        frame: DisplayRect::new(0.0, 0.0, width, height),
        visible_frame: DisplayRect::new(0.0, 0.0, width, height - 30.0),
        scale: 2.0,
        refresh_hz: Some(60.0),
        name: Some("Built-in".to_owned()),
        is_primary: true,
        is_main: true,
        is_builtin: true,
    }
}

/// A 2560x1440 display above and to the right of the built-in one.
fn external() -> DisplayInfo {
    DisplayInfo {
        id: DisplayId::new(EXTERNAL),
        frame: DisplayRect::new(1920.0, 1200.0, 2560.0, 1440.0),
        visible_frame: DisplayRect::new(1920.0, 1200.0, 2560.0, 1410.0),
        scale: 2.0,
        refresh_hz: Some(60.0),
        name: Some("External".to_owned()),
        is_primary: false,
        is_main: false,
        is_builtin: false,
    }
}

fn both() -> DisplayLayout { DisplayLayout::new(vec![builtin(1920.0, 1200.0), external()]) }

/// The usable areas in the AX space (top-left origin, y down).
const BUILTIN_AREA: Rect = Rect::new(0.0, 30.0, 1920.0, 1170.0);
const EXTERNAL_AREA: Rect = Rect::new(1920.0, -1410.0, 2560.0, 1410.0);

fn inside(frame: &Rect, area: &Rect) -> bool {
    frame.x >= area.x - 0.5
        && frame.y >= area.y - 0.5
        && frame.max_x() <= area.max_x() + 0.5
        && frame.max_y() <= area.max_y() + 0.5
        && frame.width > 1.0
        && frame.height > 1.0
}

/// `coding` (Grid of five, app 1) and `web` (app 2) on the external display,
/// `terminal` (app 3) and `notes` (app 4, twenty windows: more than the
/// in-flight frame limit) on the built-in one.
fn workspaces() -> Arc<TilingConfig> {
    config(
        0,
        &json!([
            { "name": "terminal", "layout": "master", "screen": "main",
              "rules": [{ "app_id": "com.test.app3" }] },
            { "name": "notes", "layout": "master", "screen": "main",
              "rules": [{ "app_id": "com.test.app4" }] },
            { "name": "coding", "layout": "grid", "screen": "secondary",
              "rules": [{ "app_id": "com.test.app1" }] },
            { "name": "web", "layout": "monocle", "screen": "secondary",
              "rules": [{ "app_id": "com.test.app2" }] },
        ]),
    )
}

impl World {
    fn focus(&self, workspace: &str) {
        self.runtime
            .ingress()
            .submit(Input::Intent(crate::intent::Intent::FocusWorkspace(
                workspace.to_owned(),
            )))
            .expect("intent");
    }

    fn visible_names(&self) -> Vec<String> {
        let snap = self.runtime.snapshot();
        snap.workspaces
            .iter()
            .filter(|ws| ws.visible)
            .map(|ws| ws.name.clone())
            .collect()
    }

    fn all_nodes(&self) -> Vec<NodeId> {
        self.applications.iter().flat_map(|a| a.nodes.iter().copied()).collect()
    }

    fn quiet(&self) -> bool {
        let snap = self.runtime.snapshot();
        self.runtime.outstanding_frames() == 0 && snap.frames.iter().all(|f| !f.in_flight)
    }
}

/// Boots both displays with `coding` laid out on the external display, then
/// switches that display to `web`: `coding` is hidden with external frames.
fn desk() -> World {
    let world = World::boot_with(
        &[5, 1, 2, 20],
        workspaces(),
        7,
        FakeFrameClockFactory::failing("timer"),
        both(),
        &[],
        None,
        true,
    );
    assert_eq!(world.visible_names(), ["terminal", "coding"]);
    world.focus("web");
    wait_until("web visible", || {
        world.visible_names() == ["terminal", "web"] && world.all_at_target(world.nodes(1))
    });
    for node in world.nodes(0) {
        assert!(
            inside(&world.frame_of(*node), &EXTERNAL_AREA),
            "coding starts on the external display"
        );
    }
    world
}

#[test]
fn a_disconnect_lays_out_every_workspace_hidden_ones_included_on_the_remaining_display() {
    let world = desk();
    let base = world.spy.batches().len();
    world
        .runtime
        .submit_displays(None, DisplayLayout::new(vec![builtin(1920.0, 1200.0)]))
        .expect("displays");
    wait_until("every window on the built-in display", || {
        world.quiet()
            && world
                .all_nodes()
                .iter()
                .all(|node| inside(&world.frame_of(*node), &BUILTIN_AREA))
    });
    // The hidden Grid of five: five distinct cells, none of the old frames.
    let cells: Vec<Rect> = world.nodes(0).iter().map(|node| world.frame_of(*node)).collect();
    for (index, cell) in cells.iter().enumerate() {
        assert!(cell.width < 1920.0, "a Grid cell: {cell:?}");
        assert!(
            cells[index + 1..].iter().all(|other| other != cell),
            "distinct: {cells:?}"
        );
    }
    // The hidden `notes` was written for its layout too.
    assert!(world.position_writes(world.nodes(3)) > 0);
    let hidden = [
        world.applications[0].info.application,
        world.applications[3].info.application,
    ];
    let frames: Vec<_> = world
        .spy
        .frames_of(base)
        .into_iter()
        .filter(|(application, _, _)| hidden.contains(application))
        .collect();
    assert!(!frames.is_empty());
    assert!(
        frames.iter().all(|(_, _, stage)| *stage == FrameStage::Direct),
        "hidden-workspace writes are direct: {frames:?}"
    );

    // Revealing `coding` afterwards writes nothing: its windows are at target.
    let before = world.position_writes(world.nodes(0));
    let mark = world.ax.calls().len();
    world.focus("coding");
    wait_until("coding visible", || {
        world.visible_names() == ["coding"] && world.quiet() && world.all_at_target(world.nodes(0))
    });
    assert_eq!(
        world.position_writes(world.nodes(0)),
        before,
        "no jump on reveal"
    );
    let sizes = world.ax.calls()[mark..]
        .iter()
        .filter(|c| matches!(c, Call::SetSize(node, _) if world.nodes(0).contains(node)))
        .count();
    assert_eq!(sizes, 0, "no size write on reveal");
}

#[test]
fn a_returning_display_takes_its_workspaces_home_and_writes_all_their_windows() {
    let world = desk();
    world
        .runtime
        .submit_displays(None, DisplayLayout::new(vec![builtin(1920.0, 1200.0)]))
        .expect("displays");
    wait_until("everything on the built-in display", || {
        world.quiet()
            && world
                .all_nodes()
                .iter()
                .all(|node| inside(&world.frame_of(*node), &BUILTIN_AREA))
    });
    world.runtime.submit_displays(None, both()).expect("displays");
    wait_until("coding and web home on the external display", || {
        world.quiet()
            && world
                .nodes(0)
                .iter()
                .chain(world.nodes(1))
                .all(|node| inside(&world.frame_of(*node), &EXTERNAL_AREA))
    });
    let visible = world.visible_names();
    assert!(visible.contains(&"terminal".to_owned()), "{visible:?}");
    for node in world.nodes(2).iter().chain(world.nodes(3)) {
        assert!(
            inside(&world.frame_of(*node), &BUILTIN_AREA),
            "main workspaces stay"
        );
    }
}

#[test]
fn a_resolution_change_relays_out_every_workspace() {
    let world = desk();
    let smaller = Rect::new(0.0, 30.0, 1512.0, 952.0);
    world
        .runtime
        .submit_displays(
            None,
            DisplayLayout::new(vec![builtin(1512.0, 982.0), external()]),
        )
        .expect("displays");
    wait_until("main workspaces fit the new resolution", || {
        world.quiet()
            && world
                .nodes(2)
                .iter()
                .chain(world.nodes(3))
                .all(|node| inside(&world.frame_of(*node), &smaller))
    });
    // Twenty hidden windows exceed the in-flight limit: the queued ones are
    // issued as slots free.
    assert!(world.nodes(3).len() > crate::state::MAX_IN_FLIGHT_FRAMES);
    assert!(world.position_writes(world.nodes(3)) > 0, "hidden notes written");
}
