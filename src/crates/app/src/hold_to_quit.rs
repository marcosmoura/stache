//! The app-owned Hold-to-Quit composition, factory and teardown owners.
//!
//! [`compose`] builds, once per desktop launch, the host-lifetime pieces the
//! Hold-to-Quit module needs and nothing else: one safe [`ApplicationControl`],
//! the [`HudHost`] that owns the HUD backend and any HUD whose release was not
//! acknowledged, and the [`HoldToQuitRuntime`] over them and the host
//! frontmost-context lease. Every piece is inert: no observer, panel, lease,
//! subscription or worker exists until the enabled module starts.
//!
//! [`HoldToQuitFactory`] registers the module through
//! [`register_module_observed`] so the typed accepted-configuration watch is
//! installed before the registry starts modules. The factory has no socket
//! surface, CLI command or window: the only runtime detail, HUD-degraded mode,
//! travels on the generic [`ModuleStatusView::detail`].
//!
//! Two owners acknowledge retained cleanup during coordinated teardown. A
//! module stop that could not prove its release keeps the exact native owner
//! (the factory's [`HudHost`] for the panel, the [`ApplicationControl`] for the
//! observer) and these owners retry it without blocking the coordinator.

use std::sync::Arc;

use stache_hold_to_quit::{
    CommandQuitConfig, FrontmostLeaser, HoldToQuitParts, HoldToQuitRuntime, HudHost, LeaseHandle,
    SystemHoldClock,
};
use stache_macos::ApplicationControl;
use stache_sdk::{CancellationToken, ConfigStore, MainThreadExecutor, ModuleId, Registry};

use crate::bindings::{ModuleState, ModuleStatusView};
use crate::frontmost_context::{FrontmostContext, FrontmostLease, ReleaseOutcome};
use crate::host::{
    HostError, ModuleControl, ModuleFactory, input_priority, register_module_observed,
};
use crate::shutdown::{NativeOwner, OwnerAnswer};

/// What the composition root keeps from [`compose`].
pub struct HoldToQuitComposition {
    /// The service the factory registers.
    pub runtime: HoldToQuitRuntime,
    /// The HUD owner, for the teardown acknowledgement.
    pub hud: Arc<HudHost>,
    /// The application control, for the teardown acknowledgement.
    pub control: ApplicationControl,
}

/// Builds the Hold-to-Quit pieces over the real native layers. Nothing runs.
#[must_use]
pub fn compose(
    main_thread: Arc<dyn MainThreadExecutor>,
    frontmost: FrontmostContext,
) -> HoldToQuitComposition {
    let control = ApplicationControl::system(Arc::clone(&main_thread));
    let hud = Arc::new(HudHost::system(main_thread));
    let runtime = HoldToQuitRuntime::new(HoldToQuitParts {
        control: control.clone(),
        hud: Arc::clone(&hud),
        frontmost: Arc::new(ContextLeaser(frontmost)),
        clock: Arc::new(SystemHoldClock),
        priority: input_priority::HOLD_TO_QUIT,
    });
    HoldToQuitComposition { runtime, hud, control }
}

/// Leases the host frontmost-context publisher for the module.
struct ContextLeaser(FrontmostContext);

impl FrontmostLeaser for ContextLeaser {
    fn lease(&self, cancel: &CancellationToken) -> Result<Box<dyn LeaseHandle>, String> {
        self.0
            .lease(cancel)
            .map(|lease| Box::new(HeldLease(lease)) as Box<dyn LeaseHandle>)
            .map_err(|error| error.to_string())
    }
}

struct HeldLease(FrontmostLease);

impl LeaseHandle for HeldLease {
    fn release(self: Box<Self>) -> Result<(), String> {
        match self.0.release() {
            ReleaseOutcome::Held | ReleaseOutcome::Stopped => Ok(()),
            ReleaseOutcome::Retained(error) => Err(error.to_string()),
        }
    }
}

/// Registers the Hold-to-Quit module over the host-lifetime runtime service.
///
/// Construction is inert. Registration installs the typed accepted-configuration
/// watch before the registry starts modules.
pub struct HoldToQuitFactory {
    runtime: HoldToQuitRuntime,
}

impl HoldToQuitFactory {
    /// Wraps the one host-lifetime runtime service.
    #[must_use]
    pub const fn new(runtime: HoldToQuitRuntime) -> Self { Self { runtime } }
}

impl ModuleFactory for HoldToQuitFactory {
    fn id(&self) -> ModuleId { CommandQuitConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        let runtime = self.runtime.clone();
        register_module_observed(registry, store, runtime.module(), move |registry, handle| {
            runtime.install_accepted_watch(registry.accepted_watch(handle)?);
            Ok(())
        })
    }

    /// HUD-degraded mode keeps the module `Running` and says why on `detail`.
    fn project_status(&self, view: &mut ModuleStatusView) -> bool {
        if view.id != CommandQuitConfig::ID.as_str() {
            return false;
        }
        if view.state == ModuleState::Running
            && let Some(detail) = self.runtime.degraded_detail()
        {
            view.detail = Some(detail);
        }
        true
    }
}

/// Acknowledges a retained HUD during coordinated teardown.
///
/// The answer comes from a short worker thread because retrying the release
/// waits (bounded) on the main thread's acknowledgement; this call never
/// blocks. Nothing retained is an immediate success.
pub struct HudOwner {
    hud: Arc<HudHost>,
}

impl HudOwner {
    /// The stable teardown record name.
    pub const NAME: &'static str = "hold-to-quit-hud";

    /// Wraps the factory's HUD owner.
    #[must_use]
    pub const fn new(hud: Arc<HudHost>) -> Self { Self { hud } }
}

impl NativeOwner for HudOwner {
    fn name(&self) -> &'static str { Self::NAME }

    fn on_main(&self) -> bool { false }

    fn release(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        // A settle holds the lock across a bounded release, so the coordinator
        // never waits for it: a busy lock is answered by the cleanup thread.
        if self.hud.try_has_retained() == Some(false) {
            answer.succeed();
            return answer;
        }
        let completion = answer.clone();
        let hud = Arc::clone(&self.hud);
        let spawned = std::thread::Builder::new()
            .name("stache-hold-to-quit-hud-cleanup".to_owned())
            .spawn(move || match hud.settle(&CancellationToken::new()) {
                Ok(()) => completion.succeed(),
                Err(_) => completion.fail(),
            });
        if spawned.is_err() {
            answer.fail();
        }
        answer
    }
}

/// Acknowledges a retained application observation during coordinated teardown.
///
/// `ApplicationControl::stop` acknowledges the exact owned generation (a
/// no-op when nothing is owned) and waits only its bounded per-hop budget, off
/// the coordinator's thread.
pub struct ObserverOwner {
    control: ApplicationControl,
}

impl ObserverOwner {
    /// The stable teardown record name.
    pub const NAME: &'static str = "hold-to-quit-observer";

    /// Wraps the factory's application control.
    #[must_use]
    pub const fn new(control: ApplicationControl) -> Self { Self { control } }
}

impl NativeOwner for ObserverOwner {
    fn name(&self) -> &'static str { Self::NAME }

    fn on_main(&self) -> bool { false }

    fn release(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        let completion = answer.clone();
        let control = self.control.clone();
        let spawned = std::thread::Builder::new()
            .name("stache-hold-to-quit-observer-cleanup".to_owned())
            .spawn(move || match control.stop() {
                Ok(()) => completion.succeed(),
                Err(_) => completion.fail(),
            });
        if spawned.is_err() {
            answer.fail();
        }
        answer
    }
}
