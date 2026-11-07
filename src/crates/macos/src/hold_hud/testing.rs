//! Deterministic HUD backend double.
//!
//! [`FakeHudBackend`] implements [`HudBackend`] without touching `AppKit`: it
//! records every call, uses the real pure placement, layout and progress
//! functions, and lets a test inject install, show and release failures,
//! panics and a pre-order hook (a barrier that runs immediately before the
//! authorization gate and ordering). Pair it with
//! [`ManualExecutor`](crate::ManualExecutor) for queued or never-draining main
//! executors. No test using it creates a native panel.

// The fake holds its state mutex while assembling one result; there is no
// contention to reduce and splitting the guards would obscure the double.
#![allow(clippy::significant_drop_tightening)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{
    HudAnchor, HudAppearance, HudBackend, HudHandle, HudInstallError, HudInstallInfo, HudMotion,
    HudPlacement, HudRect, HudReleaseError, HudScreens, HudShowCommand, HudShowFailure, HudShown,
    PressId, ProgressPlan, compute_layout, effective_motion, motion_frames, progress_plan,
    select_screen,
};

/// The placement and motion one show requested, as the fake resolved it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FakeShowMotion {
    /// The press.
    pub press: PressId,
    /// The requested anchor.
    pub anchor: HudAnchor,
    /// The requested motion.
    pub motion: HudMotion,
    /// The motion after Reduce Motion (a slide becomes a fade).
    pub effective: HudMotion,
    /// The start frame of the show animation.
    pub start: HudRect,
    /// The final frame.
    pub end: HudRect,
}

/// One recorded backend call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeHudCall {
    /// `install` for a handle.
    Install(HudHandle),
    /// `show` reached the backend.
    Show {
        /// The owner.
        handle: HudHandle,
        /// The press.
        press: PressId,
    },
    /// Native ordering front happened for a press.
    Order {
        /// The owner.
        handle: HudHandle,
        /// The press.
        press: PressId,
    },
    /// `hide` reached the backend.
    Hide {
        /// The owner.
        handle: HudHandle,
        /// The press.
        press: PressId,
    },
    /// `freeze` reached the backend.
    Freeze {
        /// The owner.
        handle: HudHandle,
        /// The press.
        press: PressId,
    },
    /// `release` reached the backend.
    Release(HudHandle),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PanicAt {
    Install,
    Show,
    Release,
}

fn take_panic(panics: &mut Vec<PanicAt>, at: PanicAt) -> bool {
    panics.iter().position(|p| *p == at).is_some_and(|index| {
        panics.remove(index);
        true
    })
}

#[derive(Default)]
struct FakeState {
    screens: HudScreens,
    appearance: HudAppearance,
    install_failures: VecDeque<HudInstallError>,
    show_failures: VecDeque<HudShowFailure>,
    release_failures: VecDeque<String>,
    release_always_fails: bool,
    panics: Vec<PanicAt>,
    pre_order: Option<Arc<dyn Fn() + Send + Sync>>,
    post_order: Option<Arc<dyn Fn() + Send + Sync>>,
    calls: Vec<FakeHudCall>,
    created: usize,
    live: BTreeMap<HudHandle, Option<PressId>>,
    animating: BTreeMap<HudHandle, bool>,
    last_shown: Option<HudShown>,
    shows: Vec<FakeShowMotion>,
}

/// A scriptable [`HudBackend`] with no native dependency.
#[derive(Clone, Default)]
pub struct FakeHudBackend {
    state: Arc<Mutex<FakeState>>,
}

impl FakeHudBackend {
    /// Creates a fake with no screens, so installation reports no screen.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Creates a fake that reports `screens`.
    #[must_use]
    pub fn with_screens(screens: HudScreens) -> Self {
        let fake = Self::default();
        fake.set_screens(screens);
        fake
    }

    /// Replaces the screens used by later calls.
    pub fn set_screens(&self, screens: HudScreens) { lock(&self.state).screens = screens; }

    /// Replaces the reported accessibility options.
    pub fn set_appearance(&self, appearance: HudAppearance) {
        lock(&self.state).appearance = appearance;
    }

    /// Fails the next installation with `error` (no panel is created).
    pub fn fail_next_install(&self, error: HudInstallError) {
        lock(&self.state).install_failures.push_back(error);
    }

    /// Fails the next show with `failure` (nothing is ordered).
    pub fn fail_next_show(&self, failure: HudShowFailure) {
        lock(&self.state).show_failures.push_back(failure);
    }

    /// Fails the next release; the panel stays owned.
    pub fn fail_next_release(&self, message: impl Into<String>) {
        lock(&self.state).release_failures.push_back(message.into());
    }

    /// Makes every release fail until switched off.
    pub fn set_release_always_fails(&self, fails: bool) {
        lock(&self.state).release_always_fails = fails;
    }

    /// Makes the next install panic.
    pub fn panic_next_install(&self) { lock(&self.state).panics.push(PanicAt::Install); }

    /// Makes the next show panic.
    pub fn panic_next_show(&self) { lock(&self.state).panics.push(PanicAt::Show); }

    /// Makes the next release panic.
    pub fn panic_next_release(&self) { lock(&self.state).panics.push(PanicAt::Release); }

    /// Sets a hook that runs inside `show`, after placement and layout and
    /// before the authorization gate and ordering. It runs without the fake's
    /// lock held, so it may call back into the control.
    pub fn set_pre_order(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        lock(&self.state).pre_order = hook;
    }

    /// Sets a hook that runs inside `show` right after native ordering
    /// succeeded and before the result returns to the control. It runs without
    /// the fake's lock held.
    pub fn set_post_order(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        lock(&self.state).post_order = hook;
    }

    /// Every recorded call in order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeHudCall> { lock(&self.state).calls.clone() }

    /// How many panels were ever created.
    #[must_use]
    pub fn panels_created(&self) -> usize { lock(&self.state).created }

    /// How many panels are currently owned.
    #[must_use]
    pub fn panels_live(&self) -> usize { lock(&self.state).live.len() }

    /// The press currently ordered front for `handle`.
    #[must_use]
    pub fn displayed(&self, handle: HudHandle) -> Option<PressId> {
        lock(&self.state).live.get(&handle).copied().flatten()
    }

    /// Whether a Core Animation would be running for `handle`.
    #[must_use]
    pub fn animation_active(&self, handle: HudHandle) -> bool {
        lock(&self.state).animating.get(&handle).copied().unwrap_or(false)
    }

    /// The anchor and motion of every successful show, in order. A hide is
    /// modelled as an immediate order-out, so tests stay deterministic.
    #[must_use]
    pub fn shows(&self) -> Vec<FakeShowMotion> { lock(&self.state).shows.clone() }

    /// The last successful show report.
    #[must_use]
    pub fn last_shown(&self) -> Option<HudShown> { lock(&self.state).last_shown.clone() }

    /// How many ordering calls happened.
    #[must_use]
    pub fn order_count(&self) -> usize {
        lock(&self.state)
            .calls
            .iter()
            .filter(|c| matches!(c, FakeHudCall::Order { .. }))
            .count()
    }

    /// How many release calls reached the backend.
    #[must_use]
    pub fn release_count(&self) -> usize {
        lock(&self.state)
            .calls
            .iter()
            .filter(|c| matches!(c, FakeHudCall::Release(_)))
            .count()
    }
}

impl HudBackend for FakeHudBackend {
    fn install(&self, handle: HudHandle) -> Result<HudInstallInfo, HudInstallError> {
        let mut state = lock(&self.state);
        state.calls.push(FakeHudCall::Install(handle));
        assert!(
            !take_panic(&mut state.panics, PanicAt::Install),
            "scripted install panic"
        );
        if let Some(error) = state.install_failures.pop_front() {
            return Err(error);
        }
        if state.screens.screens.is_empty() {
            return Err(HudInstallError::NoScreen);
        }
        if !state.live.contains_key(&handle) {
            state.created += 1;
            state.live.insert(handle, None);
            state.animating.insert(handle, false);
        }
        Ok(HudInstallInfo {
            screens: state.screens.screens.len(),
        })
    }

    fn show(
        &self,
        handle: HudHandle,
        command: &HudShowCommand,
        gate: &dyn Fn() -> bool,
    ) -> Result<HudShown, HudShowFailure> {
        let (hook, screens, appearance) = {
            let mut state = lock(&self.state);
            state.calls.push(FakeHudCall::Show { handle, press: command.press });
            assert!(
                !take_panic(&mut state.panics, PanicAt::Show),
                "scripted show panic"
            );
            if let Some(failure) = state.show_failures.pop_front() {
                return Err(failure);
            }
            if !state.live.contains_key(&handle) {
                return Err(HudShowFailure::NotInstalled);
            }
            (state.pre_order.clone(), state.screens.clone(), state.appearance)
        };
        let (screen, source) =
            select_screen(&screens, command.display).map_err(|_| HudShowFailure::NoScreen)?;
        let layout = compute_layout(screen.visible, command.label.as_deref(), command.anchor);
        let plan = progress_plan(command.elapsed_ms, command.duration_ms);
        if let Some(hook) = hook {
            hook();
        }
        if !gate() {
            return Err(HudShowFailure::Stale);
        }
        let (shown, post) = {
            let mut state = lock(&self.state);
            state.calls.push(FakeHudCall::Order { handle, press: command.press });
            state.live.insert(handle, Some(command.press));
            state.animating.insert(handle, matches!(plan, ProgressPlan::Animate { .. }));
            let shown = HudShown {
                placement: HudPlacement { display: screen.id, source },
                text: layout.text,
                panel: layout.panel,
                plan,
                appearance,
            };
            let reduce = appearance.reduce_motion;
            let (start, end) = motion_frames(shown.panel, command.anchor, command.motion, reduce);
            state.shows.push(FakeShowMotion {
                press: command.press,
                anchor: command.anchor,
                motion: command.motion,
                effective: effective_motion(command.motion, reduce),
                start,
                end,
            });
            state.last_shown = Some(shown.clone());
            (shown, state.post_order.clone())
        };
        if let Some(post) = post {
            post();
        }
        Ok(shown)
    }

    fn hide(&self, handle: HudHandle, press: PressId) {
        let mut state = lock(&self.state);
        state.calls.push(FakeHudCall::Hide { handle, press });
        if state.live.get(&handle).copied().flatten() == Some(press) {
            state.live.insert(handle, None);
            state.animating.insert(handle, false);
        }
    }

    fn freeze(&self, handle: HudHandle, press: PressId) {
        let mut state = lock(&self.state);
        state.calls.push(FakeHudCall::Freeze { handle, press });
        if state.live.get(&handle).copied().flatten() == Some(press) {
            state.animating.insert(handle, false);
        }
    }

    fn release(&self, handle: HudHandle) -> Result<(), HudReleaseError> {
        let mut state = lock(&self.state);
        state.calls.push(FakeHudCall::Release(handle));
        assert!(
            !take_panic(&mut state.panics, PanicAt::Release),
            "scripted release panic"
        );
        if let Some(message) = state.release_failures.pop_front() {
            return Err(HudReleaseError(message));
        }
        if state.release_always_fails {
            return Err(HudReleaseError("scripted release failure".to_owned()));
        }
        state.live.remove(&handle);
        state.animating.remove(&handle);
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
