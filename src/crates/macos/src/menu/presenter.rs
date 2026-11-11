//! The main-thread `NSMenu` presenter.
//!
//! # Ownership
//!
//! ```text
//! worker thread ── PresentationId + pure snapshot ──> main closure (dispatch)
//!                                                       └─ SystemPresenter (ZST handle)
//!                                                            └─ thread-local map (main only):
//!                                                                 PresentationId -> { NSMenu, ActionTarget }
//! ```
//!
//! Every `NSMenu`/`NSMenuItem`/target object is created, used and dropped on the
//! main thread inside a thread-local map keyed by [`PresentationId`]. The public
//! handle ([`SystemPresenter`]) is a zero-sized `Send + Sync` value, so closures
//! that cross threads capture only IDs and pure data and look the native state
//! up on main. No blanket unsafe `Send`/`Sync` exists.
//!
//! Menu items carry only an index into the target's token table (`NSMenuItem`
//! `tag`); the action callback maps it to an [`ActionToken`], contains panics
//! (unwind-enabled builds only; release aborts)
//! and only calls the selection sink. It performs no AX call and no wait.
//!
//! The pure [`plan`] turns a [`MenuSnapshot`] into a [`RenderPlan`] (what to
//! draw and which items carry actions) so the rendering rules are testable
//! without `AppKit`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use super::geometry::AppKitPoint;
use super::snapshot::{
    ActionToken, MarkState, MenuItemKind, MenuItemSnapshot, MenuSnapshot, ModifierSet,
    PresentationId,
};

/// Receives the token a menu action selected. Called on main from the Cocoa
/// action callback: it must be bounded, nonblocking and AX-free.
pub type SelectionSink = Arc<dyn Fn(ActionToken) + Send + Sync>;

/// Why the presenter could not prepare or track a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenterError {
    /// Called off the main thread.
    NotMainThread,
    /// A menu for this presentation already exists.
    Duplicate,
    /// The native menu could not be built.
    Build(String),
}

/// The result of requesting exact tracking cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelResult {
    /// `cancelTracking` was called on the exact menu while it was tracking.
    Requested,
    /// The menu exists but is not tracking (not yet, or already closed).
    NotTracking,
    /// No such presentation.
    Unknown,
}

/// The result of removing a presentation's native state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveResult {
    /// The native state was dropped.
    Removed,
    /// No such presentation (already removed).
    Unknown,
    /// Tracking has not ended; nothing was removed.
    StillTracking,
}

/// How tracking ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackResult {
    /// Tracking began and ended.
    Closed,
    /// No such presentation.
    Unknown,
    /// Called off the main thread.
    NotMainThread,
}

/// The main-thread presenter surface. Implementations keep every native object
/// main-thread-local; the trait object itself is `Send + Sync` and is only
/// *called* from main dispatch closures.
pub trait PresenterBackend: Send + Sync {
    /// Builds and registers the native menu for `presentation`.
    ///
    /// # Errors
    ///
    /// [`PresenterError`] when it cannot be built.
    fn prepare(
        &self,
        presentation: PresentationId,
        snapshot: &MenuSnapshot,
        select: SelectionSink,
    ) -> Result<(), PresenterError>;

    /// Pops the menu up at `location` and returns when tracking ends. This is a
    /// nested loop: dispatch closures queued through a tracking-compatible
    /// dispatcher run inside it.
    fn track(&self, presentation: PresentationId, location: AppKitPoint) -> TrackResult;

    /// Calls `cancelTracking` on the exact menu of `presentation`.
    fn cancel_tracking(&self, presentation: PresentationId) -> CancelResult;

    /// Drops the native state of exactly `presentation`.
    fn remove(&self, presentation: PresentationId) -> RemoveResult;
}

/// Calls `sink` with panic containment (unwind-enabled builds only; release aborts). Returns
/// whether it ran to completion.
pub fn contain_selection(sink: &SelectionSink, token: ActionToken) -> bool {
    catch_unwind(AssertUnwindSafe(|| sink(token))).is_ok()
}

// ---- Pure rendering plan ------------------------------------------------------

/// `NSEventModifierFlag` bits (`NSEvent.h`): shift, control, option, command.
pub const MASK_SHIFT: usize = 1 << 17;
/// Control modifier bit.
pub const MASK_CONTROL: usize = 1 << 18;
/// Option modifier bit.
pub const MASK_OPTION: usize = 1 << 19;
/// Command modifier bit.
pub const MASK_COMMAND: usize = 1 << 20;

/// The `NSEventModifierFlags` mask for a displayed shortcut.
#[must_use]
pub const fn modifier_mask(modifiers: ModifierSet) -> usize {
    let mut mask = 0;
    if modifiers.shift {
        mask |= MASK_SHIFT;
    }
    if modifiers.control {
        mask |= MASK_CONTROL;
    }
    if modifiers.option {
        mask |= MASK_OPTION;
    }
    if modifiers.command {
        mask |= MASK_COMMAND;
    }
    mask
}

/// One planned native item.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedItem {
    /// A separator.
    Separator,
    /// A titled item.
    Item(Box<PlannedEntry>),
}

/// A titled planned item.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedEntry {
    /// The title.
    pub title: String,
    /// `NSMenuItem.enabled`.
    pub enabled: bool,
    /// The check state.
    pub mark: MarkState,
    /// Bold title (attributed).
    pub bold: bool,
    /// Key equivalent and modifier mask.
    pub key: Option<(String, usize)>,
    /// Index into [`RenderPlan::tokens`] when the item carries an action.
    pub action_slot: Option<u32>,
    /// The submenu, with the parent title as its title.
    pub submenu: Option<Vec<PlannedItem>>,
}

/// What to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    /// Root items.
    pub items: Vec<PlannedItem>,
    /// Action tokens; an item's `action_slot` indexes this table.
    pub tokens: Vec<ActionToken>,
}

/// Plans the native menu from a snapshot. Actions are attached only to enabled
/// leaves that carry a token, and carry only that token.
#[must_use]
pub fn plan(snapshot: &MenuSnapshot) -> RenderPlan {
    let mut tokens = Vec::new();
    let items = plan_items(&snapshot.items, &mut tokens);
    RenderPlan { items, tokens }
}

fn plan_items(items: &[MenuItemSnapshot], tokens: &mut Vec<ActionToken>) -> Vec<PlannedItem> {
    items
        .iter()
        .map(|item| match &item.kind {
            MenuItemKind::Separator => PlannedItem::Separator,
            MenuItemKind::Item | MenuItemKind::Submenu(_) => {
                let leaf = matches!(item.kind, MenuItemKind::Item);
                let action_slot = match item.action {
                    Some(token) if leaf && item.enabled => {
                        tokens.push(token);
                        u32::try_from(tokens.len() - 1).ok()
                    }
                    _ => None,
                };
                let submenu = match &item.kind {
                    MenuItemKind::Submenu(sub) => Some(plan_items(&sub.items, tokens)),
                    _ => None,
                };
                PlannedItem::Item(Box::new(PlannedEntry {
                    title: item.title.clone(),
                    enabled: item.enabled,
                    mark: item.mark,
                    bold: item.bold,
                    key: item
                        .key_equivalent
                        .as_ref()
                        .map(|key| (key.key.clone(), modifier_mask(key.modifiers))),
                    action_slot,
                    submenu,
                }))
            }
        })
        .collect()
}

// ---- System implementation ------------------------------------------------------

mod system {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{
        AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, sel,
    };
    use objc2_app_kit::{
        NSControlStateValueMixed, NSControlStateValueOff, NSControlStateValueOn,
        NSEventModifierFlags, NSFont, NSFontAttributeName, NSMenu, NSMenuItem, NSScreen,
    };
    use objc2_foundation::{
        NSAttributedString, NSDictionary, NSObject, NSObjectProtocol, NSPoint, NSString,
    };

    use super::{
        AppKitPoint, CancelResult, MarkState, PlannedEntry, PlannedItem, PresentationId,
        PresenterBackend, PresenterError, RemoveResult, SelectionSink, TrackResult,
        contain_selection, plan,
    };
    use crate::applications::DisplayRect;
    use crate::desktop::DisplayId;
    use crate::menu::geometry::{DisplayGeometry, GeometryError, GeometrySnapshot, GeometrySource};
    use crate::menu::snapshot::{ActionToken, MenuSnapshot};

    struct TargetIvars {
        tokens: Vec<ActionToken>,
        sink: SelectionSink,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements. The class owns a
        // token table and a `Send + Sync` sink; AppKit calls the selector with a
        // valid `NSMenuItem`. It is `MainThreadOnly`; no Send/Sync shim exists.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "StacheMenuAnywhereActionTarget"]
        #[ivars = TargetIvars]
        struct ActionTarget;

        impl ActionTarget {
            /// Selects the token for the item's tag; panics are contained
            /// (unwind-enabled builds only; release aborts) and
            /// nothing else happens (no AX, no wait).
            #[unsafe(method(stacheMenuItemSelected:))]
            fn selected(&self, sender: &NSMenuItem) {
                let ivars = self.ivars();
                let token = usize::try_from(sender.tag())
                    .ok()
                    .and_then(|slot| ivars.tokens.get(slot).copied());
                if let Some(token) = token {
                    let _ = contain_selection(&ivars.sink, token);
                }
            }
        }

        unsafe impl NSObjectProtocol for ActionTarget {}
    );

    struct Native {
        menu: Retained<NSMenu>,
        _target: Retained<ActionTarget>,
        tracking: Cell<bool>,
    }

    thread_local! {
        /// Main-thread-only native state, keyed by exact presentation.
        static NATIVE: RefCell<HashMap<PresentationId, Native>> = RefCell::new(HashMap::new());
    }

    /// The system presenter: a zero-sized, thread-safe handle. All native state
    /// is main-thread-local.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SystemPresenter;

    fn build_menu(
        mtm: MainThreadMarker,
        title: &str,
        items: &[PlannedItem],
        target: &ActionTarget,
    ) -> Retained<NSMenu> {
        let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(title));
        menu.setAutoenablesItems(false);
        for item in items {
            menu.addItem(&build_item(mtm, item, target));
        }
        menu
    }

    fn build_item(
        mtm: MainThreadMarker,
        item: &PlannedItem,
        target: &ActionTarget,
    ) -> Retained<NSMenuItem> {
        let PlannedItem::Item(entry) = item else {
            return NSMenuItem::separatorItem(mtm);
        };
        let PlannedEntry {
            title,
            enabled,
            mark,
            bold,
            key,
            action_slot,
            submenu,
        } = &**entry;
        let ns_title = NSString::from_str(title);
        let key_text = NSString::from_str(key.as_ref().map_or("", |(key, _)| key.as_str()));
        let action = action_slot.map(|_| sel!(stacheMenuItemSelected:));
        // SAFETY: the selector, when present, is implemented by `ActionTarget`
        // with an `NSMenuItem` argument; the strings are live.
        let native = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &ns_title,
                action,
                &key_text,
            )
        };
        if let Some((_, mask)) = key {
            native.setKeyEquivalentModifierMask(NSEventModifierFlags(*mask));
        }
        native.setEnabled(*enabled);
        native.setState(match mark {
            MarkState::On => NSControlStateValueOn,
            MarkState::Mixed => NSControlStateValueMixed,
            MarkState::Off => NSControlStateValueOff,
        });
        if let Some(slot) = action_slot {
            // SAFETY: the target is retained by the presentation's `Native`
            // entry for as long as the menu exists; `NSMenuItem` keeps only a
            // weak reference and the entry outlives every tracking session.
            unsafe { native.setTarget(Some(AsRef::<AnyObject>::as_ref(target))) };
            native.setTag(isize::try_from(*slot).unwrap_or(0));
        }
        if *bold {
            apply_bold(&native, &ns_title);
        }
        if let Some(items) = submenu {
            native.setSubmenu(Some(&build_menu(mtm, title, items, target)));
        }
        native
    }

    fn apply_bold(native: &NSMenuItem, title: &NSString) {
        let font = NSFont::boldSystemFontOfSize(NSFont::systemFontSize());
        // SAFETY: `NSFontAttributeName` is an immutable AppKit constant and the
        // value is an `NSFont`, the documented value type for that key.
        let attributes: Retained<NSDictionary<NSString, AnyObject>> = unsafe {
            NSDictionary::from_retained_objects(&[NSFontAttributeName], &[Retained::into_super(
                Retained::into_super(font),
            )])
        };
        // SAFETY: the attributes follow the NSAttributedString contract.
        let styled = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                title,
                Some(&attributes),
            )
        };
        native.setAttributedTitle(Some(&styled));
    }

    impl PresenterBackend for SystemPresenter {
        fn prepare(
            &self,
            presentation: PresentationId,
            snapshot: &MenuSnapshot,
            select: SelectionSink,
        ) -> Result<(), PresenterError> {
            let mtm = MainThreadMarker::new().ok_or(PresenterError::NotMainThread)?;
            if NATIVE.with(|native| native.borrow().contains_key(&presentation)) {
                return Err(PresenterError::Duplicate);
            }
            let plan = plan(snapshot);
            let target = ActionTarget::alloc(mtm).set_ivars(TargetIvars {
                tokens: plan.tokens,
                sink: select,
            });
            // SAFETY: `alloc`+`set_ivars` returned an uninitialized instance of
            // this exact subclass; `init` is NSObject's designated initializer.
            let target: Retained<ActionTarget> = unsafe { objc2::msg_send![super(target), init] };
            let menu = build_menu(mtm, "", &plan.items, &target);
            NATIVE.with(|native| {
                native.borrow_mut().insert(presentation, Native {
                    menu,
                    _target: target,
                    tracking: Cell::new(false),
                });
            });
            Ok(())
        }

        fn track(&self, presentation: PresentationId, location: AppKitPoint) -> TrackResult {
            if MainThreadMarker::new().is_none() {
                return TrackResult::NotMainThread;
            }
            // Clone the menu and release the map borrow *before* the nested
            // loop: dispatch closures run inside it and re-enter this map.
            let menu = NATIVE.with(|native| {
                native.borrow().get(&presentation).map(|entry| {
                    entry.tracking.set(true);
                    entry.menu.clone()
                })
            });
            let Some(menu) = menu else {
                return TrackResult::Unknown;
            };
            // `nil` view: `location` is in AppKit global screen coordinates.
            let _ = menu.popUpMenuPositioningItem_atLocation_inView(
                None,
                NSPoint::new(location.x, location.y),
                None,
            );
            NATIVE.with(|native| {
                if let Some(entry) = native.borrow().get(&presentation) {
                    entry.tracking.set(false);
                }
            });
            TrackResult::Closed
        }

        fn cancel_tracking(&self, presentation: PresentationId) -> CancelResult {
            if MainThreadMarker::new().is_none() {
                return CancelResult::Unknown;
            }
            let found = NATIVE.with(|native| {
                native
                    .borrow()
                    .get(&presentation)
                    .map(|entry| (entry.tracking.get(), entry.menu.clone()))
            });
            match found {
                None => CancelResult::Unknown,
                Some((false, _)) => CancelResult::NotTracking,
                Some((true, menu)) => {
                    menu.cancelTracking();
                    CancelResult::Requested
                }
            }
        }

        fn remove(&self, presentation: PresentationId) -> RemoveResult {
            if MainThreadMarker::new().is_none() {
                return RemoveResult::StillTracking;
            }
            NATIVE.with(|native| {
                let mut native = native.borrow_mut();
                match native.get(&presentation) {
                    None => RemoveResult::Unknown,
                    Some(entry) if entry.tracking.get() => RemoveResult::StillTracking,
                    Some(_) => {
                        native.remove(&presentation);
                        RemoveResult::Removed
                    }
                }
            })
        }
    }

    /// Pairs `NSScreen.screens` with `CGDisplayBounds` by `CGDirectDisplayID`.
    /// Main thread only.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SystemGeometry;

    impl GeometrySource for SystemGeometry {
        fn snapshot(&self) -> Result<GeometrySnapshot, GeometryError> {
            use core_graphics::display::CGDisplay;
            let mtm = MainThreadMarker::new().ok_or(GeometryError::Unavailable)?;
            let displays = NSScreen::screens(mtm)
                .to_vec()
                .iter()
                .map(|screen| {
                    let id = screen.CGDirectDisplayID();
                    let frame = screen.frame();
                    let bounds = CGDisplay::new(id).bounds();
                    DisplayGeometry {
                        id: DisplayId::new(id),
                        quartz: DisplayRect::new(
                            bounds.origin.x,
                            bounds.origin.y,
                            bounds.size.width,
                            bounds.size.height,
                        ),
                        appkit: DisplayRect::new(
                            frame.origin.x,
                            frame.origin.y,
                            frame.size.width,
                            frame.size.height,
                        ),
                        backing_scale: screen.backingScaleFactor(),
                    }
                })
                .collect();
            Ok(GeometrySnapshot {
                primary: DisplayId::new(CGDisplay::main().id),
                displays,
            })
        }
    }
}

pub use system::{SystemGeometry, SystemPresenter};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::snapshot::{KeyEquivalent, SubmenuSnapshot};

    fn token(ordinal: u32) -> ActionToken { ActionToken::new(PresentationId::allocate(), ordinal) }

    fn leaf(title: &str, action: Option<ActionToken>) -> MenuItemSnapshot {
        MenuItemSnapshot {
            title: title.to_owned(),
            kind: MenuItemKind::Item,
            enabled: true,
            mark: MarkState::Off,
            key_equivalent: None,
            bold: false,
            action,
        }
    }

    fn entry(item: &PlannedItem) -> &PlannedEntry {
        match item {
            PlannedItem::Item(entry) => entry,
            PlannedItem::Separator => panic!("separator"),
        }
    }

    #[test]
    fn modifier_masks_follow_the_appkit_bits() {
        let all = ModifierSet {
            shift: true,
            option: true,
            control: true,
            command: true,
        };
        assert_eq!(modifier_mask(all), (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20));
        assert_eq!(modifier_mask(ModifierSet::default()), 0);
        assert_eq!(modifier_mask(ModifierSet::from_ax(None)), MASK_COMMAND);
        assert_eq!(modifier_mask(ModifierSet::from_ax(Some(8))), 0);
        assert_eq!(modifier_mask(ModifierSet::from_ax(Some(1 | 8))), MASK_SHIFT);
    }

    #[test]
    fn only_enabled_leaves_with_a_token_get_an_action_and_slots_index_the_table() {
        let (a, b) = (token(0), token(1));
        let mut disabled = leaf("Disabled", Some(token(9)));
        disabled.enabled = false;
        let mut with_key = leaf("Save", Some(b));
        with_key.key_equivalent = Some(KeyEquivalent {
            key: "s".to_owned(),
            modifiers: ModifierSet::from_ax(None),
        });
        let mut submenu = leaf("More", Some(token(10)));
        submenu.kind = MenuItemKind::Submenu(SubmenuSnapshot {
            title: "More".to_owned(),
            items: vec![leaf("Inner", Some(a))],
        });
        let snapshot = MenuSnapshot {
            presentation: PresentationId::allocate(),
            items: vec![
                disabled,
                MenuItemSnapshot::separator(),
                with_key,
                submenu,
                leaf("No token", None),
            ],
        };
        let plan = plan(&snapshot);
        assert_eq!(entry(&plan.items[0]).action_slot, None);
        assert_eq!(plan.items[1], PlannedItem::Separator);
        assert_eq!(entry(&plan.items[2]).action_slot, Some(0));
        assert_eq!(entry(&plan.items[2]).key, Some(("s".to_owned(), MASK_COMMAND)));
        // A submenu parent never carries an action, but its leaves do.
        let parent = entry(&plan.items[3]);
        assert_eq!(parent.action_slot, None);
        let inner = entry(&parent.submenu.as_ref().expect("submenu")[0]);
        assert_eq!(inner.action_slot, Some(1));
        assert_eq!(entry(&plan.items[4]).action_slot, None);
        assert_eq!(plan.tokens, vec![b, a]);
    }

    #[test]
    fn a_panicking_sink_is_contained() {
        let sink: SelectionSink = Arc::new(|_| panic!("boom"));
        assert!(!contain_selection(&sink, token(0)));
        let ok: SelectionSink = Arc::new(|_| {});
        assert!(contain_selection(&ok, token(0)));
    }
}
