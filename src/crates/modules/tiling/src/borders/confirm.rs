//! The native active-Space confirmation (part 05, second half).
//!
//! [`OnScreenConfirm`] implements [`ActiveSpaceConfirm`] over an
//! [`OnScreenWindows`] listing: a target is on the active Space of its display
//! when its `CGWindowID` is in `CGWindowListCopyWindowInfo(
//! kCGWindowListOptionOnScreenOnly, kCGNullWindowID)`. Only `kCGWindowNumber`
//! is read, which needs no Screen Recording permission. It runs on the actor
//! thread, only on show and resume (the engine calls it nowhere else), and a
//! failed listing is [`ConfirmFailed`]. The gated private window-Space query is
//! not used (it needs its own native check first).
//!
//! The system listing is built by [`system_confirmer_factory`] when the module
//! starts; construction creates nothing and default tests inject a fake list.

use std::collections::BTreeSet;
use std::sync::Arc;

use stache_macos::border_overlay::{OnScreenWindows, SystemOnScreenWindows, confirm_on_screen};
use stache_macos::windows::WindowInstance;

use super::engine::{ActiveSpaceConfirm, ConfirmFailed};

/// Creates a fresh confirmation for each run of the module.
pub type ConfirmerFactory = Arc<dyn Fn() -> Box<dyn ActiveSpaceConfirm> + Send + Sync>;

/// Confirms windows against an on-screen window listing.
pub struct OnScreenConfirm<S: OnScreenWindows> {
    list: S,
}

impl<S: OnScreenWindows> OnScreenConfirm<S> {
    /// A confirmation over `list`.
    #[must_use]
    pub const fn new(list: S) -> Self { Self { list } }
}

impl<S: OnScreenWindows> ActiveSpaceConfirm for OnScreenConfirm<S> {
    fn confirm_on_active_space(
        &mut self,
        windows: &[(WindowInstance, u32)],
    ) -> Result<BTreeSet<WindowInstance>, ConfirmFailed> {
        confirm_on_screen(&mut self.list, windows).map_err(|_| ConfirmFailed)
    }
}

/// The factory of the system confirmation (public Core Graphics listing).
#[must_use]
pub fn system_confirmer_factory() -> ConfirmerFactory {
    Arc::new(|| {
        Box::new(OnScreenConfirm::new(SystemOnScreenWindows)) as Box<dyn ActiveSpaceConfirm>
    })
}

#[cfg(test)]
mod tests {
    use stache_macos::border_overlay::WindowListError;

    use super::*;

    struct List(Option<Vec<u32>>);

    impl OnScreenWindows for List {
        fn on_screen(&mut self, ids: &mut Vec<u32>) -> Result<(), WindowListError> {
            let list = self.0.as_ref().ok_or(WindowListError)?;
            ids.clone_from(list);
            Ok(())
        }
    }

    fn windows() -> Vec<(WindowInstance, u32)> {
        let feed = crate::test_support::Feed::new(1);
        vec![(feed.token(1, 1), 10), (feed.token(1, 2), 20)]
    }

    #[test]
    fn a_listed_target_is_confirmed_and_an_absent_one_is_not() {
        let windows = windows();
        let mut confirm = OnScreenConfirm::new(List(Some(vec![20, 99])));
        let confirmed = confirm.confirm_on_active_space(&windows).unwrap();
        assert_eq!(confirmed.into_iter().collect::<Vec<_>>(), [windows[1].0]);
    }

    #[test]
    fn a_failed_listing_is_a_failed_confirmation() {
        let mut confirm = OnScreenConfirm::new(List(None));
        assert_eq!(confirm.confirm_on_active_space(&windows()), Err(ConfirmFailed));
    }
}
