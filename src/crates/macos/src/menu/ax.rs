//! The AX element abstraction now lives in the neutral [`crate::ax`] module.
//!
//! This shim keeps every `menu::ax` path and `menu` re-export stable so the
//! `MenuAnywhere` callers and tests compile unchanged.

pub use crate::ax::{AxApi, AxElement, AxError, StringAttribute, SystemAx};
