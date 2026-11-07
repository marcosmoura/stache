//! The Carbon callback's decision function (runs on the main thread).
//!
//! It reads the already-extracted event, looks the ID up in the main-local route
//! table and either forwards, swallows or reports the press to the sink. It does
//! no native query, wait, process spawn or blocking lock of its own: the only
//! shared state it reads is two atomics and the central registry's non-blocking
//! `try_dispatchable` check, which fails closed (no dispatch) under contention.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use stache_sdk::input::ReservationId;

use super::control::Shared;
use super::native::{Dispatch, Disposition, RawEvent, RawEventKind, SIGNATURE};
use super::types::BindingToken;

/// One native registration known to the callback.
#[derive(Clone, Copy, Debug)]
pub(super) struct Route {
    pub token: BindingToken,
    /// The central reservation; `None` while the registration is still staged.
    pub reservation: Option<ReservationId>,
    /// Logical state. A removed, staged or guarded route is not live: it is
    /// still registered natively but never dispatches.
    pub live: bool,
    /// The generation the route was created in.
    pub generation: u64,
}

pub(super) type Routes = HashMap<u32, Route>;

/// Builds the handler's decision closure over the main-local route table.
pub(super) fn dispatcher(routes: Rc<RefCell<Routes>>, shared: Arc<Shared>) -> Dispatch {
    Rc::new(move |event| {
        // A panic anywhere below must not unwind into Carbon. Forwarding is the
        // safe answer: an undecidable event is never dispatched.
        catch_unwind(AssertUnwindSafe(|| decide(event, &routes, &shared)))
            .unwrap_or(Disposition::Forward)
    })
}

fn decide(event: RawEvent, routes: &RefCell<Routes>, shared: &Shared) -> Disposition {
    // Malformed (the parameter could not be read) or foreign signature.
    let Ok((signature, id)) = event.hot_key else {
        return Disposition::Forward;
    };
    if signature != SIGNATURE {
        return Disposition::Forward;
    }
    // Unknown or stale ID: an ID is never reused, so it can only be a retired
    // registration (or garbage), never a successor.
    let route = {
        let Ok(routes) = routes.try_borrow() else {
            return Disposition::Forward;
        };
        let Some(route) = routes.get(&id) else {
            return Disposition::Forward;
        };
        *route
    };
    match event.kind {
        RawEventKind::Other => Disposition::Forward,
        // Releases never dispatch; an event for our own registration stops here.
        RawEventKind::Released => Disposition::Handled,
        RawEventKind::Pressed => {
            if route.live
                && route.generation > shared.inhibited.load(Ordering::Acquire)
                // Fail closed when the registry is busy: a dropped press is
                // safer than running a chord that may have just been yielded.
                && route.reservation.is_some_and(|reservation| {
                    shared.service.reservations().try_dispatchable(reservation) == Some(true)
                })
            {
                // A panicking sink must not turn an accepted press into a forward.
                let _ = catch_unwind(AssertUnwindSafe(|| shared.sink.pressed(route.token)));
            }
            Disposition::Handled
        }
    }
}
