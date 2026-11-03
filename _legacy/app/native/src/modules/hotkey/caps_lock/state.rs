use std::sync::Mutex;

use super::CapsKey;
use crate::config::ShortcutCommands;

/// State machine tracking a `CapsLock` pseudo-modifier press.
///
/// Caps Lock is remapped to F18 at the HID layer (see [`super::remap`]), so it
/// never toggles capitalization. This state machine only decides whether a key
/// pressed while the pseudo-modifier is held forms a configured chord.
#[derive(Debug, Default)]
pub(super) struct CapsState {
    mode: CapsMode,
    active_key: Option<CapsKey>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum CapsMode {
    #[default]
    Idle,
    CapsHeld,
    ChordUsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CapsInput {
    CapsDown,
    CapsUp,
    KeyDown(CapsKey, bool),
    KeyUp(CapsKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CapsDecision {
    Pass,
    Suppress,
    Execute(CapsKey),
}

#[derive(Debug, Default)]
pub(super) struct CapsAction {
    pub suppress: bool,
    pub commands: Option<ShortcutCommands>,
}

pub(super) static STATE: Mutex<CapsState> = Mutex::new(CapsState {
    mode: CapsMode::Idle,
    active_key: None,
});

impl CapsState {
    pub(super) fn handle_input(
        &mut self,
        input: CapsInput,
        has_binding: impl Fn(CapsKey) -> bool,
    ) -> CapsDecision {
        match input {
            CapsInput::CapsDown => {
                if self.mode == CapsMode::Idle {
                    self.mode = CapsMode::CapsHeld;
                    self.active_key = None;
                }
                CapsDecision::Pass
            }
            CapsInput::CapsUp => match self.mode {
                CapsMode::CapsHeld | CapsMode::ChordUsed => {
                    // Keep `active_key` so a straggler chord key-up after Caps is
                    // released is still suppressed.
                    self.mode = CapsMode::Idle;
                    CapsDecision::Pass
                }
                CapsMode::Idle => CapsDecision::Pass,
            },
            CapsInput::KeyDown(key, true) if self.active_key == Some(key) => CapsDecision::Suppress,
            CapsInput::KeyDown(key, is_repeat) => match self.mode {
                CapsMode::CapsHeld
                    if has_binding(key) && !is_repeat && self.active_key.is_none() =>
                {
                    self.mode = CapsMode::ChordUsed;
                    self.active_key = Some(key);
                    CapsDecision::Execute(key)
                }
                CapsMode::CapsHeld if !is_repeat && self.active_key.is_none() => {
                    self.mode = CapsMode::ChordUsed;
                    CapsDecision::Pass
                }
                CapsMode::ChordUsed
                    if has_binding(key) && !is_repeat && self.active_key.is_none() =>
                {
                    self.active_key = Some(key);
                    CapsDecision::Execute(key)
                }
                CapsMode::ChordUsed if self.active_key == Some(key) => CapsDecision::Suppress,
                _ => CapsDecision::Pass,
            },
            CapsInput::KeyUp(key) => {
                if self.active_key == Some(key) {
                    self.active_key = None;
                    CapsDecision::Suppress
                } else {
                    CapsDecision::Pass
                }
            }
        }
    }
}

pub(super) fn action_for_input(
    state: &mut CapsState,
    input: CapsInput,
    bindings: &std::collections::HashMap<CapsKey, super::CapsBinding>,
) -> CapsAction {
    let decision = state.handle_input(input, |key| bindings.contains_key(&key));

    match decision {
        CapsDecision::Pass => CapsAction::default(),
        CapsDecision::Suppress => CapsAction {
            suppress: true,
            ..CapsAction::default()
        },
        CapsDecision::Execute(key) => CapsAction {
            suppress: true,
            commands: bindings.get(&key).map(|binding| binding.commands.clone()),
        },
    }
}
