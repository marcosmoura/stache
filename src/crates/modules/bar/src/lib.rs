//! Bar configuration.
//!
//! [`BarConfig`] is the single authoritative source for `bar.toml`, its
//! schema, template and legacy migration. This crate is inert: it ships no
//! runtime, window, provider or command. `enabled: true` validates but nothing
//! runs until the Bar runtime parts land.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;

pub use config::{BarConfig, WeatherConfig, WeatherProvider};
