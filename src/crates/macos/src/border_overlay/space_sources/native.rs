//! The native [`SpaceSession`]: run loop, Dock Accessibility observer,
//! `NSWorkspace` observers and the private notification. Default tests never
//! construct it (they use fake sessions); it is exercised only by an approved
//! native check.
//!
//! Everything here is created on the owner thread and is not `Send`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSApplicationDidChangeScreenParametersNotification, NSRunningApplication, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification, NSWorkspaceApplicationKey,
    NSWorkspaceDidLaunchApplicationNotification, NSWorkspaceDidTerminateApplicationNotification,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNotificationName, NSOperationQueue, NSString,
};

use super::super::spaces::{SpaceDiagnostic, UnknownCause};
use super::super::wake::WakeSource;
use super::hub::{DisplayGeometry, DisplayLister, LoopWaker, MAX_DISPLAYS, SourceHub};
use super::owner::{SessionFactory, Source, SpaceSession};
use super::skylight::{SkyLightSpaceNotify, SpaceChangeRegistrar};
use crate::ax::{AxApi, AxElement, AxNotification, AxObserver, ELEMENT_TIMEOUT, SystemAx};

/// The Dock's bundle identifier.
pub const DOCK_BUNDLE_ID: &str = "com.apple.dock";

/// How long one run-loop turn may wait (a day; wakes come from sources).
const PUMP_TIMEOUT: Duration = Duration::from_hours(24);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
}

const MAX_DISPLAYS_U32: u32 = 16;
const _: () = assert!(MAX_DISPLAYS_U32 as usize == MAX_DISPLAYS);

/// The public active display list (thread-safe, no allocation).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDisplays;

impl DisplayLister for SystemDisplays {
    fn active(&self, out: &mut [u32; MAX_DISPLAYS]) -> Option<usize> {
        let mut count: u32 = 0;
        // SAFETY: `out` has room for `MAX_DISPLAYS` ids and `count` is a valid
        // out pointer.
        let code =
            unsafe { CGGetActiveDisplayList(MAX_DISPLAYS_U32, out.as_mut_ptr(), &raw mut count) };
        // A full buffer may be truncated: treat it as a failed query.
        (code == 0 && (count as usize) < MAX_DISPLAYS).then_some(count as usize)
    }

    fn geometry(&self, id: u32) -> Option<DisplayGeometry> {
        // SAFETY: plain value queries by display id (thread-safe, no
        // allocation); an unknown id yields an empty rectangle and zero sizes.
        let (bounds, pixels_wide, pixels_high) = unsafe {
            (
                CGDisplayBounds(id),
                CGDisplayPixelsWide(id),
                CGDisplayPixelsHigh(id),
            )
        };
        Some(DisplayGeometry {
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
            pixels_wide,
            pixels_high,
        })
    }
}

/// The factory the system source uses.
pub fn session_factory() -> SessionFactory {
    Box::new(|hub| Box::new(NativeSession::new(hub)) as Box<dyn SpaceSession>)
}

type Token = Retained<ProtocolObject<dyn NSObjectProtocol>>;

/// The `NSWorkspace` and display observers on a dedicated serial queue.
struct WorkspaceObservers {
    queue: Retained<NSOperationQueue>,
    registrations: Vec<(Retained<NSNotificationCenter>, Token)>,
}

impl WorkspaceObservers {
    fn new(hub: &Arc<SourceHub>) -> Self {
        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        queue.setName(Some(&NSString::from_str(
            "stache-tiling-borders-spaces-notifications",
        )));
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        let default = NSNotificationCenter::defaultCenter();
        let mut registrations = Vec::new();
        // SAFETY (the four `observe` calls): the names are immutable AppKit
        // constants; each block is `Fn`, only touches the `Send + Sync` hub and
        // is retained by the center until the token is removed in `remove`.
        let mut observe =
            |center: &Retained<NSNotificationCenter>,
             name: &NSNotificationName,
             handler: Box<dyn Fn(&NSNotification) + Send + Sync>| {
                let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        // SAFETY: the center passes a valid notification for the call.
                        handler(unsafe { notification.as_ref() });
                    }));
                });
                let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
                // SAFETY: see above; delivery is on `queue`, never on main.
                let token = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        Some(&queue),
                        block,
                    )
                };
                registrations.push((Retained::clone(center), token));
            };
        let completed = Arc::clone(hub);
        // SAFETY: immutable AppKit constants.
        observe(
            &workspace,
            unsafe { NSWorkspaceActiveSpaceDidChangeNotification },
            Box::new(move |_| completed.space_change_completed()),
        );
        let screens = Arc::clone(hub);
        // SAFETY: immutable AppKit constant. Posted on main, delivered to `queue`.
        observe(
            &default,
            unsafe { NSApplicationDidChangeScreenParametersNotification },
            Box::new(move |_| screens.display_layout_changed()),
        );
        let launched = Arc::clone(hub);
        // SAFETY: immutable AppKit constant.
        observe(
            &workspace,
            unsafe { NSWorkspaceDidLaunchApplicationNotification },
            Box::new(move |notification| {
                if is_dock(notification) {
                    launched.dock_application_changed();
                }
            }),
        );
        let terminated = Arc::clone(hub);
        // SAFETY: immutable AppKit constant.
        observe(
            &workspace,
            unsafe { NSWorkspaceDidTerminateApplicationNotification },
            Box::new(move |notification| {
                if is_dock(notification) {
                    terminated.dock_application_changed();
                }
            }),
        );
        Self { queue, registrations }
    }

    /// Removes every observer, then waits for blocks already queued (they only
    /// call the hub, which the caller closes first).
    fn remove(self) {
        for (center, token) in &self.registrations {
            // SAFETY: the token came from this center and is removed once.
            unsafe { center.removeObserver(AsRef::<AnyObject>::as_ref(token)) };
        }
        self.queue.waitUntilAllOperationsAreFinished();
    }
}

/// Whether a launch or terminate notification names the Dock application.
fn is_dock(notification: &NSNotification) -> bool {
    let Some(info) = notification.userInfo() else {
        return false;
    };
    // SAFETY: `NSWorkspaceApplicationKey` is an immutable AppKit constant.
    let key: &AnyObject = AsRef::<AnyObject>::as_ref(unsafe { NSWorkspaceApplicationKey });
    info.objectForKey(key)
        .and_then(|value| value.downcast::<NSRunningApplication>().ok())
        .and_then(|application| application.bundleIdentifier())
        .is_some_and(|id| id.to_string() == DOCK_BUNDLE_ID)
}

fn dock_pid() -> Option<i32> {
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(
        &NSString::from_str(DOCK_BUNDLE_ID),
    );
    running
        .to_vec()
        .iter()
        .map(|application| application.processIdentifier())
        .find(|pid| *pid > 0)
}

/// The Dock Accessibility observer; dropping it removes its notifications.
struct DockObserver {
    observer: AxObserver,
    _application: AxElement,
}

/// The owner-thread session.
pub struct NativeSession {
    hub: Arc<SourceHub>,
    run_loop: CFRunLoop,
    wake: Option<Arc<WakeSource>>,
    dock: Option<DockObserver>,
    workspace: Option<WorkspaceObservers>,
    skylight: SkyLightSpaceNotify,
}

impl NativeSession {
    fn new(hub: Arc<SourceHub>) -> Self {
        let run_loop = CFRunLoop::get_current();
        let wake = WakeSource::new(&run_loop).map(Arc::new);
        Self {
            hub,
            run_loop,
            wake,
            dock: None,
            workspace: None,
            skylight: SkyLightSpaceNotify::system(),
        }
    }

    fn register_dock(&mut self) -> Result<(), UnknownCause> {
        self.dock = None;
        let pid = dock_pid().ok_or(UnknownCause::DockUnregistered)?;
        let ax = SystemAx::new();
        let application = ax.create_application(pid).map_err(|_| UnknownCause::DockUnregistered)?;
        let _ = ax.set_messaging_timeout(&application, ELEMENT_TIMEOUT);
        let mut observer = AxObserver::new(pid).map_err(|_| UnknownCause::RegistrationFailed)?;
        // The source joins this (owner) thread's run loop in the default mode,
        // the mode `pump` runs.
        observer.attach_current_run_loop();
        // Every name is tried and reported, so the log shows which ones the Dock
        // accepts; any refusal fails the source closed.
        let mut failed = false;
        for notification in AxNotification::DOCK {
            let error = observer.add_notification(&application, notification).err();
            failed |= error.is_some();
            self.hub.diagnostic(SpaceDiagnostic::DockRegistration {
                name: notification.name(),
                error,
            });
        }
        if failed {
            return Err(UnknownCause::RegistrationFailed);
        }
        self.dock = Some(DockObserver {
            observer,
            _application: application,
        });
        Ok(())
    }

    /// Turns the Dock events the run loop delivered into facts. The observer's
    /// callback already recorded them without any AX read.
    fn drain_dock(&mut self) {
        let Some(dock) = self.dock.as_mut() else {
            return;
        };
        let drain = dock.observer.drain();
        for event in drain.events {
            self.hub.diagnostic(SpaceDiagnostic::DockNotification {
                name: event.notification.name(),
            });
            match event.notification {
                AxNotification::ExposeShowAllWindows
                | AxNotification::ExposeShowFrontWindows
                | AxNotification::ExposeShowDesktop => self.hub.mission_control(true),
                AxNotification::ExposeExit => self.hub.mission_control(false),
                _ => {}
            }
        }
        if drain.overflowed {
            self.hub.diagnostic(SpaceDiagnostic::DockDropped);
            // Events were lost: the observer is rebuilt (SourceLost, then
            // SourceRestored(DockUnregistered), which re-confirms). Mission
            // Control state read from the lost events is not recoverable.
            self.hub.dock_application_changed();
        }
    }
}

impl SpaceSession for NativeSession {
    fn waker(&self) -> LoopWaker {
        // Without a wake source a wake stops the run loop if it is running.
        self.wake.as_ref().map_or_else(
            || {
                let run_loop = self.run_loop.clone();
                Arc::new(move || run_loop.stop()) as LoopWaker
            },
            |wake| {
                let wake = Arc::clone(wake);
                Arc::new(move || wake.wake()) as LoopWaker
            },
        )
    }

    fn register(&mut self, source: Source) -> Result<(), UnknownCause> {
        match source {
            Source::Dock => self.register_dock(),
            Source::Workspace => {
                if self.workspace.is_none() {
                    self.workspace = Some(WorkspaceObservers::new(&self.hub));
                }
                Ok(())
            }
            Source::SkyLight => self.skylight.register(Arc::clone(&self.hub)),
        }
    }

    fn unregister(&mut self, source: Source) {
        match source {
            Source::Dock => self.dock = None,
            Source::Workspace => {
                if let Some(workspace) = self.workspace.take() {
                    workspace.remove();
                }
            }
            Source::SkyLight => self.skylight.unregister(),
        }
    }

    fn pump(&mut self) {
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _ = CFRunLoop::run_in_mode(mode, PUMP_TIMEOUT, true);
        self.drain_dock();
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        self.dock = None;
        if let Some(workspace) = self.workspace.take() {
            workspace.remove();
        }
        self.skylight.unregister();
    }
}
