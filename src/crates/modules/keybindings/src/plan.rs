//! The deterministic binding plan.
//!
//! [`Plan::new`] turns raw bindings into a value naming the route of each
//! binding. It performs no registration and touches no native API. Raw keys are
//! planned in lexical order, so the result never depends on map insertion order.

use std::collections::HashMap;

use stache_sdk::StartupCommands;

use crate::KeybindingsConfig;
use crate::shortcut::{self, Route, ShortcutError};

/// One accepted binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBinding {
    /// The spelling as configured.
    pub raw: String,
    /// The spelling after alias normalization.
    pub normalized: String,
    /// How the binding is delivered.
    pub route: Route,
    /// Trimmed, non-empty commands in order. They stay opaque here: the shared
    /// runner interprets quoting. An empty list means "capture without spawn".
    pub commands: Vec<String>,
}

/// A per-binding planning fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDiagnostic {
    /// The spelling was rejected and skipped.
    Invalid {
        /// The spelling as configured.
        raw: String,
        /// Why.
        error: ShortcutError,
    },
    /// Two spellings mean the same route; the later one in lexical order wins.
    Duplicate {
        /// The spelling that was replaced.
        replaced: String,
        /// The spelling that is planned.
        winner: String,
        /// The shared route.
        route: Route,
    },
}

/// The planned bindings and every skipped or replaced entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Surviving bindings in lexical order of their raw spelling.
    pub bindings: Vec<PlannedBinding>,
    /// Diagnostics in planning order.
    pub diagnostics: Vec<PlanDiagnostic>,
}

impl Plan {
    /// Plans every binding in `config`, ignoring `config.enabled`.
    #[must_use]
    pub fn from_config(config: &KeybindingsConfig) -> Self {
        Self::new(config.bindings.iter().map(|(raw, commands)| (raw.as_str(), commands)))
    }

    /// Plans raw bindings. Input order is irrelevant: entries are sorted by raw
    /// spelling first.
    #[must_use]
    pub fn new<'a>(bindings: impl IntoIterator<Item = (&'a str, &'a StartupCommands)>) -> Self {
        let mut sorted: Vec<_> = bindings.into_iter().collect();
        sorted.sort_by_key(|(left, _)| *left);

        let mut slots: Vec<Option<PlannedBinding>> = Vec::new();
        let mut by_route: HashMap<Route, usize> = HashMap::new();
        let mut diagnostics = Vec::new();

        for (raw, commands) in sorted {
            let route = match shortcut::parse(raw) {
                Ok(route) => route,
                Err(error) => {
                    diagnostics.push(PlanDiagnostic::Invalid { raw: raw.to_owned(), error });
                    continue;
                }
            };
            let binding = PlannedBinding {
                raw: raw.to_owned(),
                normalized: shortcut::normalize(raw),
                route,
                commands: commands.commands().into_iter().map(str::to_owned).collect(),
            };
            if let Some(&previous) = by_route.get(&route)
                && let Some(replaced) = slots[previous].take()
            {
                diagnostics.push(PlanDiagnostic::Duplicate {
                    replaced: replaced.raw,
                    winner: binding.raw.clone(),
                    route,
                });
            }
            by_route.insert(route, slots.len());
            slots.push(Some(binding));
        }

        Self {
            bindings: slots.into_iter().flatten().collect(),
            diagnostics,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use stache_sdk::input::ModifierFlags;

    use super::*;
    use crate::shortcut::{CapsKey, Code, PhysicalChord};

    fn one(command: &str) -> StartupCommands { StartupCommands::Single(command.to_owned()) }

    fn plan(entries: &[(&str, StartupCommands)]) -> Plan {
        Plan::new(entries.iter().map(|(raw, commands)| (*raw, commands)))
    }

    fn routes(plan: &Plan) -> Vec<(&str, Route)> {
        plan.bindings.iter().map(|b| (b.raw.as_str(), b.route)).collect()
    }

    fn ordinary(key: Code, modifiers: ModifierFlags) -> Route {
        Route::Ordinary {
            chord: PhysicalChord { key, modifiers },
        }
    }

    #[test]
    fn plan_is_independent_of_insertion_order() {
        let entries = [
            ("Cmd+Ctrl+R", one("a")),
            ("Command+Control+R", one("b")),
            ("CapsLock+S", one("c")),
            ("Command+§", one("d")),
            ("Bogus+K", one("e")),
            ("F5", StartupCommands::default()),
        ];
        let expected = plan(&entries);
        let mut permuted = entries.to_vec();
        for rotation in 0..entries.len() {
            permuted.rotate_left(1);
            assert_eq!(plan(&permuted), expected, "rotation {rotation}");
        }
        permuted.reverse();
        assert_eq!(plan(&permuted), expected);
        let map: BTreeMap<_, _> = entries.iter().cloned().map(|(k, v)| (k.to_owned(), v)).collect();
        let config = KeybindingsConfig { enabled: true, bindings: map };
        assert_eq!(Plan::from_config(&config), expected);
    }

    #[test]
    fn last_duplicate_in_lexical_order_wins_and_names_both() {
        let p = plan(&[
            ("Command+Control+R", one("second")),
            ("Cmd+Ctrl+R", one("first")),
            ("Super+Ctrl+R", one("third")),
        ]);
        // Lexical order: "Cmd+Ctrl+R" < "Command+Control+R" < "Super+Ctrl+R".
        assert_eq!(p.bindings.len(), 1);
        assert_eq!(p.bindings[0].raw, "Super+Ctrl+R");
        assert_eq!(p.bindings[0].commands, ["third"]);
        let route = p.bindings[0].route;
        assert_eq!(p.diagnostics, [
            PlanDiagnostic::Duplicate {
                replaced: "Cmd+Ctrl+R".into(),
                winner: "Command+Control+R".into(),
                route
            },
            PlanDiagnostic::Duplicate {
                replaced: "Command+Control+R".into(),
                winner: "Super+Ctrl+R".into(),
                route
            },
        ]);
    }

    #[test]
    fn routes_caps_first_then_section_then_ordinary() {
        let p = plan(&[
            ("CapsLock+§", one("")),
            ("CapsLock+S", one("")),
            ("Opt+Cmd+§", one("")),
            ("Ctrl+`", one("")),
            ("Command+CapsLock", one("")),
        ]);
        let found = routes(&p);
        assert!(found.contains(&("CapsLock+§", Route::Caps { key: CapsKey(0x0A) })));
        assert!(found.contains(&("CapsLock+S", Route::Caps { key: CapsKey(1) })));
        assert!(found.contains(&("Opt+Cmd+§", Route::Section {
            modifiers: ModifierFlags::OPTION | ModifierFlags::COMMAND
        })));
        assert!(found.contains(&("Ctrl+`", ordinary(Code::Backquote, ModifierFlags::CONTROL))));
        // Not a leading CapsLock: the ordinary grammar owns it, as in the archive.
        assert!(found.contains(&(
            "Command+CapsLock",
            ordinary(Code::CapsLock, ModifierFlags::COMMAND)
        )));
    }

    #[test]
    fn section_placement_and_modifiers() {
        let p = plan(&[
            ("§+Command", one("")),
            ("Command+§+Shift", one("")),
            ("Fn+§", one("")),
            ("§", one("")),
            ("Shift+Control+Option+Command+§", one("")),
            ("CapsLock", one("")),
            ("CapsLock+A+B", one("")),
            ("CapsLock+Nope", one("")),
        ]);
        let invalid: BTreeMap<&str, &ShortcutError> = p
            .diagnostics
            .iter()
            .filter_map(|d| match d {
                PlanDiagnostic::Invalid { raw, error } => Some((raw.as_str(), error)),
                PlanDiagnostic::Duplicate { .. } => None,
            })
            .collect();
        assert_eq!(invalid["§+Command"], &ShortcutError::SectionNotLast);
        assert_eq!(invalid["Command+§+Shift"], &ShortcutError::SectionNotLast);
        assert_eq!(
            invalid["Fn+§"],
            &ShortcutError::SectionUnknownModifier("Fn".into())
        );
        assert_eq!(invalid["CapsLock"], &ShortcutError::CapsMissingKey);
        assert_eq!(invalid["CapsLock+A+B"], &ShortcutError::CapsUnsupportedShape);
        assert_eq!(
            invalid["CapsLock+Nope"],
            &ShortcutError::CapsUnknownKey("Nope".into())
        );
        let found = routes(&p);
        assert!(found.contains(&("§", Route::Section { modifiers: ModifierFlags::NONE })));
        assert!(
            found.contains(&("Shift+Control+Option+Command+§", Route::Section {
                modifiers: ModifierFlags::SHIFT
                    | ModifierFlags::CONTROL
                    | ModifierFlags::OPTION
                    | ModifierFlags::COMMAND
            }))
        );
        assert_eq!(p.bindings.len(), 2);
    }

    #[test]
    fn empty_and_blank_commands_capture_without_spawn() {
        let p = plan(&[
            ("Command+H", one("")),
            ("Command+J", one("   ")),
            ("Command+K", StartupCommands::Multiple(vec![])),
            (
                "Command+L",
                StartupCommands::Multiple(vec![String::new(), "  \t".into()]),
            ),
            (
                "Command+M",
                StartupCommands::Multiple(vec![" a ".into(), String::new(), "b".into()]),
            ),
        ]);
        assert_eq!(p.bindings.len(), 5);
        for binding in &p.bindings[..4] {
            assert!(binding.commands.is_empty(), "{}", binding.raw);
        }
        assert_eq!(p.bindings[4].commands, ["a", "b"]);
    }

    #[test]
    fn quoted_commands_stay_opaque() {
        let quoted = r#"open -a "My App" --flag 'x y' \"#;
        let p = plan(&[("Command+Q", one(&format!("  {quoted}  ")))]);
        assert_eq!(p.bindings[0].commands, [quoted]);
        let broken = plan(&[("Command+W", one("echo \"unterminated"))]);
        assert_eq!(broken.bindings[0].commands, ["echo \"unterminated"]);
        assert!(broken.diagnostics.is_empty());
    }

    #[test]
    fn malformed_entries_are_skipped_without_dropping_valid_ones() {
        let p = plan(&[
            ("", one("x")),
            ("Command++R", one("x")),
            ("Shift", one("x")),
            ("Hyper+R", one("x")),
            ("Command+Control+R", one("ok")),
            ("F24", one("ok2")),
        ]);
        assert_eq!(p.bindings.len(), 2);
        assert_eq!(p.diagnostics.len(), 4);
        assert!(p.diagnostics.iter().all(|d| matches!(d, PlanDiagnostic::Invalid { .. })));
    }

    #[test]
    fn sample_shortcuts_plan() {
        let p = plan(&[
            ("Command+Control+R", one("stache reload")),
            ("Command+Control+W", one("stache wallpaper set --random")),
            (
                "Command+Control+Shift+W",
                one("stache wallpaper set --random --screen main"),
            ),
            (
                "Command+Control+T",
                StartupCommands::Multiple(vec!["open -a Terminal".into(), "stache reload".into()]),
            ),
        ]);
        assert!(p.diagnostics.is_empty());
        assert_eq!(p.bindings.len(), 4);
        assert_eq!(
            p.bindings[0].route,
            ordinary(Code::KeyR, ModifierFlags::COMMAND | ModifierFlags::CONTROL)
        );
    }
}
