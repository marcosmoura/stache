//! A fake [`WindowAx`] arena. It never calls the real AX API.
//!
//! Every attribute is scripted per node as a `Result`, so a test can express a
//! missing attribute, a malformed value (`AxError::WrongType`), a timed-out
//! call or a partial write. The fake records every call in order and tracks live
//! element handles so tests can prove element balance. [`FakeWindowElement`] is
//! `!Send` like the real element.

#![allow(missing_docs, reason = "test fakes are self-describing")]
#![allow(clippy::must_use_candidate, reason = "test fakes")]
#![allow(clippy::missing_panics_doc, reason = "test fakes")]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the fake holds one short-lived lock per scripted step"
)]

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::api::{AxApi, AxError, StringAttribute};
use super::window::{AxPoint, AxSize, BoolAttribute, GeometryAttribute, WindowAx, WindowString};
use super::window_id::{WindowIdCapability, WindowIdError};

pub type NodeId = usize;

type WriteHook = Arc<dyn Fn() + Send + Sync>;
type NodeWriteHook = Arc<dyn Fn(NodeId) + Send + Sync>;
type EnhancedUiHook = Arc<dyn Fn(NodeId, bool) + Send + Sync>;

/// One scripted fake element.
#[derive(Clone, Debug)]
pub struct FakeNode {
    pub position: Result<AxPoint, AxError>,
    pub size: Result<AxSize, AxError>,
    pub position_settable: Result<bool, AxError>,
    pub size_settable: Result<bool, AxError>,
    pub bools: HashMap<BoolAttribute, Result<bool, AxError>>,
    pub strings: HashMap<WindowString, Result<String, AxError>>,
    pub windows: Result<Vec<NodeId>, AxError>,
    pub focused: Result<NodeId, AxError>,
    pub main: Result<NodeId, AxError>,
    pub children: Result<Vec<NodeId>, AxError>,
    pub tabs: Result<Vec<NodeId>, AxError>,
    /// The tab element's selected flag (`AXValue`).
    pub selected: Result<bool, AxError>,
    pub window_id: Result<u32, WindowIdError>,
    pub set_position_result: Result<(), AxError>,
    pub set_size_result: Result<(), AxError>,
    pub timeout_result: Result<(), AxError>,
    pub raise_result: Result<(), AxError>,
    /// The result of `set_hidden`; on success the node's `Hidden` bool follows.
    pub set_hidden_result: Result<(), AxError>,
    /// `AXEnhancedUserInterface` of an application node. An application that
    /// lacks the attribute fails the read with `AttributeUnsupported`.
    pub enhanced_ui: Result<bool, AxError>,
    /// The result of `set_enhanced_user_interface`; on success `enhanced_ui`
    /// follows the written value.
    pub set_enhanced_ui_result: Result<(), AxError>,
}

impl Default for FakeNode {
    fn default() -> Self {
        Self {
            position: Err(AxError::AttributeUnsupported),
            size: Err(AxError::AttributeUnsupported),
            position_settable: Err(AxError::AttributeUnsupported),
            size_settable: Err(AxError::AttributeUnsupported),
            bools: HashMap::new(),
            strings: HashMap::new(),
            windows: Err(AxError::AttributeUnsupported),
            focused: Err(AxError::NoValue),
            main: Err(AxError::NoValue),
            children: Err(AxError::AttributeUnsupported),
            tabs: Err(AxError::AttributeUnsupported),
            selected: Err(AxError::AttributeUnsupported),
            window_id: Err(WindowIdError::Unsupported),
            set_position_result: Ok(()),
            set_size_result: Ok(()),
            timeout_result: Ok(()),
            raise_result: Ok(()),
            set_hidden_result: Ok(()),
            enhanced_ui: Err(AxError::AttributeUnsupported),
            set_enhanced_ui_result: Ok(()),
        }
    }
}

/// One recorded call, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    SetTimeout(NodeId, Duration),
    Position(NodeId),
    Size(NodeId),
    SetPosition(NodeId, AxPoint),
    SetSize(NodeId, AxSize),
    IsSettable(NodeId, GeometryAttribute),
    Bool(NodeId, BoolAttribute),
    String(NodeId, WindowString),
    Windows(NodeId, usize, usize),
    Children(NodeId, usize),
    Tabs(NodeId, usize),
    TabSelected(NodeId),
    WindowId(NodeId),
    Raise(NodeId),
    SetHidden(NodeId, bool),
    EnhancedUi(NodeId),
    SetEnhancedUi(NodeId, bool),
    Observe(NodeId, super::AxNotification),
    Unobserve(NodeId, super::AxNotification),
}

#[derive(Default)]
struct State {
    nodes: Vec<FakeNode>,
    calls: Vec<Call>,
    applications: HashMap<i32, NodeId>,
}

/// The fake window AX arena.
#[derive(Clone)]
pub struct FakeWindowAx {
    state: Arc<Mutex<State>>,
    live: Arc<AtomicUsize>,
    window_id_available: bool,
    write_hook: Arc<Mutex<Option<WriteHook>>>,
    node_write_hook: Arc<Mutex<Option<NodeWriteHook>>>,
    enhanced_ui_hook: Arc<Mutex<Option<EnhancedUiHook>>>,
}

impl std::fmt::Debug for FakeWindowElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FakeWindowElement({})", self.node)
    }
}

/// A fake element handle. `!Send`, counted while alive.
pub struct FakeWindowElement {
    node: NodeId,
    live: Arc<AtomicUsize>,
    _owner_local: PhantomData<*const ()>,
}

impl FakeWindowElement {
    pub const fn node(&self) -> NodeId { self.node }
}

impl Drop for FakeWindowElement {
    fn drop(&mut self) { self.live.fetch_sub(1, Ordering::SeqCst); }
}

impl Default for FakeWindowAx {
    fn default() -> Self { Self::new() }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeWindowAx {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            live: Arc::new(AtomicUsize::new(0)),
            window_id_available: true,
            write_hook: Arc::new(Mutex::new(None)),
            node_write_hook: Arc::new(Mutex::new(None)),
            enhanced_ui_hook: Arc::new(Mutex::new(None)),
        }
    }

    /// A fake whose `_AXUIElementGetWindow` symbol is missing.
    pub fn without_window_id_symbol() -> Self {
        Self {
            window_id_available: false,
            ..Self::new()
        }
    }

    pub fn add_node(&self, node: FakeNode) -> NodeId {
        let mut state = lock(&self.state);
        state.nodes.push(node);
        state.nodes.len() - 1
    }

    /// Makes `create_application(pid)` return an element for `node`.
    pub fn set_application(&self, pid: i32, node: NodeId) {
        lock(&self.state).applications.insert(pid, node);
    }

    /// Runs `hook` on the calling (owner) thread inside every write and raise,
    /// after the call is recorded and before it returns. Tests use it to stall
    /// or panic a native send.
    pub fn on_write(&self, hook: impl Fn() + Send + Sync + 'static) {
        *lock(&self.write_hook) = Some(Arc::new(hook));
    }

    /// Like [`Self::on_write`], but told which node is being written, so a test
    /// can stall or slow the writes of one application's windows only.
    pub fn on_node_write(&self, hook: impl Fn(NodeId) + Send + Sync + 'static) {
        *lock(&self.node_write_hook) = Some(Arc::new(hook));
    }

    /// Runs `hook` on the calling (owner) thread after every successful or
    /// failed `set_enhanced_user_interface`, once the call is recorded and the
    /// attribute updated. A test uses it to simulate a third party turning the
    /// attribute back on mid-lease.
    pub fn on_enhanced_ui_set(&self, hook: impl Fn(NodeId, bool) + Send + Sync + 'static) {
        *lock(&self.enhanced_ui_hook) = Some(Arc::new(hook));
    }

    /// The recorded `AXEnhancedUserInterface` writes as `(node, value)`.
    pub fn enhanced_ui_writes(&self) -> Vec<(NodeId, bool)> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::SetEnhancedUi(node, on) => Some((node, on)),
                _ => None,
            })
            .collect()
    }

    fn run_write_hook(&self, node: NodeId) {
        let hook = lock(&self.write_hook).clone();
        if let Some(hook) = hook {
            hook();
        }
        let hook = lock(&self.node_write_hook).clone();
        if let Some(hook) = hook {
            hook(node);
        }
    }

    pub fn edit(&self, id: NodeId, edit: impl FnOnce(&mut FakeNode)) {
        edit(&mut lock(&self.state).nodes[id]);
    }

    pub fn element(&self, node: NodeId) -> FakeWindowElement { self.make(node) }

    /// Destroys a node: every later read of it fails like a dead AX element.
    pub fn kill(&self, id: NodeId) {
        self.edit(id, |node| {
            node.strings.insert(WindowString::Role, Err(AxError::InvalidElement));
            node.window_id = Err(WindowIdError::Ax(AxError::InvalidElement));
        });
    }

    /// Records an observer call in the same ordered log.
    pub fn record(&self, call: Call) { lock(&self.state).calls.push(call); }

    pub fn calls(&self) -> Vec<Call> { lock(&self.state).calls.clone() }

    pub fn clear_calls(&self) { lock(&self.state).calls.clear(); }

    /// Element handles currently alive.
    pub fn live_elements(&self) -> usize { self.live.load(Ordering::SeqCst) }

    fn make(&self, node: NodeId) -> FakeWindowElement {
        self.live.fetch_add(1, Ordering::SeqCst);
        FakeWindowElement {
            node,
            live: Arc::clone(&self.live),
            _owner_local: PhantomData,
        }
    }

    fn read<T>(
        &self,
        element: &FakeWindowElement,
        call: Call,
        read: impl FnOnce(&FakeNode) -> T,
    ) -> T {
        let mut state = lock(&self.state);
        state.calls.push(call);
        read(&state.nodes[element.node])
    }

    fn elements(
        &self,
        ids: Result<Vec<NodeId>, AxError>,
        index: usize,
        limit: usize,
    ) -> Result<Vec<FakeWindowElement>, AxError> {
        Ok(ids?.into_iter().skip(index).take(limit).map(|id| self.make(id)).collect())
    }
}

impl AxApi for FakeWindowAx {
    type Element = FakeWindowElement;

    fn create_application(&self, pid: i32) -> Result<FakeWindowElement, AxError> {
        let node = lock(&self.state).applications.get(&pid).copied();
        node.map(|node| self.make(node)).ok_or(AxError::InvalidElement)
    }

    fn set_messaging_timeout(
        &self,
        element: &FakeWindowElement,
        timeout: Duration,
    ) -> Result<(), AxError> {
        self.read(element, Call::SetTimeout(element.node, timeout), |node| {
            node.timeout_result
        })
    }

    fn menu_bar(&self, _application: &FakeWindowElement) -> Result<FakeWindowElement, AxError> {
        Err(AxError::AttributeUnsupported)
    }

    fn string(
        &self,
        element: &FakeWindowElement,
        attribute: StringAttribute,
    ) -> Result<String, AxError> {
        match attribute {
            StringAttribute::Role => self.window_string(element, WindowString::Role),
            StringAttribute::Title => self.window_string(element, WindowString::Title),
            StringAttribute::MarkChar | StringAttribute::CmdChar => {
                Err(AxError::AttributeUnsupported)
            }
        }
    }

    fn enabled(&self, _element: &FakeWindowElement) -> Result<bool, AxError> {
        Err(AxError::AttributeUnsupported)
    }

    fn cmd_modifiers(&self, _element: &FakeWindowElement) -> Result<i64, AxError> {
        Err(AxError::AttributeUnsupported)
    }

    fn child_count(&self, element: &FakeWindowElement) -> Result<usize, AxError> {
        self.read(element, Call::Children(element.node, 0), |node| {
            node.children.as_ref().map(Vec::len).map_err(|error| *error)
        })
    }

    fn children(
        &self,
        element: &FakeWindowElement,
        limit: usize,
    ) -> Result<Vec<FakeWindowElement>, AxError> {
        let ids = self.read(element, Call::Children(element.node, limit), |node| {
            node.children.clone()
        });
        self.elements(ids, 0, limit)
    }

    fn press(&self, _element: &FakeWindowElement) -> Result<(), AxError> {
        Err(AxError::ActionUnsupported)
    }

    fn same_element(&self, a: &FakeWindowElement, b: &FakeWindowElement) -> bool {
        a.node == b.node
    }
}

impl WindowAx for FakeWindowAx {
    fn position(&self, element: &FakeWindowElement) -> Result<AxPoint, AxError> {
        self.read(element, Call::Position(element.node), |node| node.position)
    }

    fn size(&self, element: &FakeWindowElement) -> Result<AxSize, AxError> {
        self.read(element, Call::Size(element.node), |node| node.size)
    }

    fn set_position(&self, element: &FakeWindowElement, position: AxPoint) -> Result<(), AxError> {
        let mut state = lock(&self.state);
        state.calls.push(Call::SetPosition(element.node, position));
        let node = &mut state.nodes[element.node];
        let result = node.set_position_result;
        if result.is_ok() {
            node.position = Ok(position);
        }
        drop(state);
        self.run_write_hook(element.node);
        result
    }

    fn set_size(&self, element: &FakeWindowElement, size: AxSize) -> Result<(), AxError> {
        let mut state = lock(&self.state);
        state.calls.push(Call::SetSize(element.node, size));
        let node = &mut state.nodes[element.node];
        let result = node.set_size_result;
        if result.is_ok() {
            node.size = Ok(size);
        }
        drop(state);
        self.run_write_hook(element.node);
        result
    }

    fn is_settable(
        &self,
        element: &FakeWindowElement,
        attribute: GeometryAttribute,
    ) -> Result<bool, AxError> {
        self.read(
            element,
            Call::IsSettable(element.node, attribute),
            |node| match attribute {
                GeometryAttribute::Position => node.position_settable,
                GeometryAttribute::Size => node.size_settable,
            },
        )
    }

    fn bool_attribute(
        &self,
        element: &FakeWindowElement,
        attribute: BoolAttribute,
    ) -> Result<bool, AxError> {
        self.read(element, Call::Bool(element.node, attribute), |node| {
            node.bools
                .get(&attribute)
                .copied()
                .unwrap_or(Err(AxError::AttributeUnsupported))
        })
    }

    fn window_string(
        &self,
        element: &FakeWindowElement,
        attribute: WindowString,
    ) -> Result<String, AxError> {
        self.read(element, Call::String(element.node, attribute), |node| {
            node.strings
                .get(&attribute)
                .cloned()
                .unwrap_or(Err(AxError::AttributeUnsupported))
        })
    }

    fn window_count(&self, application: &FakeWindowElement) -> Result<usize, AxError> {
        self.read(application, Call::Windows(application.node, 0, 0), |node| {
            node.windows.as_ref().map(Vec::len).map_err(|error| *error)
        })
    }

    fn windows(
        &self,
        application: &FakeWindowElement,
        index: usize,
        limit: usize,
    ) -> Result<Vec<FakeWindowElement>, AxError> {
        let ids = self.read(
            application,
            Call::Windows(application.node, index, limit),
            |node| node.windows.clone(),
        );
        self.elements(ids, index, limit)
    }

    fn focused_window(
        &self,
        application: &FakeWindowElement,
    ) -> Result<FakeWindowElement, AxError> {
        let id = self.read(application, Call::Windows(application.node, 0, 0), |node| {
            node.focused
        });
        id.map(|id| self.make(id))
    }

    fn main_window(&self, application: &FakeWindowElement) -> Result<FakeWindowElement, AxError> {
        let id = self.read(application, Call::Windows(application.node, 0, 0), |node| {
            node.main
        });
        id.map(|id| self.make(id))
    }

    fn window_id_capability(&self) -> WindowIdCapability {
        if self.window_id_available {
            WindowIdCapability::Available
        } else {
            WindowIdCapability::Unsupported
        }
    }

    fn window_id(&self, element: &FakeWindowElement) -> Result<u32, WindowIdError> {
        if !self.window_id_available {
            return Err(WindowIdError::Unsupported);
        }
        self.read(element, Call::WindowId(element.node), |node| node.window_id)
    }

    fn tab_elements(
        &self,
        group: &FakeWindowElement,
        limit: usize,
    ) -> Result<Vec<FakeWindowElement>, AxError> {
        let ids = self.read(group, Call::Tabs(group.node, limit), |node| node.tabs.clone());
        self.elements(ids, 0, limit)
    }

    fn tab_selected(&self, tab: &FakeWindowElement) -> Result<bool, AxError> {
        self.read(tab, Call::TabSelected(tab.node), |node| node.selected)
    }

    fn set_hidden(&self, application: &FakeWindowElement, hidden: bool) -> Result<(), AxError> {
        let mut state = lock(&self.state);
        state.calls.push(Call::SetHidden(application.node, hidden));
        let node = &mut state.nodes[application.node];
        let result = node.set_hidden_result;
        if result.is_ok() {
            node.bools.insert(BoolAttribute::Hidden, Ok(hidden));
        }
        drop(state);
        self.run_write_hook(application.node);
        result
    }

    fn enhanced_user_interface(&self, application: &FakeWindowElement) -> Result<bool, AxError> {
        self.read(application, Call::EnhancedUi(application.node), |node| {
            node.enhanced_ui
        })
    }

    fn set_enhanced_user_interface(
        &self,
        application: &FakeWindowElement,
        on: bool,
    ) -> Result<(), AxError> {
        let mut state = lock(&self.state);
        state.calls.push(Call::SetEnhancedUi(application.node, on));
        let node = &mut state.nodes[application.node];
        let result = node.set_enhanced_ui_result;
        if result.is_ok() {
            node.enhanced_ui = Ok(on);
        }
        drop(state);
        let hook = lock(&self.enhanced_ui_hook).clone();
        if let Some(hook) = hook {
            hook(application.node, on);
        }
        result
    }

    fn raise(&self, element: &FakeWindowElement) -> Result<(), AxError> {
        let result = self.read(element, Call::Raise(element.node), |node| node.raise_result);
        self.run_write_hook(element.node);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::{
        AxReading, ELEMENT_TIMEOUT, FrameWriteOutcome, PRESS_TIMEOUT, TabExtraction, TabGroupRead,
        TabMemberRead, WriteStatus, extract_tabs, read_frame, read_tab_group, resizable,
        write_frame,
    };

    fn window(fake: &FakeWindowAx) -> NodeId {
        fake.add_node(FakeNode {
            position: Ok(AxPoint { x: -1440.0, y: -30.0 }),
            size: Ok(AxSize { width: 800.0, height: 600.0 }),
            size_settable: Ok(true),
            ..FakeNode::default()
        })
    }

    #[test]
    fn a_frame_read_sets_the_query_timeout_and_returns_both_halves() {
        let fake = FakeWindowAx::new();
        let id = window(&fake);
        let element = fake.element(id);
        let read = read_frame(&fake, &element);
        let frame = read.complete().expect("both halves");
        assert_eq!(frame.origin, AxPoint { x: -1440.0, y: -30.0 });
        assert_eq!(frame.size, AxSize { width: 800.0, height: 600.0 });
        assert_eq!(fake.calls()[0], Call::SetTimeout(id, ELEMENT_TIMEOUT));
    }

    #[test]
    fn missing_or_malformed_geometry_is_unknown_never_a_zero_frame() {
        let fake = FakeWindowAx::new();
        let missing = fake.add_node(FakeNode::default());
        let element = fake.element(missing);
        let read = read_frame(&fake, &element);
        assert_eq!(read.position, AxReading::Unsupported);
        assert_eq!(read.size, AxReading::Unsupported);
        assert_eq!(read.complete(), None);

        let malformed = fake.add_node(FakeNode {
            position: Ok(AxPoint { x: 1.0, y: 2.0 }),
            size: Err(AxError::WrongType),
            ..FakeNode::default()
        });
        let element = fake.element(malformed);
        let read = read_frame(&fake, &element);
        assert_eq!(read.position, AxReading::Value(AxPoint { x: 1.0, y: 2.0 }));
        assert_eq!(read.size, AxReading::Unknown(AxError::WrongType));
        assert_eq!(read.complete(), None, "one half is not a frame");

        let timed_out = fake.add_node(FakeNode {
            position: Err(AxError::CannotComplete),
            ..FakeNode::default()
        });
        let element = fake.element(timed_out);
        assert_eq!(
            read_frame(&fake, &element).position,
            AxReading::Unknown(AxError::CannotComplete)
        );
    }

    #[test]
    fn a_failed_timeout_leaves_the_frame_unknown_and_reads_nothing() {
        let fake = FakeWindowAx::new();
        let id = window(&fake);
        fake.edit(id, |node| node.timeout_result = Err(AxError::InvalidElement));
        let element = fake.element(id);
        let read = read_frame(&fake, &element);
        assert_eq!(read.position, AxReading::Unknown(AxError::InvalidElement));
        assert_eq!(read.complete(), None);
        assert_eq!(fake.calls().len(), 1, "no attribute was read");
    }

    #[test]
    fn resizability_is_the_size_settable_flag_and_unknown_stays_unknown() {
        let fake = FakeWindowAx::new();
        let id = window(&fake);
        let element = fake.element(id);
        assert_eq!(resizable(&fake, &element), AxReading::Value(true));
        fake.edit(id, |node| node.size_settable = Ok(false));
        assert_eq!(resizable(&fake, &element), AxReading::Value(false));
        fake.edit(id, |node| {
            node.size_settable = Err(AxError::AttributeUnsupported);
        });
        assert_eq!(resizable(&fake, &element), AxReading::Unsupported);
        fake.edit(id, |node| node.size_settable = Err(AxError::CannotComplete));
        assert_eq!(
            resizable(&fake, &element),
            AxReading::Unknown(AxError::CannotComplete)
        );
    }

    #[test]
    fn writes_are_separate_calls_position_first_after_the_action_timeout() {
        let fake = FakeWindowAx::new();
        let id = window(&fake);
        let element = fake.element(id);
        let target = AxPoint { x: 10.0, y: 20.0 };
        let size = AxSize { width: 300.0, height: 200.0 };
        let outcome = write_frame(&fake, &element, Some(target), Some(size));
        assert_eq!(outcome, FrameWriteOutcome::Attempted {
            position: WriteStatus::Written,
            size: WriteStatus::Written,
        });
        assert!(outcome.is_complete() && !outcome.is_partial());
        assert_eq!(fake.calls(), [
            Call::SetTimeout(id, PRESS_TIMEOUT),
            Call::SetPosition(id, target),
            Call::SetSize(id, size),
        ]);
    }

    #[test]
    fn a_partial_write_reports_each_half_and_leaves_the_applied_one_observable() {
        let fake = FakeWindowAx::new();
        let id = window(&fake);
        fake.edit(id, |node| node.set_size_result = Err(AxError::CannotComplete));
        let element = fake.element(id);
        let outcome = write_frame(
            &fake,
            &element,
            Some(AxPoint { x: 5.0, y: 6.0 }),
            Some(AxSize { width: 1.0, height: 2.0 }),
        );
        assert_eq!(outcome, FrameWriteOutcome::Attempted {
            position: WriteStatus::Written,
            size: WriteStatus::Failed(AxError::CannotComplete),
        });
        assert!(outcome.is_partial() && !outcome.is_complete());
        assert_eq!(
            read_frame(&fake, &element).position,
            AxReading::Value(AxPoint { x: 5.0, y: 6.0 }),
            "the applied half is observable by reading back"
        );

        fake.edit(id, |node| {
            node.set_size_result = Ok(());
            node.set_position_result = Err(AxError::ApiDisabled);
        });
        let outcome = write_frame(&fake, &element, Some(AxPoint { x: 0.0, y: 0.0 }), None);
        assert_eq!(outcome, FrameWriteOutcome::Attempted {
            position: WriteStatus::Failed(AxError::ApiDisabled),
            size: WriteStatus::NotRequested,
        });
        assert!(!outcome.is_partial() && !outcome.is_complete());
    }

    #[test]
    fn a_failed_action_timeout_sends_nothing() {
        let fake = FakeWindowAx::new();
        let id = window(&fake);
        fake.edit(id, |node| node.timeout_result = Err(AxError::InvalidElement));
        let element = fake.element(id);
        let outcome = write_frame(&fake, &element, Some(AxPoint { x: 1.0, y: 1.0 }), None);
        assert_eq!(outcome, FrameWriteOutcome::NotSent(AxError::InvalidElement));
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn flags_strings_and_membership_read_explicitly() {
        let fake = FakeWindowAx::new();
        let child = window(&fake);
        let app = fake.add_node(FakeNode {
            windows: Ok(vec![child]),
            focused: Ok(child),
            main: Err(AxError::NoValue),
            ..FakeNode::default()
        });
        fake.edit(child, |node| {
            node.bools.insert(BoolAttribute::Minimized, Ok(true));
            node.bools.insert(BoolAttribute::FullScreen, Err(AxError::WrongType));
            node.strings.insert(WindowString::Subrole, Ok("AXStandardWindow".to_owned()));
        });
        let application = fake.element(app);
        let window_element = fake.element(child);
        assert_eq!(fake.window_count(&application), Ok(1));
        let windows = fake.windows(&application, 0, 64).expect("windows");
        assert!(fake.same_element(&windows[0], &window_element));
        assert!(fake.same_element(
            &fake.focused_window(&application).expect("focused"),
            &window_element
        ));
        assert_eq!(fake.main_window(&application).err(), Some(AxError::NoValue));
        assert_eq!(
            fake.bool_attribute(&window_element, BoolAttribute::Minimized),
            Ok(true)
        );
        assert_eq!(
            fake.bool_attribute(&window_element, BoolAttribute::FullScreen),
            Err(AxError::WrongType)
        );
        assert_eq!(
            fake.bool_attribute(&window_element, BoolAttribute::Hidden),
            Err(AxError::AttributeUnsupported)
        );
        assert_eq!(
            fake.window_string(&window_element, WindowString::Subrole).as_deref(),
            Ok("AXStandardWindow")
        );
    }

    #[test]
    fn the_window_id_symbol_missing_is_unsupported_not_a_zero_id() {
        let fake = FakeWindowAx::without_window_id_symbol();
        let id = fake.add_node(FakeNode {
            window_id: Ok(99),
            ..FakeNode::default()
        });
        let element = fake.element(id);
        assert_eq!(fake.window_id_capability(), WindowIdCapability::Unsupported);
        assert_eq!(fake.window_id(&element), Err(WindowIdError::Unsupported));

        let present = FakeWindowAx::new();
        let id = present.add_node(FakeNode {
            window_id: Ok(99),
            ..FakeNode::default()
        });
        let element = present.element(id);
        assert_eq!(present.window_id_capability(), WindowIdCapability::Available);
        assert_eq!(present.window_id(&element), Ok(99));
    }

    #[test]
    fn tab_extraction_reads_the_tab_group_without_claiming_support() {
        let fake = FakeWindowAx::new();
        let tab_a = fake.add_node(FakeNode::default());
        let tab_b = fake.add_node(FakeNode::default());
        let other = fake.add_node(FakeNode {
            strings: HashMap::from([(WindowString::Role, Ok("AXButton".to_owned()))]),
            ..FakeNode::default()
        });
        let group = fake.add_node(FakeNode {
            strings: HashMap::from([(WindowString::Role, Ok("AXTabGroup".to_owned()))]),
            tabs: Ok(vec![tab_a, tab_b]),
            ..FakeNode::default()
        });
        let win = fake.add_node(FakeNode {
            children: Ok(vec![other, group]),
            ..FakeNode::default()
        });
        let element = fake.element(win);
        match extract_tabs(&fake, &element, 64) {
            TabExtraction::Tabs(tabs) => {
                assert_eq!(tabs.iter().map(FakeWindowElement::node).collect::<Vec<_>>(), [
                    tab_a, tab_b
                ]);
            }
            other => panic!("expected tabs, got {other:?}"),
        }
        assert_eq!(fake.live_elements(), 1, "every extracted element was released");
    }

    #[test]
    fn tab_group_read_reports_ids_and_selection_with_explicit_unknowns() {
        let fake = FakeWindowAx::new();
        let selected = fake.add_node(FakeNode {
            window_id: Ok(11),
            selected: Ok(true),
            ..FakeNode::default()
        });
        let other = fake.add_node(FakeNode {
            window_id: Ok(12),
            selected: Ok(false),
            ..FakeNode::default()
        });
        let unreadable = fake.add_node(FakeNode {
            window_id: Err(WindowIdError::Unsupported),
            selected: Err(AxError::WrongType),
            ..FakeNode::default()
        });
        let group = fake.add_node(FakeNode {
            strings: HashMap::from([(WindowString::Role, Ok("AXTabGroup".to_owned()))]),
            tabs: Ok(vec![selected, other, unreadable]),
            ..FakeNode::default()
        });
        let win = fake.add_node(FakeNode {
            children: Ok(vec![group]),
            ..FakeNode::default()
        });
        let group_read = read_tab_group(&fake, &fake.element(win), 8);
        assert_eq!(
            group_read,
            TabGroupRead::Group(vec![
                TabMemberRead {
                    cg_window_id: Some(11),
                    selected: AxReading::Value(true)
                },
                TabMemberRead {
                    cg_window_id: Some(12),
                    selected: AxReading::Value(false)
                },
                TabMemberRead {
                    cg_window_id: None,
                    selected: AxReading::Unknown(AxError::WrongType)
                },
            ])
        );
        let limited = read_tab_group(&fake, &fake.element(win), 2);
        assert!(matches!(limited, TabGroupRead::Group(members) if members.len() == 2));
        let timed_out = fake.add_node(FakeNode {
            children: Err(AxError::CannotComplete),
            ..FakeNode::default()
        });
        assert_eq!(
            read_tab_group(&fake, &fake.element(timed_out), 8),
            TabGroupRead::Unknown(AxError::CannotComplete)
        );
        assert_eq!(fake.live_elements(), 0, "every read element was released");
    }

    #[test]
    fn tab_extraction_distinguishes_no_group_from_unknown() {
        let fake = FakeWindowAx::new();
        let plain = fake.add_node(FakeNode {
            children: Ok(Vec::new()),
            ..FakeNode::default()
        });
        let no_children = fake.add_node(FakeNode::default());
        let timed_out = fake.add_node(FakeNode {
            children: Err(AxError::CannotComplete),
            ..FakeNode::default()
        });
        let broken_group = fake.add_node(FakeNode {
            strings: HashMap::from([(WindowString::Role, Ok("AXTabGroup".to_owned()))]),
            tabs: Err(AxError::WrongType),
            ..FakeNode::default()
        });
        let win_broken = fake.add_node(FakeNode {
            children: Ok(vec![broken_group]),
            ..FakeNode::default()
        });
        assert!(matches!(
            extract_tabs(&fake, &fake.element(plain), 8),
            TabExtraction::NoTabGroup
        ));
        assert!(matches!(
            extract_tabs(&fake, &fake.element(no_children), 8),
            TabExtraction::NoTabGroup
        ));
        assert!(matches!(
            extract_tabs(&fake, &fake.element(timed_out), 8),
            TabExtraction::Unknown(AxError::CannotComplete)
        ));
        assert!(matches!(
            extract_tabs(&fake, &fake.element(win_broken), 8),
            TabExtraction::Unknown(AxError::WrongType)
        ));
    }
}
