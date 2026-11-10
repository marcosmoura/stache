//! Projection of native display identity into the SDK's display identity.

/// Projects a native display ID onto the SDK's [`stache_sdk::DisplayId`].
///
/// Both wrap the same `CGDirectDisplayID`; the raw value is preserved. This is
/// a plain function because neither type is local to this crate, so the orphan
/// rule forbids a `From` impl.
#[must_use]
pub const fn sdk_display_id(native: stache_macos::desktop::DisplayId) -> stache_sdk::DisplayId {
    stache_sdk::DisplayId::new(native.raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_projection_preserves_the_raw_display_id() {
        for raw in [0, 1, 69_733_378, u32::MAX] {
            let native = stache_macos::desktop::DisplayId::new(raw);
            assert_eq!(sdk_display_id(native).raw(), raw);
        }
    }
}
