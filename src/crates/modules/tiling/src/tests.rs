//! Configuration fixtures, validation and catalog behavior.

use std::path::Path;

use serde_json::{Value, json};
use stache_sdk::{ConfigCatalog, DiagnosticKind, ModuleConfig};

use crate::*;

fn parse(text: &str) -> Result<TilingConfig, String> {
    let config: TilingConfig = serde_json::from_str(text).map_err(|e| e.to_string())?;
    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}

fn accepted(value: &Value) -> TilingConfig {
    parse(&value.to_string()).unwrap_or_else(|error| panic!("{value}: {error}"))
}

fn rejected(value: &Value) -> String {
    parse(&value.to_string()).expect_err(&format!("{value} must be rejected"))
}

fn catalog() -> ConfigCatalog {
    let mut catalog = ConfigCatalog::new();
    catalog.register::<TilingConfig>(TilingConfig::ID).expect("register");
    catalog
}

#[test]
fn defaults_match_the_contract() {
    let config = TilingConfig::default();
    assert!(!config.enabled());
    assert_eq!(config.default_layout, LayoutType::Dwindle);
    assert!(config.workspaces.is_empty() && config.ignore.is_empty());
    assert_eq!(config.animations, AnimationConfig {
        enabled: false,
        duration: 200,
        easing: EasingType::EaseOut
    });
    assert_eq!(config.floating.default_position, FloatingPosition::Center);
    assert!(config.floating.presets.is_empty());
    assert_eq!(config.master, MasterConfig {
        ratio: 60,
        position: MasterPosition::Auto
    });
    assert_eq!(config.gaps, GapsConfigValue::Global(GapsConfig::default()));
    assert_eq!(GapValue::default(), GapValue::Uniform(0));
    assert!(!config.borders.enabled);
    config.validate().expect("defaults are valid");
}

#[test]
fn empty_object_equals_the_default() {
    assert_eq!(accepted(&json!({})), TilingConfig::default());
}

#[test]
fn all_eight_layouts_and_six_easings_parse_and_serialize() {
    let layouts = [
        "dwindle",
        "split",
        "split-vertical",
        "split-horizontal",
        "monocle",
        "master",
        "grid",
        "floating",
    ];
    for name in layouts {
        let config = accepted(&json!({ "defaultLayout": name }));
        assert_eq!(config.default_layout.as_str(), name);
        assert_eq!(serde_json::to_value(config.default_layout).unwrap(), json!(name));
    }
    for name in [
        "linear",
        "ease-in",
        "ease-out",
        "ease-in-out",
        "ease-out-expo",
        "spring",
    ] {
        let config = accepted(&json!({ "animations": { "easing": name } }));
        assert_eq!(
            serde_json::to_value(config.animations.easing).unwrap(),
            json!(name)
        );
    }
    rejected(&json!({ "defaultLayout": "Dwindle" }));
    rejected(&json!({ "animations": { "easing": "bounce" } }));
}

#[test]
fn gaps_accept_every_legacy_shape() {
    let shapes = [
        json!({ "gaps": { "inner": 8, "outer": 12 } }),
        json!({ "gaps": { "inner": { "horizontal": 4, "vertical": 6 }, "outer": { "horizontal": 1, "vertical": 2 } } }),
        json!({ "gaps": { "outer": { "top": 1, "right": 2, "bottom": 3, "left": 4 } } }),
        json!({ "gaps": [
            { "screen": "main", "inner": 4, "outer": { "top": 1, "right": 2, "bottom": 3, "left": 4 } },
            { "screen": "Studio Display" },
            { "screen": "secondary", "inner": { "horizontal": 1, "vertical": 2 } },
        ] }),
    ];
    for shape in shapes {
        let config = accepted(&shape);
        let again = serde_json::to_value(&config).unwrap();
        assert_eq!(again["gaps"], serde_json::to_value(&config.gaps).unwrap());
        let reparsed: TilingConfig = serde_json::from_value(again).unwrap();
        assert_eq!(reparsed, config, "{shape}");
    }
    let per_axis = accepted(&json!({ "gaps": { "inner": { "horizontal": 4, "vertical": 6 } } }));
    let GapsConfigValue::Global(global) = per_axis.gaps else {
        panic!("global")
    };
    assert_eq!(global.inner, GapValue::PerAxis { horizontal: 4, vertical: 6 });
    let per_display = accepted(&json!({ "gaps": [{ "screen": "main", "inner": 3 }] }));
    let GapsConfigValue::PerScreen(screens) = per_display.gaps else {
        panic!("per screen")
    };
    assert_eq!(screens[0].inner, GapValue::Uniform(3));
    assert_eq!(screens[0].outer, GapValue::Uniform(0));
}

#[test]
fn malformed_gaps_and_overflow_are_rejected() {
    rejected(&json!({ "gaps": { "inner": -1 } }));
    rejected(&json!({ "gaps": { "inner": 1.5 } }));
    rejected(&json!({ "gaps": { "inner": "8" } }));
    rejected(&json!({ "gaps": [{ "inner": 4 }] }));
    let max = u32::MAX;
    let error = rejected(
        &json!({ "gaps": { "outer": { "top": 1, "right": max, "bottom": 1, "left": 1 } } }),
    );
    assert!(error.contains("overflow"), "{error}");
    rejected(
        &json!({ "gaps": [{ "screen": "main", "outer": { "top": max, "right": 1, "bottom": 1, "left": 1 } }] }),
    );
    accepted(&json!({ "gaps": { "outer": { "top": max, "right": 1, "bottom": 0, "left": 1 } } }));
}

#[test]
fn workspaces_default_screen_and_keep_preset_on_open_spelling() {
    let config = accepted(&json!({
        "floating": { "presets": [{ "name": "Wide", "width": "80%", "height": 600 }] },
        "workspaces": [
            { "name": "code" },
            { "name": "web", "layout": "grid", "screen": "secondary",
              "rules": [{ "appId": "com.apple.Safari", "title": "docs" }, {}],
              "preset-on-open": "wide" },
        ],
    }));
    assert_eq!(config.workspaces[0].screen, "main");
    assert_eq!(config.workspaces[0].layout, None);
    assert_eq!(config.workspaces[1].layout, Some(LayoutType::Grid));
    assert!(!config.workspaces[1].rules[1].has_criteria());
    let out = serde_json::to_value(&config).unwrap();
    assert_eq!(out["workspaces"][1]["preset-on-open"], "wide");
    assert!(out["workspaces"][0].get("preset-on-open").is_none());
    assert!(out["workspaces"][1]["rules"][1].as_object().unwrap().is_empty());
    rejected(&json!({ "workspaces": [{ "name": "a", "presetOnOpen": "x", "layout": "nope" }] }));
    rejected(&json!({ "workspaces": [{ "layout": "grid" }] }));
}

#[test]
fn duplicate_names_and_missing_presets_are_rejected() {
    let error = rejected(&json!({ "workspaces": [{ "name": "a" }, { "name": "a" }] }));
    assert!(error.contains("duplicate workspace"), "{error}");
    // Legacy workspace lookup is exact-case, so case variants stay distinct.
    accepted(&json!({ "workspaces": [{ "name": "A" }, { "name": "a" }] }));
    let error = rejected(&json!({ "floating": { "presets": [
        { "name": "p", "width": 100, "height": 100 }, { "name": "P", "width": 1, "height": 1 }
    ] } }));
    assert!(error.contains("duplicate preset"), "{error}");
    let error = rejected(&json!({ "workspaces": [{ "name": "a", "preset-on-open": "missing" }] }));
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn presets_accept_pixels_percentages_positions_and_center() {
    let config = accepted(&json!({ "floating": { "defaultPosition": "default", "presets": [
        { "name": "a", "width": 1440, "height": 900, "x": 10, "y": "5%" },
        { "name": "b", "width": "50%", "height": "100%", "center": true, "x": "10", "y": 0 },
        { "name": "c", "width": " 25.5% ", "height": 300 },
    ] } }));
    assert_eq!(config.floating.default_position, FloatingPosition::Default);
    assert_eq!(
        config.floating.presets[0].y,
        Some(DimensionValue::Percentage("5%".into()))
    );
    assert!(config.floating.presets[1].center);
    let out = serde_json::to_value(&config).unwrap();
    assert_eq!(out["floating"]["presets"][0]["width"], 1440);
    assert_eq!(out["floating"]["presets"][1]["width"], "50%");
    assert!(out["floating"]["presets"][2].get("x").is_none());
}

#[test]
fn malformed_preset_dimensions_are_rejected() {
    for width in [
        json!("abc"),
        json!("-5%"),
        json!("inf%"),
        json!("NaN"),
        json!(0),
        json!("0%"),
        json!(""),
        json!(-1),
    ] {
        rejected(
            &json!({ "floating": { "presets": [{ "name": "a", "width": width, "height": 10 }] } }),
        );
    }
    rejected(
        &json!({ "floating": { "presets": [{ "name": "a", "width": 10, "height": 10, "x": "wide" }] } }),
    );
    rejected(&json!({ "floating": { "presets": [{ "name": "a", "width": 10 }] } }));
    rejected(&json!({ "floating": { "defaultPosition": "left" } }));
}

#[test]
fn master_ratio_is_bounded_and_positions_parse() {
    for position in ["left", "right", "top", "bottom", "auto"] {
        accepted(&json!({ "master": { "ratio": 100, "position": position } }));
    }
    accepted(&json!({ "master": { "ratio": 0 } }));
    rejected(&json!({ "master": { "ratio": 101 } }));
    rejected(&json!({ "master": { "ratio": -1 } }));
    rejected(&json!({ "master": { "ratio": 60.5 } }));
}

#[test]
fn borders_accept_every_state_shape() {
    let config = accepted(&json!({ "borders": {
        "enabled": true, "style": "square", "hidpi": false,
        "focused": { "width": 4, "color": "#89b4fa" },
        "unfocused": false,
        "monocle": { "width": 2, "glow": "rgba(137, 180, 250, 0.2)" },
        "floating": { "width": 3, "gradient": { "from": "#f00", "to": "00ff00aa" } },
        "grid": { "width": 1, "gradient": { "from": "#fff", "to": "#000", "angle": 180.5 },
                  "animation": { "duration": 350 } },
        "splitVertical": false,
        "ignore": [{ "appName": "Preview" }],
    } }));
    let b = &config.borders;
    assert_eq!(b.style, Some(BorderStyle::Square));
    assert_eq!(b.unfocused, BorderStateConfig::Disabled(false));
    let BorderStateConfig::GradientColor { gradient, .. } = &b.floating else {
        panic!()
    };
    assert!((gradient.angle - 90.0).abs() < f64::EPSILON);
    let Some(BorderStateConfig::GradientColor { animation, .. }) = &b.grid else {
        panic!()
    };
    assert_eq!(animation.as_ref().unwrap().easing, EasingType::Linear);
    let out = serde_json::to_value(&config).unwrap();
    assert_eq!(
        out["borders"]["monocle"],
        json!({ "width": 2, "glow": "rgba(137, 180, 250, 0.2)" })
    );
    assert_eq!(out["borders"]["splitVertical"], json!(false));
    assert!(out["borders"].get("dwindle").is_none());
    assert_eq!(serde_json::from_value::<TilingConfig>(out).unwrap(), config);
}

#[test]
fn malformed_borders_are_rejected() {
    for state in [
        json!(true),
        json!({ "width": 4, "color": "#12" }),
        json!({ "width": 4, "color": "#ggg" }),
        json!({ "width": 4, "color": "#ééé" }),
        json!({ "width": 4, "glow": "rgba(1, 2, 3, 2)" }),
        json!({ "width": 4, "glow": "rgb(256, 0, 0)" }),
        json!({ "width": 4, "glow": "rgba(1, 2, 3)" }),
        json!({ "width": 4, "gradient": { "from": "#fff", "to": "nope" } }),
        json!({ "width": 4 }),
        json!({ "width": -1, "color": "#fff" }),
    ] {
        rejected(&json!({ "borders": { "focused": state } }));
    }
    rejected(&json!({ "borders": { "grid": { "width": 1, "color": "red" } } }));
    rejected(&json!({ "borders": { "style": "pill" } }));
    accepted(&json!({ "borders": { "focused": { "width": 4, "color": "rgb(1, 2, 3)" } } }));
    accepted(&json!({ "borders": { "focused": { "width": 4, "color": "89b4fa" } } }));
}

#[test]
fn unsupported_value_types_are_rejected() {
    rejected(&json!({ "enabled": "yes" }));
    rejected(&json!({ "animations": { "duration": -1 } }));
    rejected(&json!({ "workspaces": {} }));
    rejected(&json!({ "ignore": [{ "appId": 3 }] }));
}

#[test]
fn schema_template_and_default_agree() {
    let catalog = catalog();
    let template = catalog.template(TilingConfig::ID).expect("template");
    assert!(template.contains("\"enabled\": false"), "{template}");
    assert!(template.contains("defaultLayout"), "{template}");
    assert_eq!(
        catalog.default_value(TilingConfig::ID).unwrap(),
        &serde_json::to_value(TilingConfig::default()).unwrap()
    );
    let schema = serde_json::to_string(&catalog.schema(TilingConfig::ID).expect("schema")).unwrap();
    assert!(schema.contains("preset-on-open"), "{schema}");
}

#[test]
fn catalog_reports_validation_and_unknown_keys() {
    let catalog = catalog();
    let path = Path::new("tiling.jsonc");
    let warnings = catalog
        .inspect(TilingConfig::ID, path, r#"{ "enabled": true, "extra": 1 }"#)
        .expect("valid");
    assert_eq!(warnings, ["extra"]);
    let diagnostic = catalog
        .inspect(TilingConfig::ID, path, r#"{ "master": { "ratio": 500 } }"#)
        .expect_err("invalid");
    assert_eq!(diagnostic.kind, DiagnosticKind::Validation);
    assert!(diagnostic.message.contains("master.ratio"), "{diagnostic}");
    let diagnostic = catalog
        .inspect(TilingConfig::ID, path, r#"{ "defaultLayout": "tall" }"#)
        .expect_err("invalid");
    assert_ne!(diagnostic.kind, DiagnosticKind::Validation);
}

#[test]
fn migration_keeps_sections_and_warns_about_corrections() {
    let original = json!({
        "enabled": true, "defaultLayout": "grid",
        "floating": { "defaultPosition": "default" },
        "workspaces": [{ "name": "a", "layout": "master" }],
    });
    let mut section = original.clone();
    let notes = TilingConfig::migrate_legacy_section(&mut section);
    assert_eq!(section, original);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("10"), "{notes:?}");

    let mut workspace_grid = json!({ "workspaces": [{ "name": "a", "layout": "grid" }] });
    assert_eq!(
        TilingConfig::migrate_legacy_section(&mut workspace_grid).len(),
        1
    );

    let mut omitted = json!({ "enabled": true });
    let notes = TilingConfig::migrate_legacy_section(&mut omitted);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("defaultPosition")));
    assert!(notes.iter().any(|n| n.contains("dwindle")));

    let mut explicit = json!({ "enabled": true, "defaultLayout": "dwindle",
        "floating": { "defaultPosition": "center" } });
    assert_eq!(TilingConfig::migrate_legacy_section(&mut explicit).len(), 1);

    for quiet in [
        json!({ "enabled": false }),
        json!({}),
        json!([]),
        json!(null),
    ] {
        let mut section = quiet.clone();
        assert!(TilingConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(section, quiet);
    }
}
