//! The runtime-owned native tab group registry.
//!
//! A native tab group is a set of windows of one application that macOS shows
//! as one. The registry is **pure** and owned by the actor's state: it is not a
//! process-global and never caches a `CGWindowID` on its own. Groups are keyed
//! by the exact *reporter* window token that exposed the tab strip, are scoped
//! to that token's exact application, and are replaced atomically from the
//! reporter's next snapshot. A member is only ever matched to a tracked window
//! of the same application at query time, so a numeric `CGWindowID` reused by
//! another group, another application or a later window can never match a stale
//! entry.
//!
//! # Policy
//!
//! - Exactly one member (the selected one) of a group is a tile. The other
//!   members are *inactive tabs*: tracked, never tiles.
//! - Exclusion needs reliable evidence. A group that has no selected member,
//!   more than one, a selected member that is not a tracked window of the same
//!   application, or an unidentifiable unselected member excludes nothing: an
//!   ordinary window is never arbitrarily excluded.
//! - A failed or missing tab read drops the reporter's group (its members
//!   become ordinary windows) and marks tab information unavailable.
//!
//! Native evidence for the tab reads is still missing (see the feature doc).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use stache_macos::ApplicationId;
use stache_macos::windows::{TabInfo, TabMember, WindowInstance, WindowSnapshot};

use crate::state::WindowState;

/// One native tab group as its reporter last described it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabGroup {
    /// The exact window whose tab strip this describes.
    pub reporter: WindowInstance,
    /// The members in strip order.
    pub members: Vec<TabMember>,
}

/// A group resolved against the tracked windows of its application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedGroup {
    /// The reporter.
    pub reporter: WindowInstance,
    /// The tracked window that is the one visible tile.
    pub active: WindowInstance,
    /// The tracked windows that are inactive tabs.
    pub inactive: Vec<WindowInstance>,
}

/// Counters for the registry; monotonic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabStats {
    /// Applications whose initial tab scan completed (at most once each).
    pub initial_scans: u64,
    /// Groups replaced or created from a snapshot.
    pub replacements: u64,
    /// Groups removed (no tab strip, failed read, destroyed reporter, ...).
    pub removals: u64,
}

/// A read-only description of the tab capability for status and queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabsStatus {
    /// Whether the backend reads tab groups at all.
    pub reading: bool,
    /// Tracked groups.
    pub groups: usize,
    /// Tracked windows currently held back as inactive tabs.
    pub inactive_windows: usize,
    /// Applications with a window whose tab read failed.
    pub unavailable_apps: usize,
    /// Counters.
    pub stats: TabStats,
}

/// The tab group registry. Bounded: at most one group per tracked window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabRegistry {
    reading: bool,
    groups: BTreeMap<WindowInstance, TabGroup>,
    scanned: BTreeSet<ApplicationId>,
    failed: BTreeSet<WindowInstance>,
    stats: TabStats,
}

impl TabRegistry {
    /// Whether the backend reads tab groups.
    #[must_use]
    pub const fn reading(&self) -> bool { self.reading }

    /// Declares whether the backend reads tab groups. Without reads every
    /// window is an ordinary window.
    pub const fn set_reading(&mut self, reading: bool) { self.reading = reading; }

    /// Applies a snapshot's tab information for its exact window. Returns
    /// whether the registry changed.
    pub fn observe(&mut self, snapshot: &WindowSnapshot) -> bool {
        let token = snapshot.window;
        match snapshot.tabs {
            // Not read: nothing is learned, nothing is invented.
            TabInfo::Unknown => false,
            TabInfo::None => self.remove(token) | self.failed.remove(&token),
            TabInfo::Failed => {
                let newly = self.failed.insert(token);
                self.remove(token) | newly
            }
            TabInfo::Tabs(_) if snapshot.tab_members.is_empty() => {
                self.remove(token) | self.failed.remove(&token)
            }
            TabInfo::Tabs(_) => {
                self.failed.remove(&token);
                self.replace(token, snapshot.tab_members.clone())
            }
        }
    }

    /// Replaces the reporter's group atomically. Any other group of the same
    /// application that names one of the same members is the same group seen by
    /// a stale reporter and is dropped, so the newest reporter wins.
    fn replace(&mut self, reporter: WindowInstance, members: Vec<TabMember>) -> bool {
        let application = reporter.application();
        let ids: BTreeSet<u32> = members.iter().filter_map(|m| m.cg_window_id).collect();
        let stale: Vec<WindowInstance> = self
            .groups
            .iter()
            .filter(|(token, group)| {
                **token != reporter
                    && token.application() == application
                    && group
                        .members
                        .iter()
                        .any(|m| m.cg_window_id.is_some_and(|id| ids.contains(&id)))
            })
            .map(|(token, _)| *token)
            .collect();
        let mut changed = false;
        for token in stale {
            changed |= self.remove(token);
        }
        let group = TabGroup { reporter, members };
        if self.groups.get(&reporter) == Some(&group) {
            return changed;
        }
        self.groups.insert(reporter, group);
        self.stats.replacements += 1;
        true
    }

    fn remove(&mut self, reporter: WindowInstance) -> bool {
        let removed = self.groups.remove(&reporter).is_some();
        if removed {
            self.stats.removals += 1;
        }
        removed
    }

    /// Forgets everything about a window that is gone. Returns whether a group
    /// was dropped.
    ///
    /// `cg_window_id` is the departed window's numeric id: it is dropped from
    /// every group of the same application so a later window that reuses the
    /// number cannot match the stale member.
    pub fn forget_window(&mut self, token: WindowInstance, cg_window_id: Option<u32>) -> bool {
        self.failed.remove(&token);
        let mut changed = self.remove(token);
        if let Some(id) = cg_window_id {
            let application = token.application();
            for group in
                self.groups.values_mut().filter(|g| g.reporter.application() == application)
            {
                let before = group.members.len();
                group.members.retain(|member| member.cg_window_id != Some(id));
                changed |= group.members.len() != before;
            }
        }
        changed
    }

    /// Forgets everything about an application that terminated.
    pub fn forget_app(&mut self, application: ApplicationId) {
        let reporters: Vec<WindowInstance> = self
            .groups
            .keys()
            .filter(|token| token.application() == application)
            .copied()
            .collect();
        for token in reporters {
            self.remove(token);
        }
        self.failed.retain(|token| token.application() != application);
        self.scanned.remove(&application);
    }

    /// Forgets everything bound to a previous backend registry. Whether tabs
    /// are read is a backend property and stays.
    pub fn reset(&mut self) {
        self.groups.clear();
        self.scanned.clear();
        self.failed.clear();
    }

    /// Marks an application's enumeration as complete. Returns `true` only the
    /// first time: the initial tab scan of an application runs once; later
    /// changes reconcile through single-window snapshots.
    pub fn scan_complete(&mut self, application: ApplicationId) -> bool {
        if !self.reading || !self.scanned.insert(application) {
            return false;
        }
        self.stats.initial_scans += 1;
        true
    }

    /// Resolves every group of `application` against the tracked windows.
    /// Members match only a window of exactly this application whose
    /// `CGWindowID` equals the member's; unreliable groups resolve to nothing.
    #[must_use]
    pub fn resolve<'a>(
        &self,
        application: ApplicationId,
        windows: impl IntoIterator<Item = &'a WindowState>,
    ) -> Vec<ResolvedGroup> {
        if !self.reading {
            return Vec::new();
        }
        let mut by_id: HashMap<u32, Option<WindowInstance>> = HashMap::new();
        for window in windows {
            if window.application != application {
                continue;
            }
            if let Some(id) = window.cg_window_id {
                // A duplicated id inside one application is ambiguous.
                if by_id.insert(id, Some(window.token)).is_some() {
                    by_id.insert(id, None);
                }
            }
        }
        let mut out = Vec::new();
        for group in self.groups.values().filter(|g| g.reporter.application() == application) {
            if let Some(resolved) = resolve_group(group, &by_id) {
                out.push(resolved);
            }
        }
        out
    }

    /// The tracked windows of `application` held back as inactive tabs.
    #[must_use]
    pub fn inactive<'a>(
        &self,
        application: ApplicationId,
        windows: impl IntoIterator<Item = &'a WindowState>,
    ) -> BTreeSet<WindowInstance> {
        self.resolve(application, windows)
            .into_iter()
            .flat_map(|g| g.inactive)
            .collect()
    }

    /// The status view. `inactive_windows` is supplied by the caller (it is a
    /// property of the windows).
    #[must_use]
    pub fn status(&self, inactive_windows: usize) -> TabsStatus {
        TabsStatus {
            reading: self.reading,
            groups: self.groups.len(),
            inactive_windows,
            unavailable_apps: self
                .failed
                .iter()
                .map(WindowInstance::application)
                .collect::<BTreeSet<_>>()
                .len(),
            stats: self.stats,
        }
    }
}

fn resolve_group(
    group: &TabGroup,
    by_id: &HashMap<u32, Option<WindowInstance>>,
) -> Option<ResolvedGroup> {
    if group.members.len() < 2 {
        return None;
    }
    let mut selected = group.members.iter().filter(|m| m.selected == Some(true));
    let active_member = selected.next()?;
    if selected.next().is_some() {
        return None;
    }
    let active = lookup(by_id, active_member.cg_window_id?)?;
    let mut inactive = Vec::new();
    for member in group.members.iter().filter(|m| m.selected != Some(true)) {
        // An unselected member must be identifiable and tracked to be held
        // back; an unknown flag is not evidence of being inactive.
        if member.selected != Some(false) {
            continue;
        }
        let Some(token) = member.cg_window_id.and_then(|id| lookup(by_id, id)) else {
            continue;
        };
        if token != active {
            inactive.push(token);
        }
    }
    Some(ResolvedGroup {
        reporter: group.reporter,
        active,
        inactive,
    })
}

fn lookup(by_id: &HashMap<u32, Option<WindowInstance>>, id: u32) -> Option<WindowInstance> {
    by_id.get(&id).copied().flatten()
}

#[cfg(test)]
mod tests {
    use stache_macos::windows::{TabInfo, TabMember};

    use super::*;
    use crate::state::TilingState;
    use crate::test_support::{Feed, app, info, ready, snapshot};

    fn window(
        state: &TilingState,
        template: WindowInstance,
        token: WindowInstance,
        cg: u32,
    ) -> WindowState {
        let mut window = state.windows[&template].clone();
        window.token = token;
        window.application = token.application();
        window.cg_window_id = Some(cg);
        window
    }

    fn strip(token: WindowInstance, members: &[(u32, bool)]) -> WindowSnapshot {
        let mut snap = snapshot(token, "t", 1);
        snap.tabs = TabInfo::Tabs(members.len());
        snap.tab_members = members
            .iter()
            .map(|(id, sel)| TabMember {
                cg_window_id: Some(*id),
                selected: Some(*sel),
            })
            .collect();
        snap
    }

    #[test]
    fn members_only_match_windows_of_the_exact_application() {
        let mut feed = Feed::new(1);
        let mut state = ready("", &[info(1, "com.a", "A"), info(2, "com.b", "B")], &mut feed);
        let (a1, a2, other) = (feed.token(1, 1), feed.token(1, 2), feed.token(2, 1));
        let created = [(a1, 1), (a2, 5)];
        for (token, cg) in created {
            let input = feed.created(token, "w", cg);
            state = crate::test_support::step(state, input).0;
        }
        let template = state.windows[&a1].clone();
        // Application 2 has a window with the same numeric CG id as the tab.
        let foreign = window(&state, a1, other, 2);
        let tab = WindowState {
            cg_window_id: Some(2),
            ..window(&state, a1, a2, 2)
        };
        let active = WindowState {
            cg_window_id: Some(1),
            ..template
        };
        let mut registry = TabRegistry::default();
        registry.set_reading(true);
        assert!(registry.observe(&strip(a1, &[(1, true), (2, false)])));
        let windows = [active, tab, foreign];
        assert_eq!(registry.inactive(app(1), windows.iter()), BTreeSet::from([a2]));
        assert!(
            registry.inactive(app(2), windows.iter()).is_empty(),
            "a group is scoped to its reporter's application"
        );
        // Without reads nothing resolves.
        registry.set_reading(false);
        assert!(registry.inactive(app(1), windows.iter()).is_empty());
    }

    #[test]
    fn replacement_is_atomic_and_idempotent() {
        let feed = Feed::new(1);
        let (a, b) = (feed.token(1, 1), feed.token(1, 2));
        let mut registry = TabRegistry::default();
        registry.set_reading(true);
        assert!(registry.observe(&strip(a, &[(1, true), (2, false)])));
        assert!(
            !registry.observe(&strip(a, &[(1, true), (2, false)])),
            "same strip, no change"
        );
        assert!(registry.observe(&strip(b, &[(2, true), (1, false)])));
        assert_eq!(
            registry.status(0).groups,
            1,
            "the stale reporter's group was replaced"
        );
        assert!(registry.forget_window(b, Some(2)));
        assert_eq!(registry.status(0).groups, 0);
        registry.reset();
        assert_eq!(registry.status(0).stats.removals, 2);
    }

    #[test]
    fn forgetting_an_application_drops_its_groups_and_scan_marker() {
        let feed = Feed::new(1);
        let a = feed.token(1, 1);
        let mut registry = TabRegistry::default();
        registry.set_reading(true);
        registry.observe(&strip(a, &[(1, true), (2, false)]));
        assert!(registry.scan_complete(app(1)));
        assert!(!registry.scan_complete(app(1)), "the initial scan runs once");
        registry.forget_app(app(1));
        assert_eq!(registry.status(0).groups, 0);
        assert!(registry.scan_complete(app(1)), "a restarted app scans again");
    }
}
