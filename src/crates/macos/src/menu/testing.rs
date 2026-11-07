//! Fakes for the menu reader and AX owner. They never call the real AX API.
//!
//! [`FakeAx`] is an arena tree with a per-call hook (use it as a barrier or to
//! cancel / advance the clock at an exact call), injected errors, a record of
//! every call (with the thread it ran on) and retain/release counters, so tests
//! can prove balance on success, discard, cancellation and partial failure.
//! [`FakeElement`] is `!Send` like the real element.

#![allow(missing_docs, reason = "test fakes are self-describing")]
#![allow(clippy::must_use_candidate, reason = "test fakes")]
#![allow(clippy::missing_panics_doc, reason = "test fakes")]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the fakes hold one short-lived lock per scripted step"
)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::ax::{AxApi, AxError, StringAttribute};
use super::reader::{MenuClock, MenuTarget, TargetCheck, TargetValidator};

pub type NodeId = usize;

/// A nested description of one fake element.
#[derive(Clone, Debug, Default)]
pub struct Node {
    pub title: Option<String>,
    pub role: Option<String>,
    pub enabled: Option<bool>,
    pub mark: Option<String>,
    pub cmd: Option<String>,
    pub mods: Option<i64>,
    pub children: Vec<Self>,
    pub tag: Option<&'static str>,
}

impl Node {
    /// A plain item (`AXMenuItem`).
    pub fn item(title: &str) -> Self {
        Self {
            title: Some(title.to_owned()),
            role: Some("AXMenuItem".to_owned()),
            ..Self::default()
        }
    }

    /// A separator item.
    pub fn separator() -> Self {
        Self {
            title: Some(String::new()),
            role: Some("AXSeparator".to_owned()),
            ..Self::default()
        }
    }

    /// An `AXMenu` holding `items`.
    pub fn menu(items: Vec<Self>) -> Self {
        Self {
            role: Some("AXMenu".to_owned()),
            children: items,
            ..Self::default()
        }
    }

    /// An item with a submenu of `items`.
    pub fn submenu(title: &str, items: Vec<Self>) -> Self {
        Self::item(title).child(Self::menu(items))
    }

    #[must_use]
    pub fn child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub const fn disabled(mut self) -> Self {
        self.enabled = Some(false);
        self
    }

    #[must_use]
    pub fn mark(mut self, mark: &str) -> Self {
        self.mark = Some(mark.to_owned());
        self
    }

    #[must_use]
    pub fn key(mut self, key: &str, mods: Option<i64>) -> Self {
        self.cmd = Some(key.to_owned());
        self.mods = mods;
        self
    }

    #[must_use]
    pub const fn tag(mut self, tag: &'static str) -> Self {
        self.tag = Some(tag);
        self
    }

    #[must_use]
    pub fn without_title(mut self) -> Self {
        self.title = None;
        self
    }

    #[must_use]
    pub fn without_role(mut self) -> Self {
        self.role = None;
        self
    }
}

/// The AX operation a fake call stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    CreateApp,
    SetTimeout,
    MenuBar,
    Role,
    Title,
    MarkChar,
    CmdChar,
    Enabled,
    CmdModifiers,
    ChildCount,
    Children,
    Press,
}

/// One recorded call.
#[derive(Debug, Clone)]
pub struct Call {
    pub index: usize,
    pub op: Op,
    pub node: NodeId,
    pub thread: String,
}

/// A persistent injected error.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub op: Op,
    pub node: Option<NodeId>,
    pub error: AxError,
}

/// Retain/release accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub created: usize,
    pub released: usize,
}

impl Counts {
    pub const fn live(self) -> usize { self.created - self.released }
}

#[derive(Default)]
struct NodeData {
    title: Option<String>,
    role: Option<String>,
    enabled: Option<bool>,
    mark: Option<String>,
    cmd: Option<String>,
    mods: Option<i64>,
    children: Vec<NodeId>,
}

#[derive(Default)]
struct State {
    nodes: Vec<NodeData>,
    tags: HashMap<&'static str, NodeId>,
    menu_bar: Option<NodeId>,
    calls: Vec<Call>,
    created: usize,
    released: usize,
    next_serial: u64,
    with_timeout: HashSet<u64>,
    timeout_nodes: Vec<NodeId>,
    violations: Vec<Call>,
    faults: Vec<Fault>,
}

type Hook = Arc<dyn Fn(&Call) + Send + Sync>;

struct Shared {
    state: Mutex<State>,
    hook: Mutex<Option<Hook>>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An owned fake element; `!Send` like the real one.
pub struct FakeElement {
    node: NodeId,
    serial: u64,
    shared: Arc<Shared>,
    _owner_local: PhantomData<*const ()>,
}

impl Drop for FakeElement {
    fn drop(&mut self) { self.shared.state().released += 1; }
}

/// The fake AX API. The handle is `Send`; its elements are not.
#[derive(Clone)]
pub struct FakeAx {
    shared: Arc<Shared>,
}

const APPLICATION: NodeId = 0;

impl FakeAx {
    /// An application whose menu bar holds `items` (the first is the Apple menu).
    pub fn new(items: Vec<Node>) -> Self {
        let ax = Self::without_menu_bar();
        {
            let mut state = ax.shared.state();
            let bar = Self::add(&mut state, Node::default());
            state.menu_bar = Some(bar);
            for item in items {
                let child = Self::add(&mut state, item);
                state.nodes[bar].children.push(child);
            }
            state.tags.insert("menubar", bar);
        }
        ax
    }

    /// An application with no `AXMenuBar`.
    pub fn without_menu_bar() -> Self {
        let ax = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                hook: Mutex::new(None),
            }),
        };
        {
            let mut state = ax.shared.state();
            Self::add(&mut state, Node {
                role: Some("AXApplication".to_owned()),
                ..Node::default()
            });
        }
        ax
    }

    fn add(state: &mut State, node: Node) -> NodeId {
        let id = state.nodes.len();
        state.nodes.push(NodeData {
            title: node.title,
            role: node.role,
            enabled: node.enabled,
            mark: node.mark,
            cmd: node.cmd,
            mods: node.mods,
            children: Vec::new(),
        });
        if let Some(tag) = node.tag {
            state.tags.insert(tag, id);
        }
        for child in node.children {
            let child = Self::add(state, child);
            state.nodes[id].children.push(child);
        }
        id
    }

    /// Adds `node` (and its descendants) to the arena without linking it.
    pub fn add_node(&self, node: Node) -> NodeId { Self::add(&mut self.shared.state(), node) }

    /// Appends an existing node as another child (to build cycles / aliases).
    pub fn link_child(&self, parent: NodeId, child: NodeId) {
        self.shared.state().nodes[parent].children.push(child);
    }

    /// Looks up a tagged node.
    ///
    /// # Panics
    ///
    /// When the tag does not exist.
    pub fn node(&self, tag: &'static str) -> NodeId { self.shared.state().tags[tag] }

    /// Installs the per-call hook, run before each call is answered, outside
    /// every internal lock, so it may block on a barrier, cancel a token or
    /// advance a clock.
    pub fn set_hook(&self, hook: impl Fn(&Call) + Send + Sync + 'static) {
        *self.shared.hook.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(hook));
    }

    /// Injects a persistent error for `op` (optionally only on `node`).
    pub fn inject(&self, op: Op, node: Option<NodeId>, error: AxError) {
        self.shared.state().faults.push(Fault { op, node, error });
    }

    pub fn counts(&self) -> Counts {
        let state = self.shared.state();
        Counts {
            created: state.created,
            released: state.released,
        }
    }

    pub fn calls(&self) -> Vec<Call> { self.shared.state().calls.clone() }

    pub fn call_count(&self) -> usize { self.shared.state().calls.len() }

    pub fn ops(&self, op: Op) -> Vec<Call> {
        self.shared.state().calls.iter().filter(|call| call.op == op).cloned().collect()
    }

    /// Calls that messaged an element before its timeout was set.
    pub fn timeout_violations(&self) -> Vec<Call> { self.shared.state().violations.clone() }

    /// Nodes whose element objects had a timeout set, in order.
    pub fn timeout_nodes(&self) -> Vec<NodeId> { self.shared.state().timeout_nodes.clone() }

    /// The number of arena nodes (excluding the application).
    pub fn node_count(&self) -> usize { self.shared.state().nodes.len() }

    fn make(&self, node: NodeId) -> FakeElement {
        let mut state = self.shared.state();
        state.created += 1;
        state.next_serial += 1;
        FakeElement {
            node,
            serial: state.next_serial,
            shared: Arc::clone(&self.shared),
            _owner_local: PhantomData,
        }
    }

    /// Records the call, runs the hook, then applies any injected fault.
    fn begin(&self, element: Option<&FakeElement>, op: Op, node: NodeId) -> Result<(), AxError> {
        let (call, fault) = {
            let mut state = self.shared.state();
            let call = Call {
                index: state.calls.len(),
                op,
                node,
                thread: thread::current().name().unwrap_or("?").to_owned(),
            };
            state.calls.push(call.clone());
            if let Some(element) = element
                && !matches!(op, Op::SetTimeout)
                && !state.with_timeout.contains(&element.serial)
            {
                state.violations.push(call.clone());
            }
            let fault = state
                .faults
                .iter()
                .find(|fault| fault.op == op && fault.node.is_none_or(|n| n == node))
                .map(|fault| fault.error);
            (call, fault)
        };
        let hook = self.shared.hook.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(hook) = hook {
            hook(&call);
        }
        fault.map_or(Ok(()), Err)
    }

    fn data<T>(
        &self,
        node: NodeId,
        read: impl FnOnce(&NodeData) -> Option<T>,
    ) -> Result<T, AxError> {
        read(&self.shared.state().nodes[node]).ok_or(AxError::AttributeUnsupported)
    }
}

impl AxApi for FakeAx {
    type Element = FakeElement;

    fn create_application(&self, _pid: i32) -> Result<FakeElement, AxError> {
        self.begin(None, Op::CreateApp, APPLICATION)?;
        Ok(self.make(APPLICATION))
    }

    fn set_messaging_timeout(
        &self,
        element: &FakeElement,
        _timeout: Duration,
    ) -> Result<(), AxError> {
        self.begin(Some(element), Op::SetTimeout, element.node)?;
        let mut state = self.shared.state();
        state.with_timeout.insert(element.serial);
        state.timeout_nodes.push(element.node);
        Ok(())
    }

    fn menu_bar(&self, application: &FakeElement) -> Result<FakeElement, AxError> {
        self.begin(Some(application), Op::MenuBar, application.node)?;
        let bar = self.shared.state().menu_bar.ok_or(AxError::NoValue)?;
        Ok(self.make(bar))
    }

    fn string(&self, element: &FakeElement, attribute: StringAttribute) -> Result<String, AxError> {
        let op = match attribute {
            StringAttribute::Role => Op::Role,
            StringAttribute::Title => Op::Title,
            StringAttribute::MarkChar => Op::MarkChar,
            StringAttribute::CmdChar => Op::CmdChar,
        };
        self.begin(Some(element), op, element.node)?;
        self.data(element.node, |node| match attribute {
            StringAttribute::Role => node.role.clone(),
            StringAttribute::Title => node.title.clone(),
            StringAttribute::MarkChar => node.mark.clone(),
            StringAttribute::CmdChar => node.cmd.clone(),
        })
    }

    fn enabled(&self, element: &FakeElement) -> Result<bool, AxError> {
        self.begin(Some(element), Op::Enabled, element.node)?;
        self.data(element.node, |node| node.enabled)
    }

    fn cmd_modifiers(&self, element: &FakeElement) -> Result<i64, AxError> {
        self.begin(Some(element), Op::CmdModifiers, element.node)?;
        self.data(element.node, |node| node.mods)
    }

    fn child_count(&self, element: &FakeElement) -> Result<usize, AxError> {
        self.begin(Some(element), Op::ChildCount, element.node)?;
        Ok(self.shared.state().nodes[element.node].children.len())
    }

    fn children(&self, element: &FakeElement, limit: usize) -> Result<Vec<FakeElement>, AxError> {
        self.begin(Some(element), Op::Children, element.node)?;
        #[allow(
            clippy::needless_collect,
            reason = "the state lock must be released before make"
        )]
        let ids: Vec<NodeId> = self.shared.state().nodes[element.node]
            .children
            .iter()
            .copied()
            .take(limit)
            .collect();
        Ok(ids.into_iter().map(|id| self.make(id)).collect())
    }

    fn press(&self, element: &FakeElement) -> Result<(), AxError> {
        self.begin(Some(element), Op::Press, element.node)
    }

    fn same_element(&self, a: &FakeElement, b: &FakeElement) -> bool { a.node == b.node }
}

/// A deterministic monotonic clock.
pub struct FakeClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }
}

impl FakeClock {
    pub fn advance(&self, by: Duration) {
        *self.offset.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

impl MenuClock for FakeClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A scripted target validator.
pub struct FakeValidator {
    script: Mutex<VecDeque<TargetCheck>>,
    default: TargetCheck,
    calls: AtomicUsize,
}

impl FakeValidator {
    pub const fn always(check: TargetCheck) -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            default: check,
            calls: AtomicUsize::new(0),
        }
    }

    /// Answers `script` in order, then `Current`.
    pub fn scripted(script: Vec<TargetCheck>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            default: TargetCheck::Current,
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }
}

impl TargetValidator for FakeValidator {
    fn validate(&self, _target: &MenuTarget) -> TargetCheck {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(self.default)
    }
}

const _: () = {
    const fn assert_send<T: Send + 'static>() {}
    assert_send::<FakeAx>();
    assert_send::<FakeClock>();
    assert_send::<FakeValidator>();
};

/// Compile-time proof that the fake element is owner-local.
mod not_send {
    use super::FakeElement;
    use crate::menu::owner_local::{NotSend, NotSync};
    const _: fn() = || {
        <FakeElement as NotSend<_>>::check();
        <FakeElement as NotSync<_>>::check();
    };
}

/// A target with a genuine [`ApplicationId`] from the fake applications backend.
///
/// [`ApplicationId`]: crate::applications::ApplicationId
///
/// # Panics
///
/// When the fake applications backend cannot be installed.
pub fn fake_target(pid: i32) -> MenuTarget {
    use stache_sdk::CancellationToken;

    use crate::applications::{ApplicationBackend, ApplicationControl};
    use crate::{FakeApplicationBackend, ManualExecutor};

    let backend = Arc::new(FakeApplicationBackend::new());
    let control = ApplicationControl::new(
        Arc::new(ManualExecutor::new(true)),
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
    );
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("fake install");
    let _ = backend.add_running_with_pid("com.example.menu-target", pid);
    let application =
        observation.running_applications("com.example.menu-target").expect("running")[0];
    MenuTarget { pid, application }
}
