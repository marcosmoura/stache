//! A fake Carbon/TIS layer. It never registers a hotkey, installs a handler,
//! reads a layout or touches a permission: every effect is a recorded value.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the fake holds one short-lived lock per scripted step"
)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::native::{
    Dispatch, Disposition, Native, NativeFactory, NativeHotKeyId, RawEvent, RawEventKind,
    RegisterFailure, SIGNATURE, STATUS_HOT_KEY_EXISTS,
};
use super::types::{BindingToken, HotkeySink};
use crate::keyboard_layout::resolver::{CARBON_OPTION, CARBON_SHIFT};
use crate::keyboard_layout::{KeyLayout, LayoutId, LayoutRead};

/// One native operation the fake performed (or was asked to perform).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// `InstallEventHandler`.
    InstallHandler,
    /// `RemoveEventHandler`.
    RemoveHandler,
    /// `RegisterEventHotKey`.
    Register {
        /// Virtual key code.
        key_code: u16,
        /// Carbon modifier mask.
        modifiers: u32,
        /// Native ID.
        id: u32,
    },
    /// `UnregisterEventHotKey`.
    Unregister(u32),
    /// A layout read.
    ReadLayout,
    /// Observer install.
    Observe,
    /// Observer removal.
    Unobserve,
}

/// A table-driven fake keyboard layout.
#[derive(Clone, Debug)]
pub struct FakeLayout {
    id: u64,
    keys: HashMap<(u16, u32), u16>,
}

impl FakeLayout {
    /// A layout with identity `id` that types the given `(key, mods) -> unit`.
    #[must_use]
    pub fn new(id: u64, keys: impl IntoIterator<Item = ((u16, u32), u16)>) -> Self {
        Self {
            id,
            keys: keys.into_iter().collect(),
        }
    }

    /// US ANSI: `Option+6` types `§`.
    #[must_use]
    pub fn ansi() -> Self {
        Self::new(1, [
            ((0x16, CARBON_OPTION), 0x00A7),
            ((0x16, 0), u16::from(b'6')),
        ])
    }

    /// ISO: the dedicated key `0x0A` types `§` unmodified.
    #[must_use]
    pub fn iso() -> Self {
        Self::new(2, [((0x0A, 0), 0x00A7), ((0x0A, CARBON_SHIFT), u16::from(b'>'))])
    }

    /// German: `Shift+3` types `§`.
    #[must_use]
    pub fn german() -> Self {
        Self::new(3, [((0x14, CARBON_SHIFT), 0x00A7), ((0x14, 0), u16::from(b'3'))])
    }

    /// A layout with no `§` anywhere.
    #[must_use]
    pub fn without_section() -> Self { Self::new(4, [((0x00, 0), u16::from(b'a'))]) }
}

impl KeyLayout for FakeLayout {
    fn identity(&self) -> LayoutId { LayoutId(self.id) }

    fn translate(&self, key_code: u16, carbon_modifiers: u32) -> Option<u16> {
        self.keys.get(&(key_code, carbon_modifiers)).copied()
    }
}

type Hook = Box<dyn FnMut(&Call) + Send>;

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    registered: HashMap<u32, (u16, u32)>,
    occupied: HashSet<(u16, u32)>,
    handler_installed: bool,
    fail_install: Option<i32>,
    fail_remove_handler: Option<i32>,
    /// Native IDs whose unregistration keeps failing.
    stuck: HashSet<u32>,
    unregister_failures: usize,
    layout: Option<FakeLayout>,
    layout_error: Option<String>,
    observer: Option<Arc<dyn Fn() + Send + Sync>>,
    fail_observe: bool,
    fail_unobserve: bool,
    hook: Option<Hook>,
}

/// The fake's scripted failures and recorded observations.
pub struct FakeLog {
    id: u64,
    state: Mutex<State>,
}

static NEXT_LOG: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static HANDLERS: RefCell<HashMap<u64, Dispatch>> = RefCell::new(HashMap::new());
}

impl FakeLog {
    /// A fresh log with an ANSI layout.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_LOG.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(State {
                layout: Some(FakeLayout::ansi()),
                ..State::default()
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The factory a control uses to create the fake on the main thread.
    #[must_use]
    pub fn factory(self: &Arc<Self>) -> NativeFactory {
        let log = Arc::clone(self);
        Arc::new(move || {
            Box::new(FakeNative {
                log: Arc::clone(&log),
                _main_only: PhantomData,
            })
        })
    }

    /// Every recorded call, oldest first.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> { self.state().calls.clone() }

    /// Forgets recorded calls.
    pub fn clear_calls(&self) { self.state().calls.clear(); }

    /// Number of `Register` calls so far.
    #[must_use]
    pub fn register_count(&self) -> usize {
        self.calls().iter().filter(|c| matches!(c, Call::Register { .. })).count()
    }

    /// Number of layout reads so far.
    #[must_use]
    pub fn layout_reads(&self) -> usize {
        self.calls().iter().filter(|c| **c == Call::ReadLayout).count()
    }

    /// Currently registered `(key_code, modifiers)` pairs, sorted.
    #[must_use]
    pub fn registered_chords(&self) -> Vec<(u16, u32)> {
        let mut chords: Vec<_> = self.state().registered.values().copied().collect();
        chords.sort_unstable();
        chords
    }

    /// Currently registered native IDs, sorted.
    #[must_use]
    pub fn registered_ids(&self) -> Vec<u32> {
        let mut ids: Vec<_> = self.state().registered.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The native ID registered for a chord.
    #[must_use]
    pub fn id_of(&self, key_code: u16, modifiers: u32) -> Option<u32> {
        self.state()
            .registered
            .iter()
            .find(|(_, chord)| **chord == (key_code, modifiers))
            .map(|(id, _)| *id)
    }

    /// Whether the handler is installed.
    #[must_use]
    pub fn handler_installed(&self) -> bool { self.state().handler_installed }

    /// Whether the layout observer is installed.
    #[must_use]
    pub fn observing(&self) -> bool { self.state().observer.is_some() }

    /// Makes someone else hold a chord.
    pub fn occupy(&self, key_code: u16, modifiers: u32) {
        self.state().occupied.insert((key_code, modifiers));
    }

    /// Makes handler installation fail with `status`.
    pub fn fail_install(&self, status: Option<i32>) { self.state().fail_install = status; }

    /// Makes handler removal fail with `status`.
    pub fn fail_remove_handler(&self, status: Option<i32>) {
        self.state().fail_remove_handler = status;
    }

    /// Makes the next `n` unregistrations fail.
    pub fn fail_unregisters(&self, n: usize) { self.state().unregister_failures = n; }

    /// Makes unregistering `id` fail until [`Self::unstick`].
    pub fn stick(&self, id: u32) { self.state().stuck.insert(id); }

    /// Lets `id` unregister again.
    pub fn unstick(&self, id: u32) { self.state().stuck.remove(&id); }

    /// Makes observer install fail.
    pub fn fail_observe(&self, fail: bool) { self.state().fail_observe = fail; }

    /// Makes observer removal fail.
    pub fn fail_unobserve(&self, fail: bool) { self.state().fail_unobserve = fail; }

    /// Switches the keyboard layout (does not notify; see [`Self::fire_hint`]).
    pub fn set_layout(&self, layout: FakeLayout) {
        let mut state = self.state();
        state.layout = Some(layout);
        state.layout_error = None;
    }

    /// Makes the layout unreadable.
    pub fn set_layout_unreadable(&self, reason: &str) {
        let mut state = self.state();
        state.layout = None;
        state.layout_error = Some(reason.to_owned());
    }

    /// Runs `hook` before every native call, outside the log's lock. It must not
    /// call back into the control.
    pub fn set_hook(&self, hook: impl FnMut(&Call) + Send + 'static) {
        self.state().hook = Some(Box::new(hook));
    }

    /// Removes the hook.
    pub fn clear_hook(&self) { self.state().hook = None; }

    /// Simulates the distributed notification, as the real observer would: it
    /// only calls the installed hint. Returns whether an observer was installed.
    pub fn fire_hint(&self) -> bool {
        let hint = self.state().observer.clone();
        hint.is_some_and(|hint| {
            hint();
            true
        })
    }

    /// Delivers an event to the installed handler on the current thread, which
    /// plays the main thread. `Forward` when no handler is installed.
    pub fn deliver(&self, event: RawEvent) -> Disposition {
        let dispatch = HANDLERS.with(|cell| cell.borrow().get(&self.id).cloned());
        dispatch.map_or(Disposition::Forward, |dispatch| dispatch(event))
    }

    /// Delivers a press of native `id` with the Stache signature.
    pub fn press(&self, id: u32) -> Disposition {
        self.deliver(RawEvent {
            kind: RawEventKind::Pressed,
            hot_key: Ok((SIGNATURE, id)),
        })
    }

    /// Delivers a release of native `id`.
    pub fn release(&self, id: u32) -> Disposition {
        self.deliver(RawEvent {
            kind: RawEventKind::Released,
            hot_key: Ok((SIGNATURE, id)),
        })
    }

    fn record(&self, call: Call) {
        let hook = {
            let mut state = self.state();
            state.calls.push(call);
            state.hook.take()
        };
        if let Some(mut hook) = hook {
            hook(&call);
            let mut state = self.state();
            if state.hook.is_none() {
                state.hook = Some(hook);
            }
        }
    }
}

/// The fake native layer. `!Send`, like the real one.
pub struct FakeNative {
    log: Arc<FakeLog>,
    _main_only: PhantomData<Rc<()>>,
}

impl Native for FakeNative {
    fn install_handler(&mut self, dispatch: Dispatch) -> Result<(), i32> {
        self.log.record(Call::InstallHandler);
        let mut state = self.log.state();
        if let Some(status) = state.fail_install {
            return Err(status);
        }
        state.handler_installed = true;
        drop(state);
        let _ = HANDLERS.try_with(|cell| cell.borrow_mut().insert(self.log.id, dispatch));
        Ok(())
    }

    fn remove_handler(&mut self) -> Result<(), i32> {
        self.log.record(Call::RemoveHandler);
        let mut state = self.log.state();
        if let Some(status) = state.fail_remove_handler {
            return Err(status);
        }
        state.handler_installed = false;
        drop(state);
        let _ = HANDLERS.try_with(|cell| cell.borrow_mut().remove(&self.log.id));
        Ok(())
    }

    fn register(
        &mut self,
        key_code: u16,
        carbon_modifiers: u32,
        id: NativeHotKeyId,
    ) -> Result<(), RegisterFailure> {
        self.log.record(Call::Register {
            key_code,
            modifiers: carbon_modifiers,
            id: id.0,
        });
        let mut state = self.log.state();
        let chord = (key_code, carbon_modifiers);
        if state.occupied.contains(&chord) || state.registered.values().any(|c| *c == chord) {
            return Err(RegisterFailure::Occupied(STATUS_HOT_KEY_EXISTS));
        }
        state.registered.insert(id.0, chord);
        Ok(())
    }

    fn unregister(&mut self, id: NativeHotKeyId) -> Result<(), i32> {
        self.log.record(Call::Unregister(id.0));
        let mut state = self.log.state();
        if state.stuck.contains(&id.0) {
            return Err(-1);
        }
        if state.unregister_failures > 0 {
            state.unregister_failures -= 1;
            return Err(-2);
        }
        state.registered.remove(&id.0);
        Ok(())
    }

    fn read_layout(&mut self) -> LayoutRead {
        self.log.record(Call::ReadLayout);
        let state = self.log.state();
        state.layout.clone().map_or_else(
            || {
                LayoutRead::Unavailable(
                    state.layout_error.clone().unwrap_or_else(|| "no layout".to_owned()),
                )
            },
            |layout| LayoutRead::Available(Box::new(layout)),
        )
    }

    fn observe_layout(&mut self, hint: Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
        self.log.record(Call::Observe);
        let mut state = self.log.state();
        if state.fail_observe {
            return Err("scripted observer failure".to_owned());
        }
        state.observer = Some(hint);
        Ok(())
    }

    fn unobserve_layout(&mut self) -> Result<(), String> {
        self.log.record(Call::Unobserve);
        let mut state = self.log.state();
        if state.fail_unobserve {
            return Err("scripted observer removal failure".to_owned());
        }
        state.observer = None;
        Ok(())
    }
}

/// A sink that records every accepted press.
#[derive(Default)]
pub struct RecordingSink {
    pressed: Mutex<Vec<BindingToken>>,
}

impl RecordingSink {
    /// A fresh sink.
    #[must_use]
    pub fn new() -> Arc<Self> { Arc::new(Self::default()) }

    /// Every token received, in order.
    #[must_use]
    pub fn pressed(&self) -> Vec<BindingToken> {
        self.pressed.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The keys received, in order.
    #[must_use]
    pub fn keys(&self) -> Vec<u64> { self.pressed().iter().map(BindingToken::key).collect() }
}

impl HotkeySink for RecordingSink {
    fn pressed(&self, token: BindingToken) {
        self.pressed.lock().unwrap_or_else(PoisonError::into_inner).push(token);
    }
}
