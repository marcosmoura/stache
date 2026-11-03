//! Display state used by the audio pilot.
//!
//! `AirPlay` screen mirroring keeps the audio on the mirrored destination, so
//! the audio policy needs to know when software mirroring is active. Detection
//! is read-only and has no main-thread requirement.

use core_graphics::display::CGDisplay;

/// Whether any active display is in a software mirror set but not a hardware
/// one.
///
/// `AirPlay` screen mirroring uses software mirroring, so
/// `is_in_mirror_set && !is_in_hw_mirror_set` identifies it. When the system
/// reports no active displays, the main display is used, matching the legacy
/// fallback.
#[must_use]
pub fn is_screen_mirroring_active() -> bool {
    let mut displays = CGDisplay::active_displays().unwrap_or_default();
    if displays.is_empty() {
        displays.push(CGDisplay::main().id);
    }
    displays.into_iter().any(|id| {
        let display = CGDisplay::new(id);
        display.is_in_mirror_set() && !display.is_in_hw_mirror_set()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirroring_probe_reads_the_display_state() {
        // The value depends on the machine; this only exercises the
        // CoreGraphics link and proves the read is safe and non-mutating.
        let _ = is_screen_mirroring_active();
    }
}
