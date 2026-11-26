//! Passive drag over the real sharded adapter and the fake AX arena, with user
//! gestures modelled as macOS delivers them: a mouse down, the window moving or
//! resizing in steps with an `AXWindowMoved`/`AXWindowResized` per step, a last
//! notification racing the up, the up, then late notifications after it.

use stache_macos::ax::AxNotification;

use super::*;
use crate::state::DragOutcome;

fn grid() -> Arc<TilingConfig> {
    config(
        0,
        &json!([{ "name": "files", "layout": "grid", "screen": "main" }]),
    )
}

fn point(x: f64, y: f64) -> DragPoint { DragPoint { x, y } }

impl World {
    fn drag(&self, signal: DragSignal) {
        self.runtime.ingress().submit(Input::Drag(signal)).expect("drag");
    }

    fn down(&self, at: DragPoint) {
        self.drag(DragSignal::Down {
            button: 0,
            at,
            epoch: 1,
            time: Duration::ZERO,
        });
        wait_until("the gesture", || self.runtime.snapshot().drag.active);
    }

    fn up(&self, at: DragPoint) {
        self.drag(DragSignal::Up {
            button: 0,
            at,
            epoch: 1,
            time: Duration::from_millis(400),
        });
    }

    /// The application owning `node` posts `notification` for it; a blocking
    /// read wakes the shard so the observer is drained (the read itself is not
    /// reported to the actor).
    fn notify(&self, node: NodeId, notification: AxNotification) {
        let pid = self
            .applications
            .iter()
            .find(|a| a.nodes.contains(&node))
            .map(|a| a.info.pid)
            .expect("owner");
        self.observers.inject(pid, notification, node);
        let _ = self.spy.inner.snapshot(
            self.token(node),
            &CancellationToken::new(),
            Some(Instant::now() + Duration::from_secs(5)),
        );
    }

    /// The user moves or resizes `node` to `frame` (one AX step).
    fn user_step(&self, node: NodeId, frame: Rect, notification: AxNotification) {
        self.ax.edit(node, |n| {
            n.position = Ok(AxPoint { x: frame.x, y: frame.y });
            n.size = Ok(AxSize {
                width: frame.width,
                height: frame.height,
            });
        });
        self.notify(node, notification);
        std::thread::sleep(Duration::from_millis(4));
    }

    /// A whole gesture on `node`: down at its centre, `steps` frames towards
    /// `to`, the last notification racing the up, then two late echoes.
    fn gesture(&self, node: NodeId, to: Rect, notification: AxNotification) {
        self.gesture_with(node, to, notification, true);
    }

    /// [`Self::gesture`]; with `during` false the application posts nothing
    /// while the button is held (many do so only once the move ends).
    fn gesture_with(&self, node: NodeId, to: Rect, notification: AxNotification, during: bool) {
        let from = self.frame_of(node);
        let start = crate::drag::center(&from);
        self.down(start);
        let steps = 6;
        for step in 1..=steps {
            let t = f64::from(step) / f64::from(steps);
            let frame = Rect::new(
                from.x + (to.x - from.x) * t,
                from.y + (to.y - from.y) * t,
                from.width + (to.width - from.width) * t,
                from.height + (to.height - from.height) * t,
            );
            if step == steps || !during {
                self.ax.edit(node, |n| {
                    n.position = Ok(AxPoint { x: frame.x, y: frame.y });
                    n.size = Ok(AxSize {
                        width: frame.width,
                        height: frame.height,
                    });
                });
            } else {
                self.user_step(node, frame, notification);
            }
        }
        let end = point(
            start.x + (to.x - from.x) + (to.width - from.width),
            start.y + (to.y - from.y),
        );
        // The final notification races the up.
        let pid = self
            .applications
            .iter()
            .find(|a| a.nodes.contains(&node))
            .map(|a| a.info.pid)
            .expect("owner");
        self.observers.inject(pid, notification, node);
        self.up(end);
        self.notify(node, notification);
        // Late echoes after the up.
        std::thread::sleep(Duration::from_millis(3));
        self.notify(node, notification);
        std::thread::sleep(Duration::from_millis(30));
        self.notify(node, notification);
    }

    fn last_outcome(&self) -> Option<DragOutcome> {
        let snap = self.runtime.snapshot();
        if snap.drag.active {
            return None;
        }
        snap.drag.last.as_ref().map(|last| last.outcome.clone())
    }

    /// Every tile is at its planned frame, within the planner's write
    /// tolerance: after a mouse resize the resized tile's new plan may differ
    /// from where the user left it by a rounding error, which is not written.
    fn all_near_target(&self, nodes: &[NodeId]) -> bool {
        let desired = self.desired();
        nodes.iter().all(|node| {
            desired.get(&self.token(*node)).is_some_and(|target| {
                let frame = self.frame_of(*node);
                [
                    (target.x, frame.x),
                    (target.y, frame.y),
                    (target.width, frame.width),
                    (target.height, frame.height),
                ]
                .iter()
                .all(|(a, b)| (a - b).abs() < 0.5)
            })
        })
    }

    /// Every tile ends at its planned frame, nothing in flight, no inferred
    /// minimum, no overlap.
    fn assert_back_in_layout(&self, nodes: &[NodeId]) {
        wait_until("every tile back at its planned frame", || {
            self.runtime.outstanding_frames() == 0 && self.all_near_target(nodes)
        });
        // Late echoes must not undo it.
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            self.all_near_target(nodes),
            "a late echo left a tile off its frame"
        );
        let snap = self.runtime.snapshot();
        for frame in &snap.frames {
            assert_eq!(frame.minimum, None, "no inferred minimum: {frame:?}");
        }
        let desired: Vec<Rect> = self.desired().values().copied().collect();
        for (i, a) in desired.iter().enumerate() {
            for b in &desired[i + 1..] {
                let overlap_x = a.x.max(b.x) < (a.x + a.width).min(b.x + b.width) - 0.5;
                let overlap_y = a.y.max(b.y) < (a.y + a.height).min(b.y + b.height) - 0.5;
                assert!(!(overlap_x && overlap_y), "tiles overlap: {a:?} {b:?}");
            }
        }
    }
}

fn booted() -> (World, Vec<NodeId>) {
    let world = World::boot(&[3], grid());
    world.echo_moves();
    let nodes = world.nodes(0).to_vec();
    (world, nodes)
}

#[test]
fn a_tile_dragged_inside_its_own_cell_snaps_back_after_the_drop() {
    let (world, nodes) = booted();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x + 120.0, cell.y + 80.0, cell.width, cell.height);
    world.gesture(node, to, AxNotification::WindowMoved);
    wait_until("the commit", || world.last_outcome().is_some());
    assert_eq!(world.last_outcome(), Some(DragOutcome::SnappedBack));
    world.assert_back_in_layout(&nodes);
    assert_eq!(world.frame_of(node), cell);
    let status = crate::ipc::status(&world.runtime.snapshot(), None, None).drag;
    assert_eq!((status.gestures, status.snap_backs), (1, 1), "{status:?}");
    assert!(status.available && !status.active);
    assert_eq!(
        status.last.map(|last| last.outcome).as_deref(),
        Some("snappedBack")
    );
}

#[test]
fn a_tile_dropped_onto_another_swaps_with_it() {
    let (world, nodes) = booted();
    let (a, b) = (nodes[0], nodes[1]);
    let (cell_a, cell_b) = (world.frame_of(a), world.frame_of(b));
    let centre_b = crate::drag::center(&cell_b);
    let to = Rect::new(
        centre_b.x - cell_a.width / 2.0,
        centre_b.y - cell_a.height / 2.0,
        cell_a.width,
        cell_a.height,
    );
    world.gesture(a, to, AxNotification::WindowMoved);
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(world.last_outcome(), Some(DragOutcome::Swapped { .. })),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
    // Same sizes in a grid of equal cells: each took the other's cell.
    assert_eq!(world.frame_of(a).x, cell_b.x);
    assert_eq!(world.frame_of(a).y, cell_b.y);
    assert_eq!(world.frame_of(b).x, cell_a.x);
    assert_eq!(world.frame_of(b).y, cell_a.y);
    assert_eq!(
        crate::ipc::status(&world.runtime.snapshot(), None, None).drag.swaps,
        1
    );
}

/// The live capture: a Grid tile widened with the mouse, then late growth
/// echoes. No inferred minimum, every tile back in a valid layout.
#[test]
fn a_grid_tile_resized_by_the_user_never_leaves_an_inferred_minimum_or_an_overlap() {
    let (world, nodes) = booted();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x, cell.y, cell.width - 330.0, cell.height);
    world.gesture(node, to, AxNotification::WindowResized);
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(world.last_outcome(), Some(DragOutcome::Resized { .. })),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
}

/// The user's resize grows the tile; the late echo reads the larger size.
#[test]
fn a_grid_tile_grown_by_the_user_never_leaves_an_inferred_minimum_or_an_overlap() {
    let (world, nodes) = booted();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x, cell.y, cell.width + 200.0, cell.height);
    world.gesture(node, to, AxNotification::WindowResized);
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(world.last_outcome(), Some(DragOutcome::Resized { .. })),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
}

/// The last plan was animated (a gap change), so its settle-echo budget is
/// still unspent when the user drags; the application reports the move only
/// once it ends. The user's frame is not an animation echo.
#[test]
fn a_drag_right_after_an_animated_layout_change_still_snaps_back() {
    let (mut world, nodes) = booted();
    world.reload(config(
        20,
        &json!([{ "name": "files", "layout": "grid", "screen": "main" }]),
    ));
    wait_until("the animated layout", || {
        world.is_settled()
            && world.all_at_target(&nodes)
            && (world.frame_of(nodes[0]).x - 20.0).abs() < 0.5
    });
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x + 120.0, cell.y + 80.0, cell.width, cell.height);
    world.gesture_with(node, to, AxNotification::WindowMoved, false);
    wait_until("the commit", || world.last_outcome().is_some());
    assert_eq!(world.last_outcome(), Some(DragOutcome::SnappedBack));
    world.assert_back_in_layout(&nodes);
    assert_eq!(world.frame_of(node), cell);
}

/// The same after an animated layout change, for a resize.
#[test]
fn a_resize_right_after_an_animated_layout_change_relays_out() {
    let (mut world, nodes) = booted();
    world.reload(config(
        20,
        &json!([{ "name": "files", "layout": "grid", "screen": "main" }]),
    ));
    wait_until("the animated layout", || {
        world.is_settled()
            && world.all_at_target(&nodes)
            && (world.frame_of(nodes[0]).x - 20.0).abs() < 0.5
    });
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x, cell.y, cell.width - 300.0, cell.height);
    world.gesture_with(node, to, AxNotification::WindowResized, false);
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(world.last_outcome(), Some(DragOutcome::Resized { .. })),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
}

// ------------------------------------------------------- edge-aware resize

fn layout_config(layout: &str, gaps: u32) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "enabled": true,
            "gaps": { "inner": gaps, "outer": gaps },
            "animations": { "enabled": true, "duration": 200 },
            "workspaces": [{ "name": "w", "layout": layout, "screen": "main" }],
        }))
        .expect("config"),
    )
}

fn booted_with(counts: &[usize], config: Arc<TilingConfig>) -> (World, Vec<NodeId>) {
    let world = World::boot(counts, config);
    world.echo_moves();
    let nodes = world.nodes(0).to_vec();
    (world, nodes)
}

fn close(a: f64, b: f64) -> bool { (a - b).abs() < 0.5 }

fn same(a: Rect, b: Rect) -> bool {
    close(a.x, b.x) && close(a.y, b.y) && close(a.width, b.width) && close(a.height, b.height)
}

/// The user resizes `node` to `to`; waits for the resize commit and the
/// replanned layout.
fn resize_and_settle(world: &World, nodes: &[NodeId], node: NodeId, to: Rect) {
    world.gesture(node, to, AxNotification::WindowResized);
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(
            world.last_outcome(),
            Some(DragOutcome::Resized { changed: true, .. })
        ),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(nodes);
}

/// The live report: a 1920x1200 display, Grid with 14-point gaps, three
/// windows; the user narrows the left tile to 631 wide. The left column keeps
/// 631 and both right tiles fill the rest without overlap.
#[test]
fn grid_left_tile_narrowed_to_631_keeps_its_width_and_the_right_tiles_fill_the_rest() {
    let mut info = crate::test_support::display(1, true, Some(60.0), 0.0);
    info.frame = stache_macos::DisplayRect::new(0.0, 0.0, 1920.0, 1200.0);
    info.visible_frame = stache_macos::DisplayRect::new(0.0, 0.0, 1920.0, 1158.0);
    let world = World::boot_with(
        &[3],
        layout_config("grid", 14),
        3,
        FakeFrameClockFactory::failing("timer"),
        DisplayLayout::new(vec![info]),
        &[],
        None,
        true,
    );
    world.echo_moves();
    let nodes = world.nodes(0).to_vec();
    let left = world.frame_of(nodes[0]);
    assert!(same(left, Rect::new(14.0, 56.0, 939.0, 1130.0)), "{left:?}");
    resize_and_settle(
        &world,
        &nodes,
        nodes[0],
        Rect::new(left.x, left.y, 631.0, left.height),
    );
    let frames: Vec<Rect> = nodes.iter().map(|n| world.frame_of(*n)).collect();
    assert!(
        same(frames[0], Rect::new(14.0, 56.0, 631.0, 1130.0)),
        "{frames:?}"
    );
    assert!(
        same(frames[1], Rect::new(659.0, 56.0, 1247.0, 558.0)),
        "{frames:?}"
    );
    assert!(
        same(frames[2], Rect::new(659.0, 628.0, 1247.0, 558.0)),
        "{frames:?}"
    );
}

/// Master on the left, three windows: the boundary between the two stack
/// windows moves only the stack; the master's right edge moves the share.
#[test]
fn master_stack_boundary_moves_the_stack_and_the_master_edge_moves_the_share() {
    let (world, nodes) = booted_with(&[3], layout_config("master", 0));
    let master = world.frame_of(nodes[0]);
    let top = world.frame_of(nodes[1]);
    resize_and_settle(
        &world,
        &nodes,
        nodes[1],
        Rect::new(top.x, top.y, top.width, top.height + 150.0),
    );
    assert_eq!(world.frame_of(nodes[0]), master);
    let (upper, lower) = (world.frame_of(nodes[1]), world.frame_of(nodes[2]));
    assert!(close(upper.height, top.height + 150.0), "{upper:?}");
    assert!(close(lower.y, upper.y + upper.height) && close(lower.height, top.height - 150.0));

    let narrower = Rect::new(master.x, master.y, master.width - 200.0, master.height);
    resize_and_settle(&world, &nodes, nodes[0], narrower);
    let (master_now, stack) = (world.frame_of(nodes[0]), world.frame_of(nodes[1]));
    assert!(close(master_now.width, master.width - 200.0), "{master_now:?}");
    assert!(close(stack.x, master_now.x + master_now.width) && close(stack.height, upper.height));
}

/// `split_horizontal`, three windows: dragging the middle window's left edge
/// changes only the left and middle tiles.
#[test]
fn split_horizontal_middle_left_edge_moves_only_its_boundary() {
    let (world, nodes) = booted_with(&[3], layout_config("split_horizontal", 0));
    let before: Vec<Rect> = nodes.iter().map(|n| world.frame_of(*n)).collect();
    let middle = before[1];
    resize_and_settle(
        &world,
        &nodes,
        nodes[1],
        Rect::new(middle.x - 200.0, middle.y, middle.width + 200.0, middle.height),
    );
    let after: Vec<Rect> = nodes.iter().map(|n| world.frame_of(*n)).collect();
    assert!(close(after[0].width, before[0].width - 200.0), "{after:?}");
    assert!(close(after[1].x, middle.x - 200.0) && close(after[1].width, middle.width + 200.0));
    assert_eq!(after[2], before[2]);
}

/// Dwindle, four windows: the last window's top edge belongs to split 1 (an
/// ancestor), which moves; the first window keeps its frame.
#[test]
fn dwindle_ancestor_edge_moves_that_split() {
    let (world, nodes) = booted_with(&[4], layout_config("dwindle", 0));
    let before: Vec<Rect> = nodes.iter().map(|n| world.frame_of(*n)).collect();
    let last = before[3];
    resize_and_settle(
        &world,
        &nodes,
        nodes[3],
        Rect::new(last.x, last.y - 120.0, last.width, last.height + 120.0),
    );
    let after: Vec<Rect> = nodes.iter().map(|n| world.frame_of(*n)).collect();
    assert_eq!(after[0], before[0]);
    assert!(close(after[1].height, before[1].height - 120.0), "{after:?}");
    assert!(close(after[2].y, last.y - 120.0) && close(after[3].y, last.y - 120.0));
    assert!(close(after[3].width, before[3].width), "{after:?}");
}

// ------------------------------------------------------- two displays

/// Workspace `a` on the main display holds application 1; Grid workspace `b`
/// on the secondary display holds application 2. Focus stays on `a`.
fn two_display_config(outer: u32) -> Arc<TilingConfig> {
    config(
        outer,
        &json!([
            { "name": "a", "layout": "grid", "screen": "main",
              "rules": [{ "app_id": "com.test.app1" }] },
            { "name": "b", "layout": "grid", "screen": "secondary",
              "rules": [{ "app_id": "com.test.app2" }] },
        ]),
    )
}

fn booted_on_two_displays() -> (World, Vec<NodeId>) {
    let mut world = World::boot_with(
        &[1, 3],
        two_display_config(0),
        4,
        FakeFrameClockFactory::failing("timer"),
        two_displays(),
        &[],
        None,
        true,
    );
    world.echo_moves();
    let nodes = world.nodes(1).to_vec();
    // An animated layout change leaves settle echoes unspent on display B.
    world.reload(two_display_config(20));
    wait_until("the animated layout", || {
        world.is_settled()
            && world.all_at_target(&nodes)
            && (world.frame_of(nodes[0]).x - 1940.0).abs() < 0.5
    });
    let snap = world.runtime.snapshot();
    let focused = snap.workspaces.iter().find(|ws| ws.focused).map(|ws| ws.name.clone());
    assert_eq!(focused.as_deref(), Some("a"), "focus stays on display A");
    (world, nodes)
}

/// A Grid tile on the non-focused display resized with the mouse: no inferred
/// minimum, no overlap, the tiles in the new layout.
#[test]
fn a_grid_tile_resized_on_the_unfocused_display_relays_out() {
    let (world, nodes) = booted_on_two_displays();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x, cell.y, cell.width - 300.0, cell.height);
    world.gesture(node, to, AxNotification::WindowResized);
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(
            world.last_outcome(),
            Some(DragOutcome::Resized { changed: true, .. })
        ),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
    assert!(
        close(world.frame_of(node).width, cell.width - 300.0),
        "{:?}",
        world.frame_of(node)
    );
}

/// A tile on the non-focused display dragged inside its own cell snaps back.
#[test]
fn a_tile_dragged_on_the_unfocused_display_snaps_back() {
    let (world, nodes) = booted_on_two_displays();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x + 120.0, cell.y + 80.0, cell.width, cell.height);
    world.gesture_with(node, to, AxNotification::WindowMoved, false);
    wait_until("the commit", || world.last_outcome().is_some());
    assert_eq!(world.last_outcome(), Some(DragOutcome::SnappedBack));
    world.assert_back_in_layout(&nodes);
    assert_eq!(world.frame_of(node), cell);
}

// ------------------------------------------- gestures from window geometry

thread_local! {
    /// The button state the next [`World::boot_with`] on this thread runs with.
    pub(super) static BOOT_BUTTONS: std::cell::RefCell<Option<Arc<dyn crate::drag::ButtonState>>> =
        const { std::cell::RefCell::new(None) };
}

/// The physical mouse button, driven by the test. Counts every read, so a
/// timer polling it while no gesture exists shows up.
#[derive(Default)]
struct FakeButtons {
    held: AtomicBool,
    reads: AtomicUsize,
}

impl crate::drag::ButtonState for FakeButtons {
    fn held(&self) -> bool {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.held.load(Ordering::SeqCst)
    }
}

impl FakeButtons {
    fn press(&self) { self.held.store(true, Ordering::SeqCst); }

    fn release(&self) { self.held.store(false, Ordering::SeqCst); }

    fn reads(&self) -> usize { self.reads.load(Ordering::SeqCst) }
}

/// Boots with an injected button state and a tap that delivers nothing (no
/// test below submits a down or an up unless it says so).
fn booted_with_buttons(counts: &[usize], config: Arc<TilingConfig>) -> (World, Arc<FakeButtons>) {
    let buttons = Arc::new(FakeButtons::default());
    BOOT_BUTTONS.with(|slot| {
        *slot.borrow_mut() = Some(Arc::clone(&buttons) as Arc<dyn crate::drag::ButtonState>);
    });
    let world = World::boot(counts, config);
    world.echo_moves();
    (world, buttons)
}

impl World {
    /// Native frame writes (position or size) to `nodes` so far.
    fn frame_writes(&self, nodes: &[NodeId]) -> usize {
        self.ax
            .calls()
            .iter()
            .filter(|c| {
                matches!(c, Call::SetPosition(node, _) | Call::SetSize(node, _) if nodes.contains(node))
            })
            .count()
    }

    /// The user drags or resizes `node` from its frame to `to` in `steps`
    /// steps, one notification each, `pause` apart. No tap event.
    fn held_steps(
        &self,
        node: NodeId,
        to: Rect,
        steps: u32,
        pause: Duration,
        notification: AxNotification,
    ) {
        let from = self.frame_of(node);
        for step in 1..=steps {
            let t = f64::from(step) / f64::from(steps);
            let frame = Rect::new(
                from.x + (to.x - from.x) * t,
                from.y + (to.y - from.y) * t,
                from.width + (to.width - from.width) * t,
                from.height + (to.height - from.height) * t,
            );
            self.ax.edit(node, |n| {
                n.position = Ok(AxPoint { x: frame.x, y: frame.y });
                n.size = Ok(AxSize {
                    width: frame.width,
                    height: frame.height,
                });
            });
            self.notify(node, notification);
            std::thread::sleep(pause);
        }
    }

    fn drag_status(&self) -> crate::dto::StatusDragDto {
        crate::ipc::status(&self.runtime.snapshot(), None, None).drag
    }
}

/// The live failure: the tap delivers no mouse event at all. A tile dragged
/// inside its own cell is never written while the button is held, and snaps
/// back once the button state shows the release.
#[test]
fn a_drag_with_a_silent_tap_writes_nothing_while_held_and_snaps_back_on_release() {
    let (world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x + 120.0, cell.y + 80.0, cell.width, cell.height);
    let before = world.frame_writes(&nodes);
    buttons.press();
    world.held_steps(
        node,
        to,
        6,
        Duration::from_millis(16),
        AxNotification::WindowMoved,
    );
    wait_until("the gesture", || world.runtime.snapshot().drag.active);
    // Held: late notifications and the actor's own polls change nothing.
    world.notify(node, AxNotification::WindowMoved);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        world.frame_writes(&nodes),
        before,
        "a write while the button is held"
    );
    assert_eq!(world.frame_of(node), to, "the user's frame stays while held");
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    assert_eq!(world.last_outcome(), Some(DragOutcome::SnappedBack));
    world.assert_back_in_layout(&nodes);
    assert_eq!(world.frame_of(node), cell);
    let status = world.drag_status();
    assert_eq!(
        (
            status.gestures,
            status.from_geometry,
            status.from_down,
            status.downs,
            status.ups
        ),
        (1, 1, 0, 0, 0),
        "{status:?}"
    );
    assert_eq!(status.released_by_state, 1, "{status:?}");
}

/// The same silent tap: a tile dropped onto another swaps with it.
#[test]
fn a_drag_with_a_silent_tap_swaps_on_release() {
    let (world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let (a, b) = (nodes[0], nodes[1]);
    let (cell_a, cell_b) = (world.frame_of(a), world.frame_of(b));
    let centre_b = crate::drag::center(&cell_b);
    let to = Rect::new(
        centre_b.x - cell_a.width / 2.0,
        centre_b.y - cell_a.height / 2.0,
        cell_a.width,
        cell_a.height,
    );
    let before = world.frame_writes(&nodes);
    buttons.press();
    world.held_steps(a, to, 6, Duration::from_millis(16), AxNotification::WindowMoved);
    wait_until("the gesture", || world.runtime.snapshot().drag.active);
    assert_eq!(world.frame_writes(&nodes), before);
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(world.last_outcome(), Some(DragOutcome::Swapped { .. })),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
    assert_eq!((world.frame_of(a).x, world.frame_of(a).y), (cell_b.x, cell_b.y));
    assert_eq!((world.frame_of(b).x, world.frame_of(b).y), (cell_a.x, cell_a.y));
}

/// A resize held for two seconds with a notification every 16 ms: not one
/// frame write to the workspace until the release, then the neighbours follow.
#[test]
fn a_two_second_resize_writes_nothing_until_release_then_relays_out() {
    let (world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let node = nodes[0];
    let cell = world.frame_of(node);
    let to = Rect::new(cell.x, cell.y, cell.width - 300.0, cell.height);
    let before = world.frame_writes(&nodes);
    buttons.press();
    world.held_steps(
        node,
        to,
        125,
        Duration::from_millis(16),
        AxNotification::WindowResized,
    );
    assert_eq!(
        world.frame_writes(&nodes),
        before,
        "a write while the button is held"
    );
    assert!(world.runtime.snapshot().drag.active);
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(
            world.last_outcome(),
            Some(DragOutcome::Resized { changed: true, .. })
        ),
        "{:?}",
        world.last_outcome()
    );
    world.assert_back_in_layout(&nodes);
    assert!(
        close(world.frame_of(node).width, cell.width - 300.0),
        "{:?}",
        world.frame_of(node)
    );
}

/// The tap delivers the down but misses the up: the gesture ends within about
/// 100 ms of the release, found by the actor's bounded button-state poll (no
/// notification follows the release).
#[test]
fn a_missed_up_ends_the_gesture_within_about_100_ms_of_the_release() {
    let (world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let node = nodes[0];
    let cell = world.frame_of(node);
    buttons.press();
    world.down(crate::drag::center(&cell));
    let to = Rect::new(cell.x + 120.0, cell.y + 80.0, cell.width, cell.height);
    world.held_steps(
        node,
        to,
        6,
        Duration::from_millis(16),
        AxNotification::WindowMoved,
    );
    std::thread::sleep(Duration::from_millis(120));
    assert!(world.runtime.snapshot().drag.active);
    let released = Instant::now();
    buttons.release();
    while world.runtime.snapshot().drag.active {
        assert!(
            released.elapsed() < Duration::from_secs(2),
            "the gesture never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let ended = released.elapsed();
    assert!(
        ended <= Duration::from_millis(150),
        "ended {ended:?} after the release"
    );
    assert_eq!(world.last_outcome(), Some(DragOutcome::SnappedBack));
    world.assert_back_in_layout(&nodes);
    let status = world.drag_status();
    assert_eq!(
        (
            status.from_down,
            status.downs,
            status.ups,
            status.released_by_state
        ),
        (1, 1, 0, 1),
        "{status:?}"
    );
}

/// The live `terminal` workspace: `split_horizontal` with two applications
/// (Ghostty and Device Hub). The user widens the left window with a silent
/// tap; it keeps the new size and its neighbour takes the rest.
#[test]
fn a_two_app_split_horizontal_resize_keeps_its_size_and_the_neighbour_adjusts() {
    let (world, buttons) = booted_with_buttons(&[1, 1], layout_config("split_horizontal", 14));
    let (ghostty, hub) = (world.nodes(0)[0], world.nodes(1)[0]);
    let nodes = [ghostty, hub];
    let (left, right) = (world.frame_of(ghostty), world.frame_of(hub));
    let to = Rect::new(left.x, left.y, left.width + 250.0, left.height);
    let before = world.frame_writes(&nodes);
    buttons.press();
    world.held_steps(
        ghostty,
        to,
        30,
        Duration::from_millis(16),
        AxNotification::WindowResized,
    );
    assert_eq!(
        world.frame_writes(&nodes),
        before,
        "a write while the button is held"
    );
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(
            world.last_outcome(),
            Some(DragOutcome::Resized { changed: true, .. })
        ),
        "{:?}\nleft {left:?} right {right:?} to {to:?}\n{:?}\n{:#?}",
        world.last_outcome(),
        world.drag_status(),
        world.runtime.snapshot().frames
    );
    world.assert_back_in_layout(&nodes);
    let (left_now, right_now) = (world.frame_of(ghostty), world.frame_of(hub));
    assert!(close(left_now.width, left.width + 250.0), "{left_now:?}");
    assert!(close(right_now.x, right.x + 250.0), "{right_now:?}");
    assert!(close(right_now.width, right.width - 250.0), "{right_now:?}");
}

/// No gesture, no timer: the button state is read only for a geometry
/// observation or while a gesture waits for its up, never polled idle.
#[test]
fn nothing_polls_the_button_state_while_no_gesture_exists() {
    let (world, buttons) = booted_with_buttons(&[3], grid());
    let reads = buttons.reads();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(buttons.reads(), reads, "the button state was polled while idle");
    // A gesture polls while held, and stops once it ended.
    let nodes = world.nodes(0).to_vec();
    let cell = world.frame_of(nodes[0]);
    buttons.press();
    world.held_steps(
        nodes[0],
        Rect::new(cell.x + 100.0, cell.y, cell.width, cell.height),
        3,
        Duration::from_millis(16),
        AxNotification::WindowMoved,
    );
    wait_until("the gesture", || world.runtime.snapshot().drag.active);
    let held = buttons.reads();
    std::thread::sleep(Duration::from_millis(160));
    assert!(buttons.reads() >= held + 2, "no poll while the gesture is held");
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    world.assert_back_in_layout(&nodes);
    let after = buttons.reads();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(buttons.reads(), after, "the poll outlived the gesture");
}

/// `available` follows the live tap: a tap macOS disabled delivers nothing.
#[test]
fn status_reports_the_live_tap_and_a_dead_tap_is_unavailable() {
    let (world, _buttons) = booted_with_buttons(&[1], grid());
    let snap = world.runtime.snapshot();
    let ready = crate::ipc::status(&snap, None, Some((true, "ready".to_owned()))).drag;
    assert!(ready.available && ready.unavailable_reason.is_none());
    assert_eq!(ready.tap.as_deref(), Some("ready"));
    let dead = crate::ipc::status(
        &snap,
        None,
        Some((false, "re-enable failed after 3 attempts (Cancelled)".to_owned())),
    )
    .drag;
    assert!(!dead.available, "{dead:?}");
    let reason = dead.unavailable_reason.expect("reason");
    assert!(
        reason.contains("re-enable failed after 3 attempts") && reason.contains("window geometry"),
        "{reason}"
    );
}

// ------------------------------------- held buttons and our own motion

fn grid_gaps(outer: u32) -> Arc<TilingConfig> {
    config(
        outer,
        &json!([{ "name": "files", "layout": "grid", "screen": "main" }]),
    )
}

/// [`grid_gaps`] with animations off: every write is a direct one.
fn grid_direct(outer: u32) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "enabled": true,
            "gaps": { "inner": 0, "outer": outer },
            "animations": { "enabled": false },
            "workspaces": [{ "name": "files", "layout": "grid", "screen": "main" }],
        }))
        .expect("config"),
    )
}

impl World {
    /// Native frame writes to `nodes` from call index `from` on.
    fn frame_writes_since(&self, nodes: &[NodeId], from: usize) -> usize {
        self.ax.calls()[from..]
            .iter()
            .filter(|c| {
                matches!(c, Call::SetPosition(node, _) | Call::SetSize(node, _) if nodes.contains(node))
            })
            .count()
    }
}

/// The user grabs a window while our relayout animates it and the tap saw no
/// down: the first read off the animation's path starts the gesture, the
/// animation stops, nothing writes the workspace while the button is held,
/// and the commit measures from the animation's target (a move inside the
/// new cell snaps back, never a resize from the old cell).
#[test]
fn a_window_grabbed_mid_animation_with_a_silent_tap_is_not_fought() {
    let (mut world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let node = nodes[0];
    let token = world.token(node);
    let base = world.spy.batches().len();
    world.reload(grid_gaps(200));
    wait_until("an intermediate frame of the window", || {
        world
            .spy
            .frames_of(base)
            .iter()
            .any(|(_, w, s)| *w == token && *s == FrameStage::Intermediate)
    });
    let target = world.desired()[&token];
    buttons.press();
    // Intermediate frames move the window only: a narrower window is off the
    // animation's path whatever its position.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !world.runtime.snapshot().drag.active {
        assert!(Instant::now() < deadline, "the grab never started a gesture");
        let at = world.frame_of(node);
        world.user_step(
            node,
            Rect::new(at.x, at.y, target.width - 300.0, target.height),
            AxNotification::WindowResized,
        );
    }
    let calls = world.ax.calls().len();
    world.held_steps(
        node,
        Rect::new(target.x + 40.0, target.y + 30.0, target.width, target.height),
        6,
        Duration::from_millis(16),
        AxNotification::WindowMoved,
    );
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        world.frame_writes_since(&nodes, calls),
        0,
        "a write while the button is held"
    );
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    assert_eq!(world.last_outcome(), Some(DragOutcome::SnappedBack));
    world.assert_back_in_layout(&nodes);
    assert_eq!(world.frame_of(node), target);
}

/// A terminal-like sibling snaps to its character grid a moment after each of
/// our writes, so its real frame is larger than our target and than the
/// readback we recorded. A silent-tap drag onto it swaps: the sibling's
/// baseline is where it really sits, so it never reads as resized.
#[test]
fn a_drop_onto_a_grid_snapping_sibling_swaps_not_resizes() {
    let (mut world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let (a, b) = (nodes[0], nodes[1]);
    let pid = world.applications[0].info.pid;
    let (ax, observers, all) = (world.ax.clone(), world.observers.clone(), nodes.clone());
    world.ax.on_node_write(move |node| {
        if all.contains(&node) {
            observers.inject(pid, AxNotification::WindowMoved, node);
        }
        if node != b {
            return;
        }
        let ax = ax.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            ax.edit(b, |n| {
                if let Ok(size) = n.size.as_mut() {
                    // 23-point rows: the 514.5 cell reads 529.
                    size.height = (size.height / 23.0).ceil() * 23.0;
                }
            });
        });
    });
    world.reload(grid_gaps(13));
    // Settled: the terminal's late resize notifications are delivered and
    // nothing is written for a while.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let calls = world.ax.calls().len();
        std::thread::sleep(Duration::from_millis(60));
        world.notify(b, AxNotification::WindowResized);
        std::thread::sleep(Duration::from_millis(300));
        if world.frame_writes_since(&nodes, calls) == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the layout never settled");
    }
    let (cell_a, cell_b) = (world.frame_of(a), world.frame_of(b));
    let desired_b = world.desired()[&world.token(b)];
    assert!(
        cell_b.height > desired_b.height + 5.0,
        "the sibling sits on its grid: {cell_b:?} vs {desired_b:?}"
    );
    let centre_b = crate::drag::center(&cell_b);
    let to = Rect::new(
        centre_b.x - cell_a.width / 2.0,
        centre_b.y - cell_a.height / 2.0,
        cell_a.width,
        cell_a.height,
    );
    buttons.press();
    world.held_steps(a, to, 6, Duration::from_millis(16), AxNotification::WindowMoved);
    wait_until("the gesture", || world.runtime.snapshot().drag.active);
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
    assert!(
        matches!(world.last_outcome(), Some(DragOutcome::Swapped { .. })),
        "{:?}",
        world.last_outcome()
    );
}

/// A button held through a whole relayout (a text selection, say) while late
/// settle echoes of the finished animation arrive: our own motion never
/// starts a gesture.
#[test]
fn a_held_button_during_a_relayout_with_late_echoes_starts_no_gesture() {
    let (mut world, buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let node = nodes[0];
    let token = world.token(node);
    let from = world.frame_of(node);
    buttons.press();
    world.reload(grid_gaps(200));
    wait_until("the relayout", || world.desired()[&token] != from);
    world.wait_settled("the relayout settled");
    let target = world.desired()[&token];
    // A late read shows a frame from the motion, then the settled one again.
    let midway = Rect::new(
        f64::midpoint(from.x, target.x),
        f64::midpoint(from.y, target.y),
        target.width,
        target.height,
    );
    world.user_step(node, midway, AxNotification::WindowMoved);
    std::thread::sleep(Duration::from_millis(40));
    world.user_step(node, target, AxNotification::WindowMoved);
    std::thread::sleep(Duration::from_millis(100));
    let status = world.drag_status();
    assert_eq!(status.gestures, 0, "{status:?}");
    assert!(!world.runtime.snapshot().drag.active);
    buttons.release();
}

/// A gesture cancels the queued direct (not animated) writes of the windows it
/// captures: a write still waiting in its application's lane never runs.
#[test]
fn a_queued_direct_write_is_cancelled_when_a_gesture_begins() {
    let buttons = Arc::new(FakeButtons::default());
    BOOT_BUTTONS.with(|slot| {
        *slot.borrow_mut() = Some(Arc::clone(&buttons) as Arc<dyn crate::drag::ButtonState>);
    });
    let mut world = World::boot(&[1, 2], grid_direct(0));
    let a = world.nodes(0)[0];
    let (b, c) = (world.nodes(1)[0], world.nodes(1)[1]);
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    let pid = world.applications[0].info.pid;
    let observers = world.observers.clone();
    world.ax.on_node_write(move |node| {
        if node == a {
            observers.inject(pid, AxNotification::WindowMoved, node);
        }
        if node == b {
            held.pass();
        }
    });
    world.reload(grid_direct(120));
    wait_until("the first write of the slow application is held", || {
        gate.entered() >= 1
    });
    wait_until("the other application settled", || world.all_at_target(&[a]));
    let cell = world.frame_of(a);
    buttons.press();
    world.held_steps(
        a,
        Rect::new(cell.x + 60.0, cell.y + 40.0, cell.width, cell.height),
        3,
        Duration::from_millis(16),
        AxNotification::WindowMoved,
    );
    wait_until("the gesture", || world.runtime.snapshot().drag.active);
    let calls = world.ax.calls().len();
    gate.release();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        world.frame_writes_since(&[c], calls),
        0,
        "the queued write ran after the gesture began"
    );
    buttons.release();
    wait_until("the commit", || world.last_outcome().is_some());
}

/// A quick click the tap delivers: the button state already reads up when the
/// down is applied, but the tap's own up ends the gesture, not the poll.
#[test]
fn a_quick_click_ends_by_the_taps_up_not_the_button_state() {
    let (world, _buttons) = booted_with_buttons(&[3], grid());
    let nodes = world.nodes(0).to_vec();
    let centre = crate::drag::center(&world.frame_of(nodes[0]));
    world.down(centre);
    std::thread::sleep(Duration::from_millis(20));
    world.up(centre);
    wait_until("the commit", || world.last_outcome().is_some());
    let status = world.drag_status();
    assert_eq!((status.ups, status.released_by_state), (1, 0), "{status:?}");
}
