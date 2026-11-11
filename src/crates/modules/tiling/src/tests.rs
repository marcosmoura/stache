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
        "split_vertical",
        "split_horizontal",
        "monocle",
        "master",
        "grid",
        "floating",
    ];
    for name in layouts {
        let config = accepted(&json!({ "default_layout": name }));
        assert_eq!(config.default_layout.as_str(), name);
        assert_eq!(serde_json::to_value(config.default_layout).unwrap(), json!(name));
    }
    for name in [
        "linear",
        "ease_in",
        "ease_out",
        "ease_in_out",
        "ease_out_expo",
        "spring",
    ] {
        let config = accepted(&json!({ "animations": { "easing": name } }));
        assert_eq!(
            serde_json::to_value(config.animations.easing).unwrap(),
            json!(name)
        );
    }
    rejected(&json!({ "default_layout": "Dwindle" }));
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
              "rules": [{ "app_id": "com.apple.Safari", "title": "docs" }, {}],
              "preset_on_open": "wide" },
        ],
    }));
    assert_eq!(config.workspaces[0].screen, "main");
    assert_eq!(config.workspaces[0].layout, None);
    assert_eq!(config.workspaces[1].layout, Some(LayoutType::Grid));
    assert!(!config.workspaces[1].rules[1].has_criteria());
    let out = serde_json::to_value(&config).unwrap();
    assert_eq!(out["workspaces"][1]["preset_on_open"], "wide");
    assert!(out["workspaces"][0].get("preset_on_open").is_none());
    assert!(out["workspaces"][1]["rules"][1].as_object().unwrap().is_empty());
    rejected(&json!({ "workspaces": [{ "name": "a", "preset_on_open": "x", "layout": "nope" }] }));
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
    let error = rejected(&json!({ "workspaces": [{ "name": "a", "preset_on_open": "missing" }] }));
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn presets_accept_pixels_percentages_positions_and_center() {
    let config = accepted(
        &json!({ "floating": { "default_position": "default", "presets": [
        { "name": "a", "width": 1440, "height": 900, "x": 10, "y": "5%" },
        { "name": "b", "width": "50%", "height": "100%", "center": true, "x": "10", "y": 0 },
        { "name": "c", "width": " 25.5% ", "height": 300 },
    ] } }),
    );
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
    rejected(&json!({ "floating": { "default_position": "left" } }));
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
        "enabled": true, "hidpi": false,
        "focused": { "width": 4, "color": "#89b4fa" },
        "unfocused": false,
        "monocle": { "width": 2, "glow": "rgba(137, 180, 250, 0.2)" },
        "floating": { "width": 3, "gradient": { "from": "#f00", "to": "00ff00aa" } },
        "grid": { "width": 1, "gradient": { "from": "#fff", "to": "#000", "angle": 180.5 },
                  "animation": { "duration": 350 } },
        "split_vertical": false,
        "ignore": [{ "app_name": "Preview" }],
    } }));
    let b = &config.borders;
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
    assert_eq!(out["borders"]["split_vertical"], json!(false));
    assert!(out["borders"].get("dwindle").is_none());
    assert_eq!(serde_json::from_value::<TilingConfig>(out).unwrap(), config);
}

/// TB-08-04: `transition`, `animation.style` and `order` parse, default and
/// round-trip; omitted they leave the legacy meaning.
#[test]
fn border_extension_fields_parse_and_default() {
    let legacy = accepted(&json!({ "borders": {
        "transition": null,
        "focused": { "width": 4, "gradient": { "from": "#fff", "to": "#000" },
            "animation": { "duration": 800, "easing": "ease_in_out" } },
    }}));
    assert_eq!(legacy.borders.transition, None);
    assert_eq!(legacy.borders.order, None);
    let BorderStateConfig::GradientColor { animation: Some(animation), .. } =
        &legacy.borders.focused
    else {
        panic!("gradient with animation");
    };
    assert_eq!(animation.style, None);
    assert_eq!(animation.effective_style(), BorderAnimationStyle::PingPong);
    let out = serde_json::to_value(&legacy).unwrap();
    assert!(out["borders"].get("transition").is_none());
    assert!(out["borders"].get("order").is_none());
    assert!(out["borders"]["focused"]["animation"].get("style").is_none());

    let config = accepted(&json!({ "borders": {
        "transition": { "duration": 200 },
        "order": "above",
        "focused": { "width": 4, "gradient": { "from": "#fff", "to": "#000", "angle": 45 },
            "animation": { "style": "loop", "duration": 4000, "easing": "linear" } },
        "unfocused": { "width": 4, "gradient": { "from": "#fff", "to": "#000" },
            "animation": { "style": "once", "duration": 1000 } },
        "monocle": { "width": 4, "gradient": { "from": "#fff", "to": "#000" },
            "animation": { "style": "ping_pong", "duration": 500 } },
    }}));
    let b = &config.borders;
    assert_eq!(
        b.transition,
        Some(BorderTransitionConfig {
            duration: 200,
            easing: EasingType::EaseOut
        })
    );
    assert_eq!(b.order, Some(BorderOrder::Above));
    for (state, style) in [
        (&b.focused, BorderAnimationStyle::Loop),
        (&b.unfocused, BorderAnimationStyle::Once),
        (&b.monocle, BorderAnimationStyle::PingPong),
    ] {
        let BorderStateConfig::GradientColor { animation: Some(animation), .. } = state else {
            panic!("gradient with animation");
        };
        assert_eq!(animation.style, Some(style));
    }
    let out = serde_json::to_value(&config).unwrap();
    assert_eq!(out["borders"]["order"], json!("above"));
    assert_eq!(
        out["borders"]["transition"],
        json!({ "duration": 200, "easing": "ease_out" })
    );
    assert_eq!(out["borders"]["focused"]["animation"]["style"], json!("loop"));
    assert_eq!(serde_json::from_value::<TilingConfig>(out).unwrap(), config);
    accepted(&json!({ "borders": { "order": "below", "transition": { "duration": 0 } } }));
}

#[test]
fn malformed_border_extension_fields_are_rejected_with_their_path() {
    let error = rejected(&json!({ "borders": { "unfocused": { "width": 4,
        "gradient": { "from": "#fff", "to": "#000" },
        "animation": { "style": "pingPong", "duration": 400 } } } }));
    assert_eq!(
        error,
        "borders.unfocused.animation.style: unknown style `pingPong`; expected `ping_pong`, \
         `once` or `loop`"
    );
    let error = rejected(&json!({ "borders": { "monocle": { "width": 4,
        "gradient": { "from": "#fff", "to": "#000" },
        "animation": { "style": "spin", "duration": 400 } } } }));
    assert!(
        error.starts_with("borders.monocle.animation.style: unknown style `spin`"),
        "{error}"
    );
    let error = rejected(&json!({ "borders": { "order": "top" } }));
    assert!(
        error.contains("unknown variant `top`") && error.contains("expected `below` or `above`"),
        "{error}"
    );
    let error =
        rejected(&json!({ "borders": { "transition": { "duration": 200, "easing": "bouncy" } } }));
    assert!(error.contains("unknown variant `bouncy`"), "{error}");
    rejected(&json!({ "borders": { "transition": { "easing": "linear" } } }));
    rejected(&json!({ "borders": { "transition": { "duration": -1 } } }));
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
    accepted(&json!({ "borders": { "focused": { "width": 4, "color": "rgb(1, 2, 3)" } } }));
    accepted(&json!({ "borders": { "focused": { "width": 4, "color": "89b4fa" } } }));
}

#[test]
fn unsupported_value_types_are_rejected() {
    rejected(&json!({ "enabled": "yes" }));
    rejected(&json!({ "animations": { "duration": -1 } }));
    rejected(&json!({ "workspaces": {} }));
    rejected(&json!({ "ignore": [{ "app_id": 3 }] }));
}

#[test]
fn schema_template_and_default_agree() {
    let catalog = catalog();
    let template = catalog.template(TilingConfig::ID).expect("template");
    assert!(template.contains("\nenabled = false\n"), "{template}");
    assert!(template.contains("default_layout"), "{template}");
    assert_eq!(
        catalog.default_value(TilingConfig::ID).unwrap(),
        &serde_json::to_value(TilingConfig::default()).unwrap()
    );
    let schema = serde_json::to_string(&catalog.schema(TilingConfig::ID).expect("schema")).unwrap();
    assert!(schema.contains("preset_on_open"), "{schema}");
}

#[test]
fn catalog_reports_validation_and_unknown_keys() {
    let catalog = catalog();
    let path = Path::new("tiling.toml");
    let warnings = catalog
        .inspect(TilingConfig::ID, path, "enabled = true\nextra = 1\n")
        .expect("valid");
    assert_eq!(warnings, ["extra"]);
    let diagnostic = catalog
        .inspect(TilingConfig::ID, path, "master = { ratio = 500 }\n")
        .expect_err("invalid");
    assert_eq!(diagnostic.kind, DiagnosticKind::Validation);
    assert!(diagnostic.message.contains("master.ratio"), "{diagnostic}");
    let diagnostic = catalog
        .inspect(TilingConfig::ID, path, "default_layout = \"tall\"\n")
        .expect_err("invalid");
    assert_ne!(diagnostic.kind, DiagnosticKind::Validation);
}

#[test]
fn a_removed_border_style_key_still_loads_as_one_unknown_key_warning() {
    // `borders.style` was removed: borders are always round. Old files load.
    let catalog = catalog();
    let path = Path::new("tiling.toml");
    let round = accepted(&json!({ "borders": { "enabled": true } }));
    for style in ["round", "square"] {
        let text = format!("[borders]\nenabled = true\nstyle = \"{style}\"\n");
        let warnings = catalog.inspect(TilingConfig::ID, path, &text).expect("valid");
        assert_eq!(warnings, ["borders.style"], "{style}");
        let loaded = accepted(&json!({ "borders": { "enabled": true, "style": style } }));
        assert_eq!(loaded, round, "{style} loads as the always-round default");
    }
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
    assert!(notes.iter().any(|n| n.contains("default_position")));
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
