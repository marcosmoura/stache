//! Reader, rendering and owner tests over the fakes. No real AX call is made.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use stache_sdk::CancellationToken;

use super::testing::{FakeAx, FakeClock, FakeElement, FakeValidator, Node, Op, fake_target};
use super::*;

const OWN_PID: i32 = 1;
const TARGET_PID: i32 = 4242;

struct Rig {
    ax: FakeAx,
    clock: Arc<FakeClock>,
    validator: Arc<FakeValidator>,
    cancel: CancellationToken,
    target: MenuTarget,
}

impl Rig {
    fn new(ax: FakeAx) -> Self {
        Self {
            ax,
            clock: Arc::new(FakeClock::default()),
            validator: Arc::new(FakeValidator::always(TargetCheck::Current)),
            cancel: CancellationToken::new(),
            target: fake_target(TARGET_PID),
        }
    }

    fn with_bar(items: Vec<Node>) -> Self { Self::new(FakeAx::new(items)) }

    fn read(&self) -> Result<ReadSuccess<FakeElement>, MenuReadOutcome> {
        self.read_target(Some(self.target))
    }

    fn read_target(
        &self,
        target: Option<MenuTarget>,
    ) -> Result<ReadSuccess<FakeElement>, MenuReadOutcome> {
        let context = ReadContext {
            clock: self.clock.as_ref(),
            cancel: &self.cancel,
            validator: self.validator.as_ref(),
            own_pid: OWN_PID,
            request_deadline: None,
        };
        read_menu(&self.ax, &context, target.as_ref(), PresentationId::allocate())
    }

    /// Expects a failure and proves every element was released.
    fn fail(&self) -> MenuReadOutcome {
        let outcome = self.read().err().expect("read must fail");
        assert_eq!(self.ax.counts().live(), 0, "failure must release everything");
        outcome
    }

    fn snapshot(&self) -> MenuSnapshot { self.read().expect("read").snapshot }
}

fn apple() -> Node { Node::submenu("Apple", vec![Node::item("About This App")]).tag("apple") }

fn bar(items: Vec<Node>) -> Vec<Node> {
    let mut all = vec![apple()];
    all.extend(items);
    all
}

fn nested(levels: usize) -> Node {
    if levels <= 1 {
        Node::item("leaf")
    } else {
        Node::submenu(&format!("L{levels}"), vec![nested(levels - 1)])
    }
}

fn titles(items: &[MenuItemSnapshot]) -> Vec<&str> {
    items.iter().map(|item| item.title.as_str()).collect()
}

fn submenu(item: &MenuItemSnapshot) -> &SubmenuSnapshot {
    match &item.kind {
        MenuItemKind::Submenu(menu) => menu,
        other => panic!("expected a submenu, got {other:?}"),
    }
}

// ---- MA-02-03: rendering rules -------------------------------------------

#[test]
fn apple_menu_is_skipped_at_the_root_only() {
    let rig = Rig::with_bar(bar(vec![Node::submenu("File", vec![
        Node::item("New"),
        Node::item("Open"),
    ])]));
    let snapshot = rig.snapshot();
    assert_eq!(titles(&snapshot.items), ["File"]);
    // The first child of a submenu is NOT skipped.
    assert_eq!(titles(&submenu(&snapshot.items[0]).items), ["New", "Open"]);
    let apple = rig.ax.node("apple");
    assert!(
        rig.ax.calls().iter().all(|call| call.node != apple),
        "the skipped Apple menu is never messaged"
    );
}

#[test]
fn empty_missing_and_role_separators_are_separators() {
    let titled_separator = Node {
        role: Some("AXSeparator".to_owned()),
        ..Node::item("Titled")
    };
    let rig = Rig::with_bar(bar(vec![Node::submenu("Edit", vec![
        Node::item("Cut"),
        Node::item(""),
        Node::item("NoTitle").without_title(),
        titled_separator,
        Node::item("NoRole").without_role(),
    ])]));
    let items = rig.snapshot().items;
    let edit = &submenu(&items[0]).items;
    assert!(matches!(edit[0].kind, MenuItemKind::Item));
    assert!(matches!(edit[1].kind, MenuItemKind::Separator), "empty title");
    assert!(matches!(edit[2].kind, MenuItemKind::Separator), "missing title");
    assert!(
        matches!(edit[3].kind, MenuItemKind::Separator),
        "AXSeparator role"
    );
    assert!(
        matches!(edit[4].kind, MenuItemKind::Item),
        "missing role is an item"
    );
    assert!(edit[1..4].iter().all(|item| item.action.is_none()));
}

#[test]
fn first_nonseparator_root_item_is_bold_and_nothing_else() {
    let rig = Rig::with_bar(bar(vec![
        Node::separator(),
        Node::submenu("App", vec![Node::item("Inner")]),
        Node::item("Second"),
    ]));
    let items = rig.snapshot().items;
    assert!(matches!(items[0].kind, MenuItemKind::Separator));
    assert!(!items[0].bold);
    assert!(items[1].bold, "first nonseparator root item");
    assert!(!items[2].bold);
    assert!(!submenu(&items[1]).items[0].bold, "submenu items are never bold");
}

#[test]
fn submenu_title_inherits_the_parent_title() {
    let rig = Rig::with_bar(bar(vec![Node::submenu("View", vec![Node::submenu(
        "Zoom",
        vec![Node::item("In")],
    )])]));
    let items = rig.snapshot().items;
    let view = submenu(&items[0]);
    assert_eq!(view.title, "View");
    assert_eq!(submenu(&view.items[0]).title, "Zoom");
}

#[test]
fn mark_characters_map_to_on_mixed_and_off() {
    let rig = Rig::with_bar(bar(vec![Node::submenu("M", vec![
        Node::item("on").mark("✓"),
        Node::item("mixed").mark("•"),
        Node::item("other").mark("x"),
        Node::item("empty").mark(""),
        Node::item("absent"),
    ])]));
    let items = rig.snapshot().items;
    let marks: Vec<_> = submenu(&items[0]).items.iter().map(|item| item.mark).collect();
    assert_eq!(marks, [
        MarkState::On,
        MarkState::Mixed,
        MarkState::Off,
        MarkState::Off,
        MarkState::Off
    ]);
}

#[test]
fn enabled_defaults_to_true_when_the_attribute_is_missing() {
    let rig = Rig::with_bar(bar(vec![Node::submenu("M", vec![
        Node::item("default"),
        Node::item("off").disabled(),
    ])]));
    let items = rig.snapshot().items;
    let menu = &submenu(&items[0]).items;
    assert!(menu[0].enabled);
    assert!(!menu[1].enabled);
}

#[test]
fn modifier_masks_follow_the_header() {
    let set = |shift, option, control, command| ModifierSet {
        shift,
        option,
        control,
        command,
    };
    let table = [
        (None, set(false, false, false, true)),
        (Some(0), set(false, false, false, true)),
        (Some(1), set(true, false, false, true)),
        (Some(2), set(false, true, false, true)),
        (Some(4), set(false, false, true, true)),
        (Some(8), set(false, false, false, false)),
        (Some(9), set(true, false, false, false)),
        (Some(10), set(false, true, false, false)),
        (Some(12), set(false, false, true, false)),
        (Some(15), set(true, true, true, false)),
        (Some(7), set(true, true, true, true)),
    ];
    for (raw, expected) in table {
        assert_eq!(ModifierSet::from_ax(raw), expected, "mask {raw:?}");
    }
}

#[test]
fn key_equivalents_are_lowercased_keep_unicode_and_read_modifiers_lazily() {
    let rig = Rig::with_bar(bar(vec![Node::submenu("M", vec![
        Node::item("open").key("O", Some(1)),
        Node::item("umlaut").key("Ö", None),
        Node::item("arrow").key("→", Some(8)),
        Node::item("none").key("", Some(2)),
        Node::item("plain"),
    ])]));
    let items = rig.snapshot().items;
    let menu = &submenu(&items[0]).items;
    let key = |index: usize| menu[index].key_equivalent.clone();
    assert_eq!(
        key(0),
        Some(KeyEquivalent {
            key: "o".to_owned(),
            modifiers: ModifierSet {
                shift: true,
                option: false,
                control: false,
                command: true
            },
        })
    );
    let umlaut = key(1).expect("umlaut");
    assert_eq!(umlaut.key, "ö");
    assert!(umlaut.modifiers.command, "missing modifiers default to Command");
    let arrow = key(2).expect("arrow");
    assert_eq!(arrow.key, "→");
    assert!(!arrow.modifiers.command, "NoCommand removes Command");
    assert!(
        key(3).is_none() && key(4).is_none(),
        "empty or missing key has no shortcut"
    );
    assert_eq!(
        rig.ax.ops(Op::CmdModifiers).len(),
        3,
        "modifiers are read only for items with a command character"
    );
}

#[test]
fn only_enabled_leaves_get_tokens_and_submenu_parents_never_do() {
    let rig = Rig::with_bar(bar(vec![
        Node::item("Top leaf"),
        Node::submenu("Parent", vec![Node::item("Child"), Node::item("Off").disabled()]),
        Node::submenu("Disabled parent", vec![Node::item("Grandchild")]).disabled(),
        Node::separator(),
    ]));
    let success = rig.read().expect("read");
    let items = &success.snapshot.items;
    assert!(items[0].action.is_some(), "enabled top-level leaf");
    assert!(items[1].action.is_none(), "enabled submenu parent");
    assert!(submenu(&items[1]).items[0].action.is_some());
    assert!(submenu(&items[1]).items[1].action.is_none(), "disabled leaf");
    assert!(items[2].action.is_none(), "disabled submenu parent");
    assert!(!items[2].enabled);
    assert!(
        submenu(&items[2]).items[0].action.is_some(),
        "its enabled child stays actionable"
    );
    assert!(items[3].action.is_none(), "separator");

    let tokens: Vec<ActionToken> = success.retained.iter().map(|(token, _)| *token).collect();
    assert_eq!(tokens.len(), 3, "exactly the enabled leaves are retained");
    let mut unique = tokens.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3, "tokens are unique");
    assert!(tokens.iter().all(|token| token.presentation() == success.snapshot.presentation));
}

#[test]
fn the_first_axmenu_child_is_the_submenu() {
    let group = Node {
        role: Some("AXGroup".to_owned()),
        ..Node::default()
    };
    let item = Node::item("Item")
        .child(group)
        .child(Node::menu(vec![Node::item("first menu")]))
        .child(Node::menu(vec![Node::item("second menu")]));
    let rig = Rig::with_bar(bar(vec![item]));
    let items = rig.snapshot().items;
    assert_eq!(titles(&submenu(&items[0]).items), ["first menu"]);
}

#[test]
fn an_empty_axmenu_is_an_ordinary_leaf_pressable_only_when_enabled_like_legacy() {
    let rig = Rig::with_bar(bar(vec![Node::item("Lazy").child(Node::menu(Vec::new()))]));
    let success = rig.read().expect("read");
    let item = &success.snapshot.items[0];
    assert!(matches!(item.kind, MenuItemKind::Item), "no submenu is rendered");
    assert!(item.action.is_some(), "an enabled leaf carries an action token");
    assert_eq!(success.retained.len(), 1);
}

#[test]
fn a_disabled_item_with_an_empty_axmenu_is_a_leaf_without_an_action() {
    let rig = Rig::with_bar(bar(vec![
        Node::item("Lazy").disabled().child(Node::menu(Vec::new())),
    ]));
    let success = rig.read().expect("read");
    let item = &success.snapshot.items[0];
    assert!(matches!(item.kind, MenuItemKind::Item));
    assert!(item.action.is_none());
    assert!(success.retained.is_empty());
}

// ---- MA-02-01: retain/release balance -------------------------------------

#[test]
fn success_retains_only_actionable_leaves_and_balances_on_drop() {
    let rig = Rig::with_bar(bar(vec![
        Node::submenu("A", vec![Node::item("one"), Node::item("two")]),
        Node::item("B"),
    ]));
    let success = rig.read().expect("read");
    assert_eq!(success.retained.len(), 3);
    let counts = rig.ax.counts();
    assert_eq!(counts.live(), 3, "only the actionable leaves stay retained");
    assert!(counts.created > 3, "everything else was created and released");
    drop(success);
    assert_eq!(rig.ax.counts().live(), 0);
    assert_eq!(rig.ax.counts().created, rig.ax.counts().released);
}

#[test]
fn partial_failure_after_retaining_leaves_releases_everything() {
    let rig = Rig::with_bar(bar(vec![
        Node::item("early leaf"),
        Node::item("late").tag("late"),
    ]));
    let late = rig.ax.node("late");
    rig.ax.inject(Op::Title, Some(late), AxError::CannotComplete);
    assert_eq!(rig.fail(), MenuReadOutcome::Transport);
}

#[test]
fn discarded_stale_result_releases_everything() {
    let rig = Rig::with_bar(bar(vec![Node::item("leaf")]));
    let validator = Arc::new(FakeValidator::scripted(vec![
        TargetCheck::Current,
        TargetCheck::Stale,
    ]));
    let rig = Rig { validator, ..rig };
    assert_eq!(rig.fail(), MenuReadOutcome::StaleTarget);
}

// ---- MA-02-02: budgets -------------------------------------------------------

#[test]
fn every_element_gets_its_timeout_before_it_is_messaged() {
    let rig = Rig::with_bar(bar(vec![
        Node::submenu("File", vec![
            Node::item("Open").tag("open"),
            Node::submenu("Recent", vec![Node::item("x")]),
        ])
        .tag("file"),
    ]));
    let _success = rig.read().expect("read");
    assert!(
        rig.ax.timeout_violations().is_empty(),
        "{:?}",
        rig.ax.timeout_violations()
    );
    let set = rig.ax.timeout_nodes();
    for node in [
        0,
        rig.ax.node("menubar"),
        rig.ax.node("file"),
        rig.ax.node("open"),
    ] {
        assert!(set.contains(&node), "node {node} needs a timeout");
    }
    let messaged = rig
        .ax
        .calls()
        .iter()
        .filter(|call| !matches!(call.op, Op::CreateApp | Op::SetTimeout))
        .map(|call| call.node)
        .collect::<std::collections::HashSet<_>>();
    for node in messaged {
        assert!(set.contains(&node), "messaged node {node} had no timeout");
    }
}

#[test]
fn a_timeout_setting_failure_aborts_without_further_reads() {
    let rig = Rig::with_bar(bar(vec![Node::item("File").tag("file")]));
    rig.ax.inject(Op::SetTimeout, Some(rig.ax.node("file")), AxError::Other(-25200));
    assert_eq!(rig.fail(), MenuReadOutcome::TimeoutSetFailed);
    let calls = rig.ax.calls();
    let last = calls.last().expect("calls");
    assert_eq!(
        last.op,
        Op::SetTimeout,
        "nothing was messaged after the failed timeout"
    );
}

#[test]
fn the_cumulative_deadline_stops_the_traversal_mid_way() {
    let rig = Rig::with_bar(bar(vec![
        Node::submenu("A", vec![Node::item("1"), Node::item("2")]),
        Node::submenu("B", vec![Node::item("3"), Node::item("4")]),
    ]));
    let clock = Arc::clone(&rig.clock);
    rig.ax.set_hook(move |call| {
        if call.index == 9 {
            clock.advance(READ_DEADLINE);
        }
    });
    assert_eq!(rig.fail(), MenuReadOutcome::DeadlineExceeded);
    assert_eq!(
        rig.ax.call_count(),
        10,
        "no AX call is issued after the deadline"
    );
}

#[test]
fn the_deadline_is_cumulative_and_started_before_the_first_read() {
    let rig = Rig::with_bar(bar(vec![Node::item("A"), Node::item("B")]));
    let clock = Arc::clone(&rig.clock);
    // Each call takes 400 ms; the 4th call crosses 1.5 s cumulatively.
    rig.ax.set_hook(move |_| clock.advance(Duration::from_millis(400)));
    assert_eq!(rig.fail(), MenuReadOutcome::DeadlineExceeded);
    assert_eq!(rig.ax.call_count(), 4);
}

#[test]
fn a_request_deadline_tightens_the_budget() {
    let rig = Rig::with_bar(bar(vec![Node::item("A")]));
    let context = ReadContext {
        clock: rig.clock.as_ref(),
        cancel: &rig.cancel,
        validator: rig.validator.as_ref(),
        own_pid: OWN_PID,
        request_deadline: Some(rig.clock.now() + Duration::from_millis(100)),
    };
    let clock = Arc::clone(&rig.clock);
    rig.ax.set_hook(move |call| {
        if call.index == 2 {
            clock.advance(Duration::from_millis(200));
        }
    });
    let outcome = read_menu(&rig.ax, &context, Some(&rig.target), PresentationId::allocate());
    assert_eq!(outcome.err(), Some(MenuReadOutcome::DeadlineExceeded));
    assert_eq!(rig.ax.call_count(), 3);
    assert_eq!(rig.ax.counts().live(), 0);
}

#[test]
fn cancellation_between_attributes_stops_all_further_reads() {
    let rig = Rig::with_bar(bar(vec![Node::item("File"), Node::item("Edit")]));
    let cancel = rig.cancel.clone();
    rig.ax.set_hook(move |call| {
        if call.op == Op::Title {
            cancel.cancel();
        }
    });
    assert_eq!(rig.fail(), MenuReadOutcome::Cancelled);
    let last = rig.ax.calls().last().expect("calls").clone();
    assert_eq!(last.op, Op::Title, "no read is issued after cancellation");
}

#[test]
fn cancellation_before_the_first_read_issues_no_ax_call() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    rig.cancel.cancel();
    assert_eq!(rig.fail(), MenuReadOutcome::Cancelled);
    assert_eq!(rig.ax.call_count(), 0);
    assert_eq!(rig.validator.calls(), 0);
}

#[test]
fn cancellation_wins_over_an_expired_deadline() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    let clock = Arc::clone(&rig.clock);
    let cancel = rig.cancel.clone();
    rig.ax.set_hook(move |_| {
        clock.advance(READ_DEADLINE);
        cancel.cancel();
    });
    assert_eq!(rig.fail(), MenuReadOutcome::Cancelled);
}

#[test]
fn the_node_cap_discards_everything() {
    let menus = (0..50)
        .map(|n| {
            Node::submenu(
                &format!("M{n}"),
                (0..100).map(|i| Node::item(&format!("i{i}"))).collect(),
            )
        })
        .collect();
    let rig = Rig::with_bar(bar(menus));
    assert_eq!(rig.fail(), MenuReadOutcome::NodeLimit);
}

#[test]
fn an_oversized_child_array_is_rejected_before_it_is_copied() {
    let items: Vec<Node> = (0..=MAX_NODES).map(|i| Node::item(&format!("i{i}"))).collect();
    let rig = Rig::with_bar(bar(items));
    assert_eq!(rig.fail(), MenuReadOutcome::NodeLimit);
    assert_eq!(rig.ax.ops(Op::Children).len(), 0, "the array was never copied");
    assert_eq!(rig.ax.ops(Op::ChildCount).len(), 1);
}

#[test]
fn depth_sixteen_is_allowed_and_seventeen_is_rejected() {
    let ok = Rig::with_bar(bar(vec![nested(8)]));
    assert!(ok.read().is_ok(), "an item at depth 16 is allowed");
    let too_deep = Rig::with_bar(bar(vec![nested(9)]));
    assert_eq!(too_deep.fail(), MenuReadOutcome::DepthLimit);
}

#[test]
fn a_repeated_ancestor_is_a_cycle() {
    let rig = Rig::with_bar(bar(vec![
        Node::submenu("A", vec![Node::item("B").tag("b")]).tag("a"),
    ]));
    // B's children now contain its own grandparent item A.
    rig.ax.link_child(rig.ax.node("b"), rig.ax.node("a"));
    assert_eq!(rig.fail(), MenuReadOutcome::Cycle);

    let self_loop = Rig::with_bar(bar(vec![Node::item("S").tag("s")]));
    self_loop.ax.link_child(self_loop.ax.node("s"), self_loop.ax.node("s"));
    assert_eq!(self_loop.fail(), MenuReadOutcome::Cycle);
}

#[test]
fn transport_failure_is_not_a_missing_optional_value() {
    // Unsupported optional attribute: legacy defaults, read succeeds.
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    rig.ax.inject(Op::Enabled, None, AxError::AttributeUnsupported);
    rig.ax.inject(Op::MarkChar, None, AxError::NoValue);
    assert!(rig.snapshot().items[0].enabled);

    // The same attribute failing in transport aborts.
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    rig.ax.inject(Op::Enabled, None, AxError::CannotComplete);
    assert_eq!(rig.fail(), MenuReadOutcome::Transport);
}

#[test]
fn cannot_complete_after_the_element_timeout_is_a_timeout() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    let clock = Arc::clone(&rig.clock);
    rig.ax.inject(Op::Title, None, AxError::CannotComplete);
    rig.ax.set_hook(move |call| {
        if call.op == Op::Title {
            clock.advance(ELEMENT_TIMEOUT);
        }
    });
    assert_eq!(rig.fail(), MenuReadOutcome::ElementTimeout);
}

#[test]
fn structural_and_permission_errors_are_distinct_typed_outcomes() {
    let cases = [
        (
            Op::MenuBar,
            AxError::ApiDisabled,
            MenuReadOutcome::PermissionDenied,
        ),
        (Op::MenuBar, AxError::NoValue, MenuReadOutcome::NoMenu),
        (
            Op::MenuBar,
            AxError::AttributeUnsupported,
            MenuReadOutcome::NoMenu,
        ),
        (
            Op::MenuBar,
            AxError::NotImplemented,
            MenuReadOutcome::Unsupported,
        ),
        (Op::MenuBar, AxError::WrongType, MenuReadOutcome::Malformed),
        (Op::Role, AxError::WrongType, MenuReadOutcome::Malformed),
        (Op::Title, AxError::InvalidElement, MenuReadOutcome::Malformed),
        (Op::Children, AxError::WrongType, MenuReadOutcome::Malformed),
        (
            Op::ChildCount,
            AxError::ApiDisabled,
            MenuReadOutcome::PermissionDenied,
        ),
        (
            Op::CreateApp,
            AxError::InvalidElement,
            MenuReadOutcome::Unsupported,
        ),
    ];
    for (op, error, expected) in cases {
        let rig = Rig::with_bar(bar(vec![Node::item("File")]));
        rig.ax.inject(op, None, error);
        assert_eq!(rig.fail(), expected, "{op:?} {error:?}");
    }
}

#[test]
fn a_missing_or_empty_menu_bar_is_no_menu() {
    let rig = Rig::new(FakeAx::without_menu_bar());
    assert_eq!(rig.fail(), MenuReadOutcome::NoMenu);
    let only_apple = Rig::with_bar(vec![apple()]);
    assert_eq!(only_apple.fail(), MenuReadOutcome::NoMenu);
    let empty = Rig::with_bar(Vec::new());
    assert_eq!(empty.fail(), MenuReadOutcome::NoMenu);
}

// ---- Target identity ---------------------------------------------------------

#[test]
fn self_and_missing_targets_never_touch_ax() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    let own = MenuTarget { pid: OWN_PID, ..rig.target };
    assert_eq!(
        rig.read_target(Some(own)).err(),
        Some(MenuReadOutcome::SelfTarget)
    );
    assert_eq!(rig.read_target(None).err(), Some(MenuReadOutcome::NoFrontmost));
    assert_eq!(rig.ax.call_count(), 0);
    assert_eq!(rig.validator.calls(), 0);
}

#[test]
fn a_stale_target_before_the_read_creates_no_root() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    let rig = Rig {
        validator: Arc::new(FakeValidator::always(TargetCheck::Stale)),
        ..rig
    };
    assert_eq!(rig.fail(), MenuReadOutcome::StaleTarget);
    assert_eq!(rig.ax.call_count(), 0);
}

#[test]
fn an_unavailable_validation_is_a_dispatch_failure() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    let rig = Rig {
        validator: Arc::new(FakeValidator::always(TargetCheck::Unavailable)),
        ..rig
    };
    assert_eq!(rig.fail(), MenuReadOutcome::DispatchFailure);
}

#[test]
fn identity_is_validated_before_and_after_the_read() {
    let rig = Rig::with_bar(bar(vec![Node::item("File")]));
    let _ = rig.read().expect("read");
    assert_eq!(rig.validator.calls(), 2);
}

// ---- Owner thread ------------------------------------------------------------

struct OwnerRig {
    owner: AxOwner,
    ax: FakeAx,
    cancel: CancellationToken,
    target: MenuTarget,
    clock: Arc<FakeClock>,
}

fn owner_rig(items: Vec<Node>) -> OwnerRig {
    let ax = FakeAx::new(items);
    let clock = Arc::new(FakeClock::default());
    let config = OwnerConfig {
        clock: Arc::clone(&clock) as Arc<dyn MenuClock>,
        validator: Arc::new(FakeValidator::always(TargetCheck::Current)),
        own_pid: OWN_PID,
    };
    let factory_ax = ax.clone();
    let owner = AxOwner::new(move || factory_ax, config);
    OwnerRig {
        owner,
        ax,
        cancel: CancellationToken::new(),
        target: fake_target(TARGET_PID),
        clock,
    }
}

impl OwnerRig {
    fn request(&self, presentation: PresentationId) -> ReadRequest {
        ReadRequest {
            target: Some(self.target),
            presentation,
            cancel: self.cancel.clone(),
            deadline: None,
        }
    }

    fn read(&self, presentation: PresentationId) -> MenuReadOutcome {
        let (tx, rx) = mpsc::channel();
        self.owner
            .submit_read(self.request(presentation), move |outcome| {
                let _ = tx.send(outcome);
            })
            .expect("queued");
        rx.recv_timeout(Duration::from_secs(5)).expect("reply")
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("condition never became true");
}

#[test]
fn the_owner_runs_every_ax_call_on_its_dedicated_thread_and_replies_pure_data() {
    let rig = owner_rig(bar(vec![Node::submenu("File", vec![Node::item("Open")])]));
    let presentation = PresentationId::allocate();
    let MenuReadOutcome::Snapshot(snapshot) = rig.read(presentation) else {
        panic!("expected a snapshot");
    };
    assert_eq!(snapshot.presentation, presentation);
    let calls = rig.ax.calls();
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|call| call.thread == OWNER_THREAD_NAME),
        "{calls:?}"
    );
    assert_eq!(
        rig.ax.counts().live(),
        1,
        "the actionable leaf is retained on the owner"
    );
}

#[test]
fn release_drops_retained_elements_exactly_once() {
    let rig = owner_rig(bar(vec![Node::item("A"), Node::item("B")]));
    let presentation = PresentationId::allocate();
    assert!(matches!(rig.read(presentation), MenuReadOutcome::Snapshot(_)));
    assert_eq!(rig.ax.counts().live(), 2);
    assert_eq!(rig.owner.release(presentation), MenuReleaseOutcome::Released {
        elements: 2
    });
    assert_eq!(rig.ax.counts().live(), 0);
    assert_eq!(rig.owner.release(presentation), MenuReleaseOutcome::Unknown);
}

#[test]
fn a_failed_read_retains_nothing_and_has_nothing_to_release() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    rig.ax.inject(Op::Title, None, AxError::CannotComplete);
    let presentation = PresentationId::allocate();
    assert_eq!(rig.read(presentation), MenuReadOutcome::Transport);
    assert_eq!(rig.ax.counts().live(), 0);
    assert_eq!(rig.owner.release(presentation), MenuReleaseOutcome::Unknown);
}

#[test]
fn a_reused_presentation_id_is_rejected_and_released() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    let presentation = PresentationId::allocate();
    assert!(matches!(rig.read(presentation), MenuReadOutcome::Snapshot(_)));
    assert_eq!(rig.read(presentation), MenuReadOutcome::DispatchFailure);
    assert_eq!(
        rig.ax.counts().live(),
        1,
        "the original stays; the duplicate was released"
    );
    assert_eq!(rig.owner.release(presentation), MenuReleaseOutcome::Released {
        elements: 1
    });
    assert_eq!(rig.ax.counts().live(), 0);
}

#[test]
fn successor_presentations_are_isolated() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    let first = PresentationId::allocate();
    let second = PresentationId::allocate();
    assert!(matches!(rig.read(first), MenuReadOutcome::Snapshot(_)));
    assert!(matches!(rig.read(second), MenuReadOutcome::Snapshot(_)));
    assert_eq!(rig.owner.release(first), MenuReleaseOutcome::Released {
        elements: 1
    });
    assert_eq!(
        rig.ax.counts().live(),
        1,
        "releasing one presentation keeps its successor"
    );
    assert_eq!(rig.owner.release(second), MenuReleaseOutcome::Released {
        elements: 1
    });
}

#[test]
fn shutdown_releases_everything_and_closes_the_owner() {
    let rig = owner_rig(bar(vec![Node::item("A"), Node::item("B")]));
    assert!(matches!(
        rig.read(PresentationId::allocate()),
        MenuReadOutcome::Snapshot(_)
    ));
    assert_eq!(rig.owner.shutdown(), MenuReleaseOutcome::Released {
        elements: 2
    });
    assert_eq!(rig.ax.counts().live(), 0);
    wait_until(|| {
        rig.owner.release(PresentationId::allocate()) == MenuReleaseOutcome::DispatchFailure
    });
}

/// Blocks the owner inside its first AX call until the returned sender fires.
fn block_first_call(ax: &FakeAx) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let entered_tx = Mutex::new(Some(entered_tx));
    ax.set_hook(move |call| {
        if call.index == 0 {
            let taken = entered_tx.lock().expect("lock").take();
            if let Some(tx) = taken {
                let _ = tx.send(());
            }
            let _ = go_rx.lock().expect("lock").recv_timeout(Duration::from_secs(10));
        }
    });
    (entered_rx, go_tx)
}

#[test]
fn release_timeout_is_pending_not_success_and_still_completes_later() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    let presentation = PresentationId::allocate();
    assert!(matches!(rig.read(presentation), MenuReadOutcome::Snapshot(_)));
    // Stall the owner inside a second read's first AX call.
    let base = rig.ax.call_count();
    let (blocked_tx, blocked_rx) = mpsc::channel::<()>();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    let blocked_tx = Mutex::new(Some(blocked_tx));
    rig.ax.set_hook(move |call| {
        if call.index == base {
            let taken = blocked_tx.lock().expect("lock").take();
            if let Some(tx) = taken {
                let _ = tx.send(());
            }
            let _ = resume_rx.lock().expect("lock").recv_timeout(Duration::from_secs(10));
        }
    });
    let (read_tx, read_rx) = mpsc::channel();
    rig.owner
        .submit_read(rig.request(PresentationId::allocate()), move |outcome| {
            let _ = read_tx.send(outcome);
        })
        .expect("queued");
    blocked_rx.recv_timeout(Duration::from_secs(5)).expect("owner stalled in AX");
    assert_eq!(
        rig.owner.release_within(presentation, Duration::from_millis(20)),
        MenuReleaseOutcome::Pending,
        "a timed-out cleanup is pending, not successful"
    );
    assert_eq!(rig.ax.counts().live(), 1, "nothing was released yet");
    resume_tx.send(()).expect("resume");
    let _ = read_rx.recv_timeout(Duration::from_secs(5)).expect("stalled read finishes");
    // The queued release still ran: only the second presentation remains.
    wait_until(|| rig.ax.counts().live() == 1 && rig.ax.counts().released > 0);
    assert_eq!(
        rig.owner.release(presentation),
        MenuReleaseOutcome::Unknown,
        "released once"
    );
}

#[test]
fn a_cancelled_queued_read_issues_no_ax_call() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    let (entered, go) = block_first_call(&rig.ax);
    let (first_tx, first_rx) = mpsc::channel();
    rig.owner
        .submit_read(rig.request(PresentationId::allocate()), move |outcome| {
            let _ = first_tx.send(outcome);
        })
        .expect("queued");
    entered.recv_timeout(Duration::from_secs(5)).expect("stalled");

    let queued_cancel = CancellationToken::new();
    let (second_tx, second_rx) = mpsc::channel();
    rig.owner
        .submit_read(
            ReadRequest {
                target: Some(rig.target),
                presentation: PresentationId::allocate(),
                cancel: queued_cancel.clone(),
                deadline: None,
            },
            move |outcome| {
                let _ = second_tx.send(outcome);
            },
        )
        .expect("queued");
    queued_cancel.cancel();
    go.send(()).expect("resume");
    assert!(matches!(
        first_rx.recv_timeout(Duration::from_secs(5)).expect("first"),
        MenuReadOutcome::Snapshot(_)
    ));
    let calls_after_first = rig.ax.call_count();
    assert_eq!(
        second_rx.recv_timeout(Duration::from_secs(5)).expect("second"),
        MenuReadOutcome::Cancelled
    );
    assert_eq!(
        rig.ax.call_count(),
        calls_after_first,
        "the cancelled read issued nothing"
    );
    let _ = &rig.clock;
}

#[test]
fn the_control_queue_is_bounded() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    let (entered, go) = block_first_call(&rig.ax);
    let sink = |_: MenuReadOutcome| {};
    rig.owner
        .submit_read(rig.request(PresentationId::allocate()), sink)
        .expect("first");
    entered.recv_timeout(Duration::from_secs(5)).expect("stalled");
    let mut accepted = 0;
    let rejected = loop {
        match rig.owner.submit_read(rig.request(PresentationId::allocate()), sink) {
            Ok(()) => accepted += 1,
            Err(error) => break error,
        }
        assert!(accepted <= QUEUE_BOUND, "queue never filled");
    };
    assert_eq!(rejected, SubmitError::Full);
    assert_eq!(accepted, QUEUE_BOUND);
    go.send(()).expect("resume");
    let _ = rig.owner.shutdown_within(Duration::from_secs(5));
}

#[test]
fn a_panic_inside_owner_work_is_contained_and_typed() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    rig.ax.set_hook(|call| assert!(call.index != 3, "scripted AX panic"));
    assert_eq!(rig.read(PresentationId::allocate()), MenuReadOutcome::OwnerPanic);
    assert_eq!(rig.ax.counts().live(), 0, "unwinding released everything");
    // The owner keeps serving requests afterwards.
    rig.ax.set_hook(|_| {});
    assert!(matches!(
        rig.read(PresentationId::allocate()),
        MenuReadOutcome::Snapshot(_)
    ));
}

#[test]
fn dropping_the_owner_handle_releases_retained_elements_on_the_owner_thread() {
    let rig = owner_rig(bar(vec![Node::item("A")]));
    assert!(matches!(
        rig.read(PresentationId::allocate()),
        MenuReadOutcome::Snapshot(_)
    ));
    let OwnerRig { owner, ax, .. } = rig;
    drop(owner);
    wait_until(|| ax.counts().live() == 0);
}
