//! KB-04-02: strict `hidutil` parsing and edit plans.

use super::super::journal::{Journal, Phase};
use super::super::mapping::*;
use super::super::storage::JOURNAL_MAX_BYTES;

const OTHER: Mapping = Mapping {
    src: 0x7_0000_00E1,
    dst: 0x7_0000_00E5,
};
const OTHER_2: Mapping = Mapping { src: 100, dst: 200 };

fn entry(src: &str, dst: &str) -> String {
    format!(
        "        {{\n        HIDKeyboardModifierMappingDst = {dst};\n        HIDKeyboardModifierMappingSrc = {src};\n    }}"
    )
}

fn parse(text: &str) -> Result<Vec<Mapping>, MappingError> {
    parse_user_key_mapping(text.as_bytes())
}

#[test]
fn kb_04_02_real_output_shapes_parse() {
    assert_eq!(parse("(null)\n"), Ok(vec![]));
    assert_eq!(parse("  (null)  "), Ok(vec![]));
    assert_eq!(parse("()"), Ok(vec![]));
    assert_eq!(parse("(\n)\n"), Ok(vec![]));
    let two = format!(
        "(\n{},\n{}\n)\n",
        entry("30064771129", "30064771181"),
        entry("100", "200")
    );
    assert_eq!(parse(&two), Ok(vec![OWNED, OTHER_2]));
    // Trailing comma and the legacy wrapped form are the same value.
    let trailing = format!("(\n{},\n)\n", entry("100", "200"));
    assert_eq!(parse(&trailing), Ok(vec![OTHER_2]));
    let wrapped = format!(
        "{{\n    UserKeyMapping = (\n{}\n    );\n}}\n",
        entry("100", "200")
    );
    assert_eq!(parse(&wrapped), Ok(vec![OTHER_2]));
    assert_eq!(
        parse(&render_openstep(&[OTHER, OWNED, OTHER_2])),
        Ok(vec![OTHER, OWNED, OTHER_2])
    );
    assert_eq!(OWNED.src, 30_064_771_129);
    assert_eq!(OWNED.dst, 30_064_771_181);
}

#[test]
fn kb_04_02_unknown_truncated_malformed_null_and_quoted_output_is_refused() {
    let good = format!("(\n{}\n)\n", entry("100", "200"));
    assert!(parse(&good).is_ok());
    let cases: Vec<(&str, String, MappingError)> = vec![
        ("empty", String::new(), MappingError::Truncated),
        ("blank", "   \n".into(), MappingError::Truncated),
        ("open only", "(".into(), MappingError::Truncated),
        ("cut inside entry", good[..good.len() - 12].to_owned(), MappingError::Truncated),
        ("trailing", format!("{good}extra"), MappingError::Trailing),
        ("two values", format!("{good}{good}"), MappingError::Trailing),
        (
            "unknown key",
            "( { HIDKeyboardModifierMappingSrc = 1; HIDKeyboardModifierMappingDst = 2; Vendor = 3; } )".into(),
            MappingError::UnknownKey("Vendor".into()),
        ),
        (
            "wrapper unknown key",
            "{ Other = ( ); }".into(),
            MappingError::UnknownKey("Other".into()),
        ),
        (
            "missing dst",
            "( { HIDKeyboardModifierMappingSrc = 1; } )".into(),
            MappingError::MissingField("HIDKeyboardModifierMappingDst"),
        ),
        (
            "missing src",
            "( { HIDKeyboardModifierMappingDst = 1; } )".into(),
            MappingError::MissingField("HIDKeyboardModifierMappingSrc"),
        ),
        (
            "duplicate key",
            "( { HIDKeyboardModifierMappingSrc = 1; HIDKeyboardModifierMappingSrc = 2; } )".into(),
            MappingError::DuplicateKey("HIDKeyboardModifierMappingSrc".into()),
        ),
        (
            "quoted number",
            "( { HIDKeyboardModifierMappingSrc = \"1\"; HIDKeyboardModifierMappingDst = 2; } )".into(),
            MappingError::Quoted,
        ),
        (
            "quoted key",
            "( { \"HIDKeyboardModifierMappingSrc\" = 1; } )".into(),
            MappingError::Quoted,
        ),
        (
            "null value",
            "( { HIDKeyboardModifierMappingSrc = null; HIDKeyboardModifierMappingDst = 2; } )".into(),
            MappingError::Null,
        ),
        ("null entry", "( null )".into(), MappingError::Null),
        ("null among entries", "(null, null)".into(), MappingError::Null),
        (
            "hex number",
            "( { HIDKeyboardModifierMappingSrc = 0x39; HIDKeyboardModifierMappingDst = 2; } )".into(),
            MappingError::BadNumber("0x39".into()),
        ),
        (
            "negative number",
            "( { HIDKeyboardModifierMappingSrc = -1; HIDKeyboardModifierMappingDst = 2; } )".into(),
            MappingError::Malformed("unexpected `-`".into()),
        ),
        (
            "overflowing number",
            "( { HIDKeyboardModifierMappingSrc = 18446744073709551616; HIDKeyboardModifierMappingDst = 2; } )".into(),
            MappingError::BadNumber("18446744073709551616".into()),
        ),
        (
            "missing semicolon",
            "( { HIDKeyboardModifierMappingSrc = 1 HIDKeyboardModifierMappingDst = 2; } )".into(),
            MappingError::Malformed("expected Semicolon, found Word(\"HIDKeyboardModifierMappingDst\")".into()),
        ),
        (
            "not a mapping at all",
            "Error could not read property".into(),
            MappingError::Malformed("unexpected Word(\"Error\")".into()),
        ),
        (
            "two caps sources",
            "( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 1; }, { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771181; } )".into(),
            MappingError::DuplicateCapsSource,
        ),
    ];
    for (name, text, expected) in cases {
        assert_eq!(parse(&text), Err(expected), "{name}");
    }
    assert_eq!(parse_user_key_mapping(&[0xFF, 0xFE]), Err(MappingError::NotText));
    assert_eq!(parse_user_key_mapping(b"(\0)"), Err(MappingError::NotText));
}

#[test]
fn kb_04_02_the_entry_count_is_bounded() {
    let entries = vec![
        "{ HIDKeyboardModifierMappingSrc = 1; HIDKeyboardModifierMappingDst = 2; }";
        MAX_ENTRIES
    ];
    assert_eq!(
        parse(&format!("({})", entries.join(","))).map(|m| m.len()),
        Ok(MAX_ENTRIES)
    );
    let entries = vec![
        "{ HIDKeyboardModifierMappingSrc = 1; HIDKeyboardModifierMappingDst = 2; }";
        MAX_ENTRIES + 1
    ];
    assert_eq!(
        parse(&format!("({})", entries.join(","))),
        Err(MappingError::TooMany)
    );
}

#[test]
fn kb_04_02_duplicate_non_caps_sources_are_preserved_exactly() {
    let text = "( { HIDKeyboardModifierMappingSrc = 5; HIDKeyboardModifierMappingDst = 6; }, { HIDKeyboardModifierMappingSrc = 5; HIDKeyboardModifierMappingDst = 7; } )";
    assert_eq!(
        parse(text),
        Ok(vec![Mapping { src: 5, dst: 6 }, Mapping { src: 5, dst: 7 }])
    );
}

#[test]
fn kb_04_02_any_unowned_caps_mapping_is_classified_foreign_even_f18() {
    assert_eq!(caps_state(&[OTHER]), CapsState::Absent);
    assert_eq!(caps_state(&[OTHER, OWNED]), CapsState::Surrogate);
    assert_eq!(
        caps_state(&[Mapping {
            src: CAPS_LOCK_USAGE,
            dst: 0x7_0000_00E0
        }]),
        CapsState::Foreign(0x7_0000_00E0)
    );
}

#[test]
fn kb_04_04_edits_preserve_unrelated_mappings_exactly_and_remove_only_the_owned_entry() {
    let current = vec![OTHER, OTHER_2, Mapping { src: 9, dst: 9 }];
    let applied = with_surrogate(&current);
    assert_eq!(&applied[..3], current.as_slice());
    assert_eq!(applied[3], OWNED);
    assert_eq!(without_surrogate(&applied), current);
    // A foreign Caps entry is never removed by the restore plan.
    let foreign = Mapping { src: CAPS_LOCK_USAGE, dst: 5 };
    assert_eq!(without_surrogate(&[OTHER, foreign]), vec![OTHER, foreign]);
    // An empty result is a valid, explicit clear.
    assert_eq!(render_set_payload(&[]), "{\"UserKeyMapping\":[]}");
    assert_eq!(
        render_set_payload(&[OWNED]),
        "{\"UserKeyMapping\":[{\"HIDKeyboardModifierMappingSrc\":30064771129,\"HIDKeyboardModifierMappingDst\":30064771181}]}"
    );
}

#[test]
fn kb_04_03_the_journal_round_trips_and_refuses_anything_it_did_not_write() {
    for phase in [
        Phase::ApplyPending,
        Phase::Applied,
        Phase::RestorePending,
        Phase::OwnershipLost,
    ] {
        let journal = Journal::new(phase).next(phase);
        assert_eq!(Journal::decode(&journal.encode()), Ok(journal));
    }
    let good = String::from_utf8(Journal::new(Phase::Applied).encode()).expect("utf8");
    assert!(good.contains("\"version\":1"));
    assert!(good.contains("\"identity\":\"com.marcosmoura.stache.next\""));
    let bad: Vec<(&str, String)> = vec![
        ("version", good.replace("\"version\":1", "\"version\":2")),
        (
            "identity",
            good.replace("com.marcosmoura.stache.next", "com.marcosmoura.stache"),
        ),
        ("destination", good.replace("30064771181", "1")),
        ("source", good.replace("30064771129", "1")),
        ("prior", good.replace("absent", "mapped")),
        (
            "extra field",
            good.replace("\"revision\"", "\"extra\":1,\"revision\""),
        ),
        ("phase", good.replace("applied", "restored")),
        ("truncated", good[..good.len() - 4].to_owned()),
        ("empty", String::new()),
        ("legacy shape", "{\"previous_caps_usage\":null}".to_owned()),
    ];
    for (name, text) in bad {
        assert!(Journal::decode(text.as_bytes()).is_err(), "{name}");
    }
    assert!(Journal::decode(&vec![b' '; JOURNAL_MAX_BYTES + 1]).is_err());
}
