//! Pure resolution of requests into physical chords.
//!
//! Nothing here touches a native API: it maps keys, resolves `§` against a
//! layout, and settles physical-chord collisions deterministically.

use std::collections::{HashMap, HashSet};

use stache_sdk::input::{InputPriority, ModifierFlags};

use super::keymap::virtual_key;
use super::types::{BindingOutcome, HotkeyRequest, HotkeyTarget, ResolvedChord};
use crate::keyboard_layout::{
    LayoutRead, LayoutResolution, SectionSource, carbon_modifiers, resolver,
};

/// A binding that has a physical chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub key: u64,
    pub priority: InputPriority,
    pub key_code: u16,
    pub modifiers: ModifierFlags,
    pub carbon: u32,
    pub section: Option<SectionSource>,
}

impl Candidate {
    pub(super) const fn physical(&self) -> (u16, u32) { (self.key_code, self.carbon) }

    pub(super) fn resolved(&self) -> ResolvedChord {
        ResolvedChord {
            key_code: self.key_code,
            modifiers: self.modifiers,
            section: self.section.clone(),
        }
    }
}

/// A request's resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Resolved {
    Ready(Candidate),
    Rejected(BindingOutcome),
}

/// Resolves every request, in request order.
///
/// Collisions: when several bindings resolve to one physical chord the ordinary
/// (non-`§`) binding wins over a `§` one, otherwise the lowest key wins; every
/// loser is `Collision { winner }`. The result never depends on hash order.
pub(super) fn resolve(requests: &[HotkeyRequest], layout: &LayoutRead) -> Vec<(u64, Resolved)> {
    let mut section: Option<LayoutResolution> = None;
    let mut seen: HashSet<u64> = HashSet::new();
    let mut resolved: Vec<(u64, Resolved)> = requests
        .iter()
        .map(|request| {
            let outcome = if seen.insert(request.key) {
                resolve_one(request, layout, &mut section)
            } else {
                Resolved::Rejected(BindingOutcome::Failed {
                    status: 0,
                    detail: "duplicate binding key in one request".to_owned(),
                })
            };
            (request.key, outcome)
        })
        .collect();

    // Group by physical chord; order within a group is (is_section, key).
    let mut groups: HashMap<(u16, u32), Vec<usize>> = HashMap::new();
    for (index, (_, entry)) in resolved.iter().enumerate() {
        if let Resolved::Ready(candidate) = entry {
            groups.entry(candidate.physical()).or_default().push(index);
        }
    }
    for members in groups.values_mut().filter(|members| members.len() > 1) {
        members.sort_by_key(|&index| match &resolved[index].1 {
            Resolved::Ready(candidate) => (candidate.section.is_some(), candidate.key),
            Resolved::Rejected(_) => (true, u64::MAX),
        });
        let winner = resolved[members[0]].0;
        for &loser in &members[1..] {
            resolved[loser].1 = Resolved::Rejected(BindingOutcome::Collision { winner });
        }
    }
    resolved
}

fn resolve_one(
    request: &HotkeyRequest,
    layout: &LayoutRead,
    section: &mut Option<LayoutResolution>,
) -> Resolved {
    let (key_code, modifiers, source) = match request.target {
        HotkeyTarget::Physical { code, modifiers } => {
            let Some(key_code) = virtual_key(code) else {
                return Resolved::Rejected(BindingOutcome::Unsupported(format!(
                    "{code:?} has no Carbon virtual key"
                )));
            };
            (key_code, modifiers, None)
        }
        HotkeyTarget::Section { modifiers } => {
            let resolution = section.get_or_insert_with(|| resolver::resolve(layout));
            let (key_code, merged) = resolution.chord(modifiers);
            (key_code, merged, Some(resolution.source.clone()))
        }
    };
    let modifiers = modifiers.without(ModifierFlags::CAPS_LOCK);
    let Some(carbon) = carbon_modifiers(modifiers) else {
        return Resolved::Rejected(BindingOutcome::Unsupported(
            "the Function modifier cannot be registered as a Carbon hotkey".to_owned(),
        ));
    };
    Resolved::Ready(Candidate {
        key: request.key,
        priority: request.priority,
        key_code,
        modifiers,
        carbon,
        section: source,
    })
}
