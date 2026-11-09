//! The bounded event pipeline between producers and the actor.
//!
//! Producers (the backend sink, lifecycle callbacks, display hints, config)
//! admit pure values; the single actor drains them. Every queue has an explicit
//! capacity and admission accounting; overflow never silently loses a
//! destroy or focus: it marks the affected scope unconfirmed so one bounded
//! reconciliation repairs it.

mod geometry;
mod ingress;
mod pipeline;

pub use geometry::{
    DEFAULT_REFRESH_HZ, GEOMETRY_CAPACITY, GeometryCoalescer, MAX_REFRESH_HZ, MIN_REFRESH_HZ,
    RETIME_EPSILON, Record, clamp_refresh_hz, flush_period,
};
pub use ingress::{Ingress, IntentSubmitError, SubmitError, Wake};
pub use pipeline::{
    AdmissionStats, COMPLETION_CAPACITY, CONTROL_CAPACITY, Pipeline, SEMANTIC_CAPACITY,
};

#[cfg(test)]
mod tests;
