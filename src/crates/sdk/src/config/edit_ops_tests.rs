use serde_json::json;

use super::*;

fn key(name: &str) -> PathSegment { PathSegment::Key(name.to_owned()) }

const fn at(index: u32) -> PathSegment { PathSegment::Index(index) }

fn edited(source: &str, op: &ConfigEditOp) -> String {
    match apply_edit(source, op).expect("edit") {
        ConfigEdit::Changed(text) => text,
        ConfigEdit::Unchanged => panic!("expected a change"),
    }
}

const TILING: &str = r#"#:schema https://example.invalid/tiling.schema.json

# Tiling is opt-in.
enabled = true # kept

# Gaps between windows.
[gaps]
inner = 10 # inner note
outer = 14

# The first workspace.
[[workspaces]]
name = "terminal" # term
layout = "monocle"
rules = [{ app_id = "com.mitchellh.ghostty" }]

# The second workspace.
[[workspaces]]
name = "coding"
layout = "dwindle"
rules = [
  { app_id = "dev.zed.Zed" }, # zed
  { app_id = "com.apple.finder" }, # finder
]

[[workspaces]]
name = "browser"
layout = "monocle"
"#;

const KEYS: &str = "#:schema x\r\n\r\n[bindings]\r\n\"CapsLock+a\" = \"open -a Zed\" # editor\r\n\"Cmd+Q\" = \"quit\"";

#[test]
fn set_replaces_only_the_value_span() {
    let op = ConfigEditOp::Set {
        path: vec![key("gaps"), key("inner")],
        value: json!(12),
    };
    let after = edited(TILING, &op);
    assert_eq!(after, TILING.replacen("inner = 10", "inner = 12", 1));
    assert!(after.contains("inner = 12 # inner note"));
    let op = ConfigEditOp::Set {
        path: vec![key("workspaces"), at(1), key("rules"), at(1), key("app_id")],
        value: json!("com.apple.Safari"),
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        TILING.replacen("\"com.apple.finder\"", "\"com.apple.Safari\"", 1)
    );
}

#[test]
fn set_keeps_crlf_quoted_keys_and_a_missing_trailing_newline() {
    let op = ConfigEditOp::Set {
        path: vec![key("bindings"), key("Cmd+Q")],
        value: json!("exit"),
    };
    let after = edited(KEYS, &op);
    assert_eq!(after, KEYS.replace("\"quit\"", "\"exit\""));
    let op = ConfigEditOp::Set {
        path: vec![key("bindings"), key("Cmd+W")],
        value: json!("close"),
    };
    let after = edited(KEYS, &op);
    assert_eq!(after, format!("{KEYS}\r\n\"Cmd+W\" = \"close\"\r\n"));
}

#[test]
fn set_inserts_a_missing_key_after_the_last_key_of_its_table() {
    let op = ConfigEditOp::Set {
        path: vec![key("gaps"), key("screen")],
        value: json!(4),
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        TILING.replacen("outer = 14\n", "outer = 14\nscreen = 4\n", 1)
    );
    assert!(after.contains("outer = 14\nscreen = 4\n\n# The first workspace."));
    // A missing parent table becomes a block at the end of the file.
    let op = ConfigEditOp::Set {
        path: vec![key("master"), key("ratio")],
        value: json!(60),
    };
    let after = edited(TILING, &op);
    assert_eq!(after, format!("{TILING}\n[master]\nratio = 60\n"));
    // Inside an array of tables, a new object is written inline.
    let op = ConfigEditOp::Set {
        path: vec![key("workspaces"), at(2), key("gaps"), key("inner")],
        value: json!(2),
    };
    let after = edited(TILING, &op);
    assert_eq!(after, format!("{TILING}gaps = {{ inner = 2 }}\n"));
}

#[test]
fn set_null_removes_and_a_whole_table_is_set_member_by_member() {
    let op = ConfigEditOp::Set {
        path: vec![key("gaps"), key("outer")],
        value: json!(null),
    };
    let after = edited(TILING, &op);
    assert_eq!(after, TILING.replacen("outer = 14\n", "", 1));
    let op = ConfigEditOp::Set {
        path: vec![key("gaps")],
        value: json!({ "inner": 10, "outer": 20 }),
    };
    let after = edited(TILING, &op);
    assert_eq!(after, TILING.replacen("outer = 14", "outer = 20", 1));
}

#[test]
fn remove_deletes_a_key_line_an_element_and_a_table_element() {
    let op = ConfigEditOp::Remove { path: vec![key("enabled")] };
    let after = edited(TILING, &op);
    assert_eq!(after, TILING.replacen("enabled = true # kept\n", "", 1));
    let op = ConfigEditOp::Remove {
        path: vec![key("workspaces"), at(1), key("rules"), at(0)],
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        TILING.replacen("{ app_id = \"dev.zed.Zed\" }, # zed\n  ", "", 1)
    );
    let op = ConfigEditOp::Remove {
        path: vec![key("workspaces"), at(2)],
    };
    let after = edited(TILING, &op);
    assert!(after.ends_with("# finder\n]\n\n"), "{after}");
    assert!(!after.contains("browser"));
    // A missing key is already the default.
    assert_eq!(
        apply_edit(TILING, &ConfigEditOp::Remove { path: vec![key("absent")] }).expect("edit"),
        ConfigEdit::Unchanged
    );
    // Removing the only element of an inline array leaves an empty array.
    let op = ConfigEditOp::Remove {
        path: vec![key("workspaces"), at(0), key("rules"), at(0)],
    };
    assert!(edited(TILING, &op).contains("rules = []\n"));
}

#[test]
fn remove_keeps_crlf_and_quoted_keys() {
    let op = ConfigEditOp::Remove {
        path: vec![key("bindings"), key("CapsLock+a")],
    };
    let after = edited(KEYS, &op);
    assert_eq!(after, "#:schema x\r\n\r\n[bindings]\r\n\"Cmd+Q\" = \"quit\"");
}

#[test]
fn insert_adds_array_elements_and_array_of_table_elements() {
    let op = ConfigEditOp::Insert {
        path: vec![key("workspaces"), at(1), key("rules")],
        index: 2,
        value: json!({ "app_id": "com.apple.Terminal" }),
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        TILING.replacen(
            "\"com.apple.finder\" },",
            "\"com.apple.finder\" },\n  { app_id = \"com.apple.Terminal\" },",
            1
        )
    );
    let op = ConfigEditOp::Insert {
        path: vec![key("workspaces"), at(0), key("rules")],
        index: 0,
        value: json!({ "app_id": "a" }),
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        TILING.replacen("rules = [{", "rules = [{ app_id = \"a\" }, {", 1)
    );
    let op = ConfigEditOp::Insert {
        path: vec![key("workspaces")],
        index: 3,
        value: json!({ "name": "music", "layout": "floating" }),
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        format!("{TILING}\n[[workspaces]]\nlayout = \"floating\"\nname = \"music\"\n")
    );
    let op = ConfigEditOp::Insert {
        path: vec![key("workspaces")],
        index: 1,
        value: json!({ "name": "music" }),
    };
    let after = edited(TILING, &op);
    assert_eq!(
        after,
        TILING.replacen(
            "# The second workspace.",
            "[[workspaces]]\nname = \"music\"\n\n# The second workspace.",
            1
        )
    );
    assert!(after.contains("rules = [{ app_id = \"com.mitchellh.ghostty\" }]\n\n[[workspaces]]\nname = \"music\"\n\n# The second workspace."));
}

#[test]
fn move_reorders_elements_with_their_comments() {
    let op = ConfigEditOp::Move {
        path: vec![key("workspaces")],
        from: 1,
        to: 0,
    };
    let after = edited(TILING, &op);
    let second = "# The second workspace.\n[[workspaces]]\nname = \"coding\"";
    let first = "# The first workspace.\n[[workspaces]]\nname = \"terminal\"";
    assert!(after.find(second) < after.find(first), "{after}");
    assert!(after.starts_with(&TILING[..TILING.find("# The first").expect("first")]));
    assert!(after.ends_with("[[workspaces]]\nname = \"browser\"\nlayout = \"monocle\"\n"));
    let op = ConfigEditOp::Move {
        path: vec![key("workspaces"), at(1), key("rules")],
        from: 1,
        to: 0,
    };
    let after = edited(TILING, &op);
    assert!(after.contains(
        "rules = [\n  { app_id = \"com.apple.finder\" }, # finder\n  { app_id = \"dev.zed.Zed\" }, # zed\n]"
    ));
    let source = "list = [1, 2, 3] # c\n";
    let op = ConfigEditOp::Move {
        path: vec![key("list")],
        from: 0,
        to: 2,
    };
    assert_eq!(edited(source, &op), "list = [2, 3, 1] # c\n");
}

#[test]
fn rename_changes_only_the_key() {
    let op = ConfigEditOp::Rename {
        path: vec![key("bindings")],
        from: "CapsLock+a".to_owned(),
        to: "CapsLock+e".to_owned(),
    };
    let after = edited(KEYS, &op);
    assert_eq!(after, KEYS.replacen("CapsLock+a", "CapsLock+e", 1));
    let op = ConfigEditOp::Rename {
        path: vec![key("bindings")],
        from: "Cmd+Q".to_owned(),
        to: "CapsLock+a".to_owned(),
    };
    assert!(matches!(
        apply_edit(KEYS, &op),
        Err(ConfigEditError::KeyExists { .. })
    ));
    let source = "[a]\nk = 1\n\n[a.b]\nz = 2\n";
    let op = ConfigEditOp::Rename {
        path: vec![],
        from: "a".to_owned(),
        to: "c".to_owned(),
    };
    assert_eq!(edited(source, &op), "[c]\nk = 1\n\n[c.b]\nz = 2\n");
}

#[test]
fn path_refusals_are_typed() {
    let op = ConfigEditOp::Set {
        path: vec![key("enabled"), key("x")],
        value: json!(1),
    };
    assert!(matches!(
        apply_edit(TILING, &op),
        Err(ConfigEditError::NotATable { .. })
    ));
    let op = ConfigEditOp::Set {
        path: vec![key("workspaces"), at(9), key("name")],
        value: json!("x"),
    };
    assert!(matches!(
        apply_edit(TILING, &op),
        Err(ConfigEditError::IndexOutOfRange { index: 9, len: 3, .. })
    ));
    let op = ConfigEditOp::Move {
        path: vec![key("workspaces")],
        from: 0,
        to: 3,
    };
    assert!(matches!(
        apply_edit(TILING, &op),
        Err(ConfigEditError::IndexOutOfRange { .. })
    ));
    let op = ConfigEditOp::Insert {
        path: vec![key("gaps")],
        index: 0,
        value: json!(1),
    };
    assert!(matches!(
        apply_edit(TILING, &op),
        Err(ConfigEditError::NotAnArray { .. })
    ));
    let op = ConfigEditOp::Set {
        path: vec![key("list")],
        value: json!([1, null]),
    };
    assert!(matches!(
        apply_edit(TILING, &op),
        Err(ConfigEditError::Unrepresentable(_))
    ));
    assert!(matches!(
        apply_edit("a = \n", &ConfigEditOp::Remove { path: vec![key("a")] }),
        Err(ConfigEditError::Parse(_))
    ));
}

#[test]
fn the_hash_is_fnv1a_over_the_exact_bytes() {
    assert_eq!(content_hash(b""), "cbf29ce484222325");
    assert_eq!(content_hash(b"a"), "af63dc4c8601ec8c");
    assert_ne!(content_hash(b"a\n"), content_hash(b"a\r\n"));
}

#[test]
fn ops_deserialize_from_the_wire_shape() {
    let op: ConfigEditOp = serde_json::from_value(json!({
        "op": "set", "path": ["workspaces", 1, "name"], "value": "x"
    }))
    .expect("decode");
    assert_eq!(op, ConfigEditOp::Set {
        path: vec![key("workspaces"), at(1), key("name")],
        value: json!("x"),
    });
    assert_eq!(
        display_path(&[key("bindings"), key("Cmd+Q"), at(2)]),
        "bindings.\"Cmd+Q\"[2]"
    );
}

/// A deterministic xorshift generator; no external randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound.max(1)).unwrap_or(1)).unwrap_or(0)
    }
}

fn random_op(rng: &mut Rng, document: &Json) -> ConfigEditOp {
    let count = |path: &[PathSegment]| -> usize {
        let mut node = document;
        for segment in path {
            node = match (node, segment) {
                (Json::Object(map), PathSegment::Key(name)) => match map.get(name) {
                    Some(next) => next,
                    None => return 0,
                },
                (Json::Array(items), PathSegment::Index(index)) => match items.get(*index as usize)
                {
                    Some(next) => next,
                    None => return 0,
                },
                _ => return 0,
            };
        }
        node.as_array().map_or(0, Vec::len)
    };
    let workspaces = count(&[key("workspaces")]);
    let pick = |rng: &mut Rng, len: usize| index_u32(rng.below(len));
    match rng.below(7) {
        0 => ConfigEditOp::Set {
            path: vec![key("gaps"), key(["inner", "outer", "extra"][rng.below(3)])],
            value: json!(rng.below(40)),
        },
        1 if workspaces > 0 => ConfigEditOp::Set {
            path: vec![key("workspaces"), at(pick(rng, workspaces)), key("name")],
            value: json!(format!("w{}", rng.below(100))),
        },
        2 if workspaces > 1 => ConfigEditOp::Move {
            path: vec![key("workspaces")],
            from: pick(rng, workspaces),
            to: pick(rng, workspaces),
        },
        3 if workspaces > 1 => ConfigEditOp::Remove {
            path: vec![key("workspaces"), at(pick(rng, workspaces))],
        },
        4 if workspaces > 0 => ConfigEditOp::Insert {
            path: vec![key("workspaces")],
            index: pick(rng, workspaces + 1),
            value: json!({ "name": format!("n{}", rng.below(100)) }),
        },
        5 => {
            let index = pick(rng, workspaces);
            let rules = count(&[key("workspaces"), at(index), key("rules")]);
            if rules > 1 {
                ConfigEditOp::Move {
                    path: vec![key("workspaces"), at(index), key("rules")],
                    from: pick(rng, rules),
                    to: pick(rng, rules),
                }
            } else {
                ConfigEditOp::Remove {
                    path: vec![key("gaps"), key("extra")],
                }
            }
        }
        _ => ConfigEditOp::Remove {
            path: vec![key("gaps"), key(["inner", "outer", "extra"][rng.below(3)])],
        },
    }
}

#[test]
fn random_edit_sequences_stay_valid_and_keep_unrelated_comments() {
    let protected = [
        "#:schema https://example.invalid/tiling.schema.json",
        "# Tiling is opt-in.",
        "# Gaps between windows.",
    ];
    for seed in 1..=40_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut text = TILING.to_owned();
        let mut model = document_json(&text).expect("model");
        for _ in 0..12 {
            let op = random_op(&mut rng, &model);
            let mut expected = model.clone();
            apply_json(&mut expected, &op).expect("model op");
            match apply_edit(&text, &op).expect("edit") {
                ConfigEdit::Changed(next) => text = next,
                ConfigEdit::Unchanged => {}
            }
            model = document_json(&text).expect("candidate parses");
            assert_eq!(model, expected, "seed {seed}: {op:?}\n{text}");
            for line in protected {
                assert!(text.contains(line), "seed {seed} lost `{line}`:\n{text}");
            }
            assert!(text.starts_with("#:schema "), "seed {seed}");
        }
    }
}

#[test]
fn every_op_on_a_crlf_file_matches_the_lf_edit_line_for_line() {
    let crlf = TILING.replace('\n', "\r\n");
    let no_trailing = TILING.trim_end_matches('\n');
    let ops = [
        ConfigEditOp::Set {
            path: vec![key("gaps"), key("inner")],
            value: json!(3),
        },
        ConfigEditOp::Set {
            path: vec![key("gaps"), key("screen")],
            value: json!(3),
        },
        ConfigEditOp::Remove {
            path: vec![key("workspaces"), at(0)],
        },
        ConfigEditOp::Insert {
            path: vec![key("workspaces")],
            index: 3,
            value: json!({ "name": "x" }),
        },
        ConfigEditOp::Insert {
            path: vec![key("workspaces"), at(1), key("rules")],
            index: 1,
            value: json!({ "app_id": "x" }),
        },
        ConfigEditOp::Move {
            path: vec![key("workspaces")],
            from: 2,
            to: 0,
        },
        ConfigEditOp::Move {
            path: vec![key("workspaces"), at(1), key("rules")],
            from: 0,
            to: 1,
        },
        ConfigEditOp::Rename {
            path: vec![],
            from: "gaps".to_owned(),
            to: "spacing".to_owned(),
        },
    ];
    for op in &ops {
        let lf = edited(TILING, op);
        assert_eq!(edited(&crlf, op), lf.replace('\n', "\r\n"), "{op:?}");
        // Without a trailing newline the edit stays valid and keeps the rest.
        let tail = edited(no_trailing, op);
        let expected = document_json(&lf).expect("lf");
        assert_eq!(document_json(&tail).expect("tail"), expected, "{op:?}\n{tail}");
        assert!(tail.starts_with("#:schema "), "{op:?}");
    }
}

#[test]
fn set_into_a_dotted_table_writes_a_dotted_line() {
    let op = ConfigEditOp::Set {
        path: vec![key("a"), key("c")],
        value: json!(2),
    };
    assert_eq!(edited("x = 0\na.b = 1\n", &op), "x = 0\na.b = 1\na.c = 2\n");
    let op = ConfigEditOp::Set {
        path: vec![key("w"), at(0), key("a"), key("c")],
        value: json!(2),
    };
    assert_eq!(
        edited("[[w]]\nname = \"one\"\na.b = 1\n", &op),
        "[[w]]\nname = \"one\"\na.b = 1\na.c = 2\n"
    );
}

#[test]
fn removing_the_last_dotted_key_drops_the_empty_dotted_parent() {
    let op = ConfigEditOp::Remove { path: vec![key("a"), key("b")] };
    assert_eq!(edited("x = 0\na.b = 1\n", &op), "x = 0\n");
    // Not the last key: only that line goes.
    assert_eq!(edited("x = 0\na.b = 1\na.c = 2\n", &op), "x = 0\na.c = 2\n");
    let op = ConfigEditOp::Remove {
        path: vec![key("w"), at(0), key("a"), key("b")],
    };
    assert_eq!(
        edited("[[w]]\nname = \"one\"\na.b = 1\n", &op),
        "[[w]]\nname = \"one\"\n"
    );
}
