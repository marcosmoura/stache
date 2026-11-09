//! The budgeted, all-or-nothing menu-bar reader.
//!
//! [`read_menu`] is generic over [`AxApi`], so the same code runs against the
//! system and against fakes. It runs on the AX owner thread. Budgets
//! (see the Part 02 spec): 0.5 s messaging timeout set on **each element object
//! before it is messaged**, one 1.5 s cumulative deadline started before the
//! first read, 4096 visited nodes, depth 16 (the application root is depth 0),
//! cycle detection by `CFEqual` ancestry, and a child-array cap checked before
//! the array is copied. The deadline and cancellation are checked before and
//! after every AX call; after either trips no further AX call is issued. An
//! in-flight call cannot be interrupted. Any failure discards everything:
//! every element held so far is dropped (released) and no partial snapshot is
//! returned.

use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::ax::{AxApi, AxError, StringAttribute};
use super::snapshot::{
    ActionToken, MarkState, MenuItemKind, MenuItemSnapshot, MenuSnapshot, PresentationId,
    SubmenuSnapshot, key_equivalent,
};
pub use crate::ax::ELEMENT_TIMEOUT;
/// Cumulative read deadline, started before the first AX read.
pub const READ_DEADLINE: Duration = Duration::from_millis(1500);
/// Maximum visited elements.
pub const MAX_NODES: usize = 4096;
/// Maximum depth; the application root is depth 0.
pub const MAX_DEPTH: usize = 16;

/// A monotonic clock, injected for deterministic tests.
pub trait MenuClock: Send + Sync {
    /// The current monotonic time.
    fn now(&self) -> Instant;
}

/// The system monotonic clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemMenuClock;

impl MenuClock for SystemMenuClock {
    fn now(&self) -> Instant { Instant::now() }
}

/// The application instance whose menu is read.
///
/// The frontmost capture is done by the caller through the applications
/// adapter; PID alone is never identity, so the exact [`ApplicationId`] rides
/// along for validation.
///
/// [`ApplicationId`]: crate::applications::ApplicationId
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuTarget {
    /// The process id used to create the AX root.
    pub pid: i32,
    /// The exact retained application instance.
    pub application: crate::applications::ApplicationId,
}

/// The result of validating a captured target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCheck {
    /// The exact instance is still the target.
    Current,
    /// The instance changed or terminated.
    Stale,
    /// The check could not run (queue rejected, acknowledgement lost).
    Unavailable,
}

/// Validates the exact captured instance. Called by the reader before the AX
/// root is created and again after the read, never between AX calls.
pub trait TargetValidator: Send + Sync {
    /// Checks `target` against the live application state.
    fn validate(&self, target: &MenuTarget) -> TargetCheck;
}

/// The outcome of one read. Only [`Self::Snapshot`] carries a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuReadOutcome {
    /// A complete menu.
    Snapshot(MenuSnapshot),
    /// The target has no menu bar, or it is empty.
    NoMenu,
    /// `kAXErrorAPIDisabled`: Accessibility is not granted.
    PermissionDenied,
    /// The target does not support the AX menu API.
    Unsupported,
    /// The tree was structurally invalid (wrong type, invalid element).
    Malformed,
    /// Cancelled; logical authority was lost immediately.
    Cancelled,
    /// The cumulative deadline (or the request deadline) elapsed.
    DeadlineExceeded,
    /// More than [`MAX_NODES`] elements, or an oversized child array.
    NodeLimit,
    /// Deeper than [`MAX_DEPTH`].
    DepthLimit,
    /// An element was its own ancestor.
    Cycle,
    /// `kAXErrorCannotComplete` after less than the element timeout.
    Transport,
    /// `kAXErrorCannotComplete` after the element timeout elapsed.
    ElementTimeout,
    /// The per-element timeout could not be set.
    TimeoutSetFailed,
    /// The request could not be queued or answered by the owner.
    DispatchFailure,
    /// The owner thread panicked while handling the request.
    OwnerPanic,
    /// The frontmost application is this process.
    SelfTarget,
    /// There was no frontmost application.
    NoFrontmost,
    /// The captured instance changed before or after the read.
    StaleTarget,
}

/// Everything [`read_menu`] needs besides the AX API.
pub struct ReadContext<'a> {
    /// The monotonic clock.
    pub clock: &'a dyn MenuClock,
    /// Cancellation for this request.
    pub cancel: &'a CancellationToken,
    /// Identity validation.
    pub validator: &'a dyn TargetValidator,
    /// This process id, for [`MenuReadOutcome::SelfTarget`].
    pub own_pid: i32,
    /// An optional caller deadline, tighter than [`READ_DEADLINE`].
    pub request_deadline: Option<Instant>,
}

/// A successful read: the pure snapshot plus the retained actionable leaves.
///
/// Dropping this releases every retained element.
pub struct ReadSuccess<E> {
    /// The pure data.
    pub snapshot: MenuSnapshot,
    /// Actionable leaves, retained for the presentation's life.
    pub retained: Vec<(ActionToken, E)>,
}

/// Reads the menu bar of `target`. `Err` is never [`MenuReadOutcome::Snapshot`].
///
/// # Errors
///
/// Returns the typed failure; all retained state is already released.
pub fn read_menu<A: AxApi>(
    api: &A,
    ctx: &ReadContext<'_>,
    target: Option<&MenuTarget>,
    presentation: PresentationId,
) -> Result<ReadSuccess<A::Element>, MenuReadOutcome> {
    let target = target.ok_or(MenuReadOutcome::NoFrontmost)?;
    if target.pid == ctx.own_pid {
        return Err(MenuReadOutcome::SelfTarget);
    }
    if ctx.cancel.is_cancelled() {
        return Err(MenuReadOutcome::Cancelled);
    }
    match ctx.validator.validate(target) {
        TargetCheck::Current => {}
        TargetCheck::Stale => return Err(MenuReadOutcome::StaleTarget),
        TargetCheck::Unavailable => return Err(MenuReadOutcome::DispatchFailure),
    }
    // The cumulative deadline starts before the first AX read.
    let started = ctx.clock.now();
    let mut deadline = started + READ_DEADLINE;
    if let Some(request) = ctx.request_deadline {
        deadline = deadline.min(request);
    }
    let mut walker = Walker {
        api,
        clock: ctx.clock,
        cancel: ctx.cancel,
        deadline,
        visited: 0,
        presentation,
        next_ordinal: 0,
        retained: Vec::new(),
    };
    let items = walker.read_root(target.pid)?;
    walker.check()?;
    // Identity is validated again after reading and before anything is shown.
    match ctx.validator.validate(target) {
        TargetCheck::Current => {}
        TargetCheck::Stale => return Err(MenuReadOutcome::StaleTarget),
        TargetCheck::Unavailable => return Err(MenuReadOutcome::DispatchFailure),
    }
    walker.check()?;
    if items.is_empty() {
        return Err(MenuReadOutcome::NoMenu);
    }
    Ok(ReadSuccess {
        snapshot: MenuSnapshot { presentation, items },
        retained: walker.retained,
    })
}

/// The chain of ancestors, used for repeated-ancestry detection.
struct Path<'p, E> {
    element: &'p E,
    parent: Option<&'p Self>,
}

struct Walker<'a, A: AxApi> {
    api: &'a A,
    clock: &'a dyn MenuClock,
    cancel: &'a CancellationToken,
    deadline: Instant,
    visited: usize,
    presentation: PresentationId,
    next_ordinal: u32,
    retained: Vec<(ActionToken, A::Element)>,
}

type Step<T> = Result<T, MenuReadOutcome>;

impl<A: AxApi> Walker<'_, A> {
    /// Cancellation wins over an expired deadline.
    fn check(&self) -> Step<()> {
        if self.cancel.is_cancelled() {
            return Err(MenuReadOutcome::Cancelled);
        }
        if self.clock.now() >= self.deadline {
            return Err(MenuReadOutcome::DeadlineExceeded);
        }
        Ok(())
    }

    /// Runs one AX call between a pre- and a post-check. `Ok(Err(_))` is an
    /// absent attribute; every other failure aborts the whole read.
    fn call<T>(&self, call: impl FnOnce(&A) -> Result<T, AxError>) -> Step<Result<T, AxError>> {
        self.check()?;
        let begun = self.clock.now();
        let result = call(self.api);
        let elapsed = self.clock.now().saturating_duration_since(begun);
        self.check()?;
        match result {
            Ok(value) => Ok(Ok(value)),
            Err(error) if error.is_absent() => Ok(Err(error)),
            Err(AxError::ApiDisabled) => Err(MenuReadOutcome::PermissionDenied),
            Err(AxError::CannotComplete) if elapsed >= ELEMENT_TIMEOUT => {
                Err(MenuReadOutcome::ElementTimeout)
            }
            Err(AxError::CannotComplete) => Err(MenuReadOutcome::Transport),
            Err(_) => Err(MenuReadOutcome::Malformed),
        }
    }

    /// An optional attribute: absent yields `None`; anything else aborts.
    fn optional<T>(&self, call: impl FnOnce(&A) -> Result<T, AxError>) -> Step<Option<T>> {
        Ok(self.call(call)?.ok())
    }

    /// Counts, depth- and cycle-checks `element`, then sets its timeout.
    fn visit(
        &mut self,
        element: &A::Element,
        depth: usize,
        path: Option<&Path<'_, A::Element>>,
    ) -> Step<()> {
        if depth > MAX_DEPTH {
            return Err(MenuReadOutcome::DepthLimit);
        }
        let mut ancestor = path;
        while let Some(frame) = ancestor {
            if self.api.same_element(frame.element, element) {
                return Err(MenuReadOutcome::Cycle);
            }
            ancestor = frame.parent;
        }
        self.visited += 1;
        if self.visited > MAX_NODES {
            return Err(MenuReadOutcome::NodeLimit);
        }
        self.check()?;
        let set = self.api.set_messaging_timeout(element, ELEMENT_TIMEOUT);
        self.check()?;
        match set {
            Ok(()) => Ok(()),
            Err(AxError::ApiDisabled) => Err(MenuReadOutcome::PermissionDenied),
            Err(_) => Err(MenuReadOutcome::TimeoutSetFailed),
        }
    }

    /// The children of `element`, with the array capped before it is copied.
    fn children_of(&self, element: &A::Element) -> Step<Vec<A::Element>> {
        let Some(count) = self.optional(|api| api.child_count(element))? else {
            return Ok(Vec::new());
        };
        if count == 0 {
            return Ok(Vec::new());
        }
        if count > MAX_NODES.saturating_sub(self.visited) {
            return Err(MenuReadOutcome::NodeLimit);
        }
        let children = self.optional(|api| api.children(element, count))?.unwrap_or_default();
        if children.len() > count {
            return Err(MenuReadOutcome::NodeLimit);
        }
        Ok(children)
    }

    fn read_root(&mut self, pid: i32) -> Step<Vec<MenuItemSnapshot>> {
        self.check()?;
        let created = self.api.create_application(pid);
        let application = created.map_err(|_| MenuReadOutcome::Unsupported)?;
        self.check()?;
        self.visit(&application, 0, None)?;
        let root = Path {
            element: &application,
            parent: None,
        };
        let menu_bar = match self.call(|api| api.menu_bar(&application))? {
            Ok(menu_bar) => menu_bar,
            Err(AxError::NotImplemented) => return Err(MenuReadOutcome::Unsupported),
            Err(_) => return Err(MenuReadOutcome::NoMenu),
        };
        self.visit(&menu_bar, 1, Some(&root))?;
        let bar_path = Path {
            element: &menu_bar,
            parent: Some(&root),
        };
        let children = self.children_of(&menu_bar)?;
        self.read_items(children, &bar_path, 2, true)
    }

    /// Reads the items of one menu. `is_root` skips the first child (Apple
    /// menu) and bolds the first nonseparator item.
    fn read_items(
        &mut self,
        children: Vec<A::Element>,
        parent: &Path<'_, A::Element>,
        depth: usize,
        is_root: bool,
    ) -> Step<Vec<MenuItemSnapshot>> {
        let mut items = Vec::with_capacity(children.len());
        let mut first = is_root;
        for (index, child) in children.into_iter().enumerate() {
            if is_root && index == 0 {
                continue;
            }
            let read = self.read_item(&child, parent, depth, first)?;
            if !matches!(read.snapshot.kind, MenuItemKind::Separator) {
                first = false;
            }
            let mut snapshot = read.snapshot;
            if read.actionable {
                let token = ActionToken::new(self.presentation, self.next_ordinal);
                self.next_ordinal += 1;
                snapshot.action = Some(token);
                self.retained.push((token, child));
            }
            items.push(snapshot);
        }
        Ok(items)
    }

    fn read_item(
        &mut self,
        element: &A::Element,
        parent: &Path<'_, A::Element>,
        depth: usize,
        bold: bool,
    ) -> Step<ItemRead> {
        self.visit(element, depth, Some(parent))?;
        let path = Path { element, parent: Some(parent) };
        let title = self
            .optional(|api| api.string(element, StringAttribute::Title))?
            .unwrap_or_default();
        let role = self.optional(|api| api.string(element, StringAttribute::Role))?;
        if title.is_empty() || role.as_deref() == Some("AXSeparator") {
            return Ok(ItemRead {
                snapshot: MenuItemSnapshot::separator(),
                actionable: false,
            });
        }
        let enabled = self.optional(|api| api.enabled(element))?.unwrap_or(true);
        let mark = self.optional(|api| api.string(element, StringAttribute::MarkChar))?;
        let cmd = self.optional(|api| api.string(element, StringAttribute::CmdChar))?;
        let key = if cmd.as_deref().is_some_and(|c| !c.is_empty()) {
            let modifiers = self.optional(|api| api.cmd_modifiers(element))?;
            key_equivalent(cmd.as_deref(), modifiers)
        } else {
            None
        };
        let submenu = self.read_submenu(element, &path, depth, &title)?;
        let actionable = enabled && submenu.is_none();
        Ok(ItemRead {
            snapshot: MenuItemSnapshot {
                bold,
                kind: submenu.map_or(MenuItemKind::Item, MenuItemKind::Submenu),
                title,
                enabled,
                mark: MarkState::from_mark_char(mark.as_deref()),
                key_equivalent: key,
                action: None,
            },
            actionable,
        })
    }

    /// The first `AXMenu` child is the submenu. Like the legacy builder, an
    /// `AXMenu` with no children yields no submenu, so the item stays an
    /// ordinary leaf: pressable when enabled.
    fn read_submenu(
        &mut self,
        item: &A::Element,
        item_path: &Path<'_, A::Element>,
        item_depth: usize,
        title: &str,
    ) -> Step<Option<SubmenuSnapshot>> {
        let candidates = self.children_of(item)?;
        for candidate in candidates {
            self.visit(&candidate, item_depth + 1, Some(item_path))?;
            let role = self.optional(|api| api.string(&candidate, StringAttribute::Role))?;
            if role.as_deref() != Some("AXMenu") {
                continue;
            }
            let menu_path = Path {
                element: &candidate,
                parent: Some(item_path),
            };
            let children = self.children_of(&candidate)?;
            if children.is_empty() {
                return Ok(None);
            }
            let items = self.read_items(children, &menu_path, item_depth + 2, false)?;
            return Ok(Some(SubmenuSnapshot { title: title.to_owned(), items }));
        }
        Ok(None)
    }
}

struct ItemRead {
    snapshot: MenuItemSnapshot,
    actionable: bool,
}

const _: () = {
    const fn assert_send<T: Send + 'static>() {}
    assert_send::<MenuReadOutcome>();
    assert_send::<MenuTarget>();
};
