//! Application event handlers for the state actor.
//!
//! These handlers process application lifecycle events:
//! - App launched → prepare for new windows from this app
//! - App activated → potentially switch workspace
//!
//! `AppHidden`/`AppShown`/`AppTerminated` are handled by the actor's exact
//! revalidation methods (see `StateActor`), not here.

use crate::modules::tiling::identity::AppIdentity;
use crate::modules::tiling::state::TilingState;

/// Handles an app launched event.
///
/// This is called when a new application starts. Windows from this app
/// will be tracked as they are created via window events.
pub fn on_app_launched(
    state: &mut TilingState,
    identity: AppIdentity,
    pid: i32,
    bundle_id: &str,
    name: &str,
) {
    tracing::debug!("Handling app launched: pid={pid}, bundle={bundle_id}, name={name}");

    // Nothing to do immediately - windows will be tracked as they're created
    // via WindowCreated events. We just log for debugging.
    let _ = (state, identity);
}

/// Handles an app activated event (brought to front).
///
/// This is informational - focus changes happen via window focus events.
pub fn on_app_activated(state: &mut TilingState, identity: AppIdentity, pid: i32) {
    tracing::debug!("Handling app activated: pid={pid}");

    // Nothing specific to do - focus will be handled by window focus events
    let _ = (state, identity);
}
// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use smallvec::smallvec;
    use uuid::Uuid;

    use crate::modules::tiling::actor::StateActor;
    use crate::modules::tiling::actor::handle::PendingActorMessages;
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits, WindowTarget};
    use crate::modules::tiling::state::{TilingState, Window, Workspace};
    use crate::modules::tiling::visibility::VisibilityRegistry;

    #[allow(clippy::cast_precision_loss)] // test-only identity construction
    fn test_identity(pid: i32, v: u64) -> AppIdentity {
        AppIdentity {
            pid,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(v as f64).unwrap(),
        }
    }

    fn make_actor() -> (StateActor, Arc<VisibilityRegistry>) {
        let registry = Arc::new(VisibilityRegistry::default());
        let actor = StateActor {
            state: TilingState::new(),
            receiver: tokio::sync::mpsc::channel(16).1,
            pending: Arc::new(parking_lot::Mutex::new(PendingActorMessages::default())),
            registry: Arc::clone(&registry),
        };
        (actor, registry)
    }

    #[test]
    fn app_shown_visible_removes_owner_and_marks_windows() {
        let identity = test_identity(42, 100);
        let (mut actor, registry) = make_actor();
        registry.insert(identity);
        actor.state.upsert_window(Window {
            id: 1,
            pid: 42,
            identity: Some(identity),
            is_hidden: true,
            workspace_id: Uuid::nil(),
            ..Window::default()
        });
        actor.on_app_shown_revalidated(identity, |_| Some(false));
        assert!(!registry.contains(&identity));
        for w in actor.state.windows.iter().filter(|w| w.identity == Some(identity)) {
            assert!(!w.is_hidden);
        }
    }

    #[test]
    fn queued_app_shown_consumed_after_external_rehide_retains_owner() {
        let identity = test_identity(42, 100);
        let (mut actor, registry) = make_actor();
        registry.insert(identity);
        actor.state.upsert_window(Window {
            id: 1,
            pid: 42,
            identity: Some(identity),
            is_hidden: true,
            workspace_id: Uuid::nil(),
            ..Window::default()
        });
        // OS reports hidden — retain owner, no window change and no hide call.
        actor.on_app_shown_revalidated(identity, |_| Some(true));
        assert!(
            registry.contains(&identity),
            "keep ownership under unavoidable-history policy"
        );
        for w in actor.state.windows.iter().filter(|w| w.identity == Some(identity)) {
            assert!(w.is_hidden);
        }
    }

    #[test]
    fn app_shown_mismatch_retains_owner() {
        let stored = test_identity(42, 100);
        let different = test_identity(42, 200);
        let (mut actor, registry) = make_actor();
        registry.insert(stored);
        actor.on_app_shown_revalidated(different, |_| Some(false));
        assert!(registry.contains(&stored));
    }

    #[test]
    fn delayed_termination_removes_only_exact_instance_resources() {
        let a = test_identity(42, 100);
        let b = test_identity(42, 200); // same PID, different launch date
        let (mut actor, registry) = make_actor();
        registry.insert(a);
        registry.insert(b);

        let wa = Uuid::now_v7();
        let wb = Uuid::now_v7();
        actor.state.upsert_workspace(Workspace {
            id: wa,
            name: "A".into(),
            window_ids: smallvec![1],
            focused_window_index: Some(0),
            ..Workspace::default()
        });
        actor.state.upsert_workspace(Workspace {
            id: wb,
            name: "B".into(),
            window_ids: smallvec![2],
            focused_window_index: Some(0),
            ..Workspace::default()
        });
        actor.state.upsert_window(Window {
            id: 1,
            pid: 42,
            identity: Some(a),
            workspace_id: wa,
            app_id: "com.test.a".into(),
            app_name: "A".into(),
            ..Window::default()
        });
        actor.state.upsert_window(Window {
            id: 2,
            pid: 42,
            identity: Some(b),
            workspace_id: wb,
            app_id: "com.test.b".into(),
            app_name: "B".into(),
            ..Window::default()
        });

        crate::modules::tiling::tabs::clear_all_tabs();
        crate::modules::tiling::tabs::register_tab(101, a);
        crate::modules::tiling::tabs::register_tab(202, b);
        actor.state.record_focus_history(wa, 1);
        actor.state.record_focus_history(wb, 2);
        let mut invalidated = Vec::new();
        let mut cached_apps = std::collections::HashSet::from([a, b]);

        actor.on_app_terminated_exact_with(
            a,
            crate::modules::tiling::tabs::clear_tabs_for_identity,
            |target| invalidated.push(target),
            |identity| {
                cached_apps.remove(&identity);
            },
        );

        assert!(!registry.contains(&a));
        assert!(registry.contains(&b));
        assert!(actor.state.get_window(1).is_none());
        assert_eq!(actor.state.get_window(2).and_then(|w| w.identity), Some(b));
        assert!(!crate::modules::tiling::tabs::is_tab_for_identity(101, a));
        assert!(crate::modules::tiling::tabs::is_tab_for_identity(202, b));
        assert!(actor.state.get_workspace(wa).is_some_and(|ws| !ws.window_ids.contains(&1)));
        assert!(actor.state.get_workspace(wb).is_some_and(|ws| ws.window_ids.contains(&2)));
        assert_eq!(actor.state.get_focus_history(wa), None);
        assert_eq!(actor.state.get_focus_history(wb), Some(2));
        assert_eq!(invalidated, vec![WindowTarget { identity: a, window_id: 1 }]);
        assert!(!cached_apps.contains(&a));
        assert!(cached_apps.contains(&b));
        crate::modules::tiling::tabs::clear_all_tabs();
    }

    #[test]
    fn exact_termination_preserves_focused_index() {
        let a = test_identity(42, 100);
        let b = test_identity(42, 200);
        let (mut actor, registry) = make_actor();
        registry.insert(a);

        let ws_id = Uuid::now_v7();
        actor.state.upsert_workspace(Workspace {
            id: ws_id,
            name: "main".into(),
            window_ids: smallvec![1, 2, 3],
            focused_window_index: Some(1),
            ..Workspace::default()
        });
        for (id, identity) in [(1, a), (2, b), (3, b)] {
            actor.state.upsert_window(Window {
                id,
                pid: 42,
                identity: Some(identity),
                workspace_id: ws_id,
                ..Window::default()
            });
        }

        actor.on_app_terminated_exact_with(a, |_| {}, |_| {}, |_| {});

        let ws = actor.state.get_workspace(ws_id).unwrap();
        assert_eq!(ws.window_ids.as_slice(), &[2, 3]);
        assert_eq!(ws.focused_window_index, Some(0));
        assert_eq!(
            ws.focused_window_index.and_then(|i| ws.window_ids.get(i)).copied(),
            Some(2),
            "focused window must stay window 2, not shift to a sibling"
        );
    }
}
