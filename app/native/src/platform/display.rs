//! Display utilities for querying macOS display state.
//!
//! Provides functions to detect screen mirroring (`AirPlay`) via CoreGraphics.

use core_graphics::display::CGDisplay;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut u32,
        display_count: *mut u32,
    ) -> i32;
}

/// Returns all active display IDs.
fn get_active_display_ids() -> Vec<u32> {
    let mut display_count: u32 = 0;

    let result = unsafe { CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut display_count) };

    if result != 0 || display_count == 0 {
        return vec![CGDisplay::main().id];
    }

    let mut displays = vec![0u32; display_count as usize];

    let result = unsafe {
        CGGetActiveDisplayList(display_count, displays.as_mut_ptr(), &raw mut display_count)
    };

    if result != 0 {
        return vec![CGDisplay::main().id];
    }

    displays.truncate(display_count as usize);
    displays
}

/// Returns `true` if screen mirroring (`AirPlay`) is currently active.
///
/// Detects software mirroring by checking if any display is in a mirror set
/// but NOT in a hardware mirror set. `AirPlay` screen mirroring uses software
/// mirroring, so `is_in_mirror_set && !is_in_hw_mirror_set` identifies it.
#[must_use]
pub fn is_screen_mirroring_active() -> bool {
    get_active_display_ids().iter().any(|&id| {
        let display = CGDisplay::new(id);
        display.is_in_mirror_set() && !display.is_in_hw_mirror_set()
    })
}
