//! KB-03-03: the pure `§` resolver over ANSI, ISO and missing-data fixtures.

use stache_sdk::input::ModifierFlags;

use super::resolver::{self, CARBON_CONTROL, CARBON_OPTION, CARBON_SHIFT};
use super::{FallbackReason, ISO_SECTION_KEY, LayoutRead, SectionSource, select_candidate};
use crate::hotkeys::testing::FakeLayout;

const SECTION: u16 = 0x00A7;

fn available(layout: FakeLayout) -> LayoutRead { LayoutRead::Available(Box::new(layout)) }

#[test]
fn ansi_needs_option_on_the_six_key() {
    let resolution = resolver::resolve(&available(FakeLayout::ansi()));
    assert_eq!(resolution.key_code, 0x16);
    assert_eq!(resolution.source, SectionSource::Layout {
        required: ModifierFlags::OPTION
    });
    assert_eq!(
        resolution.chord(ModifierFlags::NONE),
        (0x16, ModifierFlags::OPTION)
    );
}

#[test]
fn iso_uses_the_dedicated_key_with_no_modifier() {
    let resolution = resolver::resolve(&available(FakeLayout::iso()));
    assert_eq!(resolution.key_code, ISO_SECTION_KEY);
    assert_eq!(resolution.source, SectionSource::Layout {
        required: ModifierFlags::NONE
    });
    assert_eq!(resolution.source.diagnostic(), None);
}

#[test]
fn required_modifiers_are_or_ed_with_configured_ones() {
    let german = resolver::resolve(&available(FakeLayout::german()));
    assert_eq!(
        german.chord(ModifierFlags::COMMAND),
        (0x14, ModifierFlags::COMMAND | ModifierFlags::SHIFT)
    );
    // A configured modifier that the layout also needs is not doubled.
    assert_eq!(german.chord(ModifierFlags::SHIFT), (0x14, ModifierFlags::SHIFT));
}

#[test]
fn missing_layout_data_falls_back_with_an_honest_diagnostic() {
    let resolution = resolver::resolve(&LayoutRead::Unavailable("no uchr".to_owned()));
    assert_eq!(resolution.key_code, ISO_SECTION_KEY);
    assert_eq!(
        resolution.source,
        SectionSource::Fallback(FallbackReason::LayoutUnavailable("no uchr".to_owned()))
    );
    let note = resolution.source.diagnostic().expect("a fallback is explained");
    assert!(note.contains("no uchr"), "{note}");
    assert!(note.contains("does not guarantee"), "{note}");
    assert_eq!(
        resolution.chord(ModifierFlags::SHIFT),
        (ISO_SECTION_KEY, ModifierFlags::SHIFT)
    );
}

#[test]
fn a_layout_without_section_has_no_candidate() {
    let resolution = resolver::resolve(&available(FakeLayout::without_section()));
    assert_eq!(resolution.key_code, ISO_SECTION_KEY);
    assert_eq!(
        resolution.source,
        SectionSource::Fallback(FallbackReason::NoCandidate)
    );
    assert!(
        resolution
            .source
            .diagnostic()
            .expect("explained")
            .contains("does not guarantee")
    );
}

#[test]
fn candidate_tie_breaks_are_fewest_modifiers_then_bits_then_key() {
    // Fewest modifiers beats a lower key code.
    assert_eq!(select_candidate(&[(10, CARBON_SHIFT), (50, 0)]), Some((50, 0)));
    // Equal count: lower Carbon bits (Shift 0x200 < Option 0x800 < Control 0x1000).
    assert_eq!(
        select_candidate(&[(3, CARBON_OPTION), (9, CARBON_SHIFT)]),
        Some((9, CARBON_SHIFT))
    );
    assert_eq!(
        select_candidate(&[(3, CARBON_CONTROL), (9, CARBON_OPTION)]),
        Some((9, CARBON_OPTION))
    );
    // Equal modifiers: lower key code.
    assert_eq!(
        select_candidate(&[(22, CARBON_OPTION), (10, CARBON_OPTION)]),
        Some((10, CARBON_OPTION))
    );
    assert_eq!(select_candidate(&[]), None);

    // The same rules through a layout with several `§` keys.
    let layout = FakeLayout::new(9, [
        ((0x20, CARBON_OPTION), SECTION),
        ((0x10, CARBON_OPTION), SECTION),
        ((0x30, CARBON_SHIFT | CARBON_OPTION), SECTION),
        ((0x40, CARBON_CONTROL), SECTION),
    ]);
    let resolution = resolver::resolve(&available(layout));
    assert_eq!(
        (resolution.key_code, resolution.chord(ModifierFlags::NONE).1),
        (0x10, ModifierFlags::OPTION)
    );
}

#[test]
fn modifier_conversions_are_exact() {
    use resolver::{CARBON_COMMAND, carbon_modifiers, section_modifiers};
    assert_eq!(carbon_modifiers(ModifierFlags::NONE), Some(0));
    assert_eq!(
        carbon_modifiers(
            ModifierFlags::COMMAND
                | ModifierFlags::SHIFT
                | ModifierFlags::OPTION
                | ModifierFlags::CONTROL
        ),
        Some(CARBON_COMMAND | CARBON_SHIFT | CARBON_OPTION | CARBON_CONTROL)
    );
    assert_eq!(carbon_modifiers(ModifierFlags::CAPS_LOCK), Some(0));
    assert_eq!(carbon_modifiers(ModifierFlags::FUNCTION), None);
    assert_eq!(
        section_modifiers(CARBON_SHIFT | CARBON_CONTROL),
        ModifierFlags::SHIFT | ModifierFlags::CONTROL
    );
}
