//! Pixel or percentage dimensions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

/// A dimension that is either pixels (`1440`) or a percentage (`"50%"`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum DimensionValue {
    /// Value in pixels.
    Pixels(u32),
    /// Value as a percentage string such as `"50%"`.
    Percentage(String),
}

impl Default for DimensionValue {
    fn default() -> Self { Self::Pixels(0) }
}

impl DimensionValue {
    /// The percentage as a finite non-negative number, or `None` for pixels.
    ///
    /// Mirrors the accepted legacy spelling: surrounding whitespace and any
    /// trailing `%` characters are ignored.
    fn percentage(value: &str) -> Result<f64, String> {
        let trimmed = value.trim().trim_end_matches('%');
        let number: f64 = trimmed
            .parse()
            .map_err(|_| format!("`{value}` is not a pixel or percentage value"))?;
        if !number.is_finite() || number < 0.0 {
            return Err(format!("`{value}` must be a finite, non-negative percentage"));
        }
        Ok(number)
    }

    /// Checks the value; `positive` additionally rejects zero (sizes).
    pub(super) fn validate(&self, field: &str, positive: bool) -> Result<(), String> {
        let (zero, text) = match self {
            Self::Pixels(pixels) => (*pixels == 0, pixels.to_string()),
            Self::Percentage(value) => (
                Self::percentage(value).map_err(|error| format!("{field}: {error}"))? == 0.0,
                value.clone(),
            ),
        };
        if positive && zero {
            return Err(format!("{field}: `{text}` must be greater than zero"));
        }
        Ok(())
    }
}
