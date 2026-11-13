//! Renderer goldens: table glyphs, padding, alignment, truncation, empty
//! bodies, ANSI-stripped equality and the frozen JSON key order.

use super::render;
use crate::dto::{AppDto, FrameDto, ScreenDto, WindowDto, WorkspaceDto};

fn frame(x: f64, y: f64, width: f64, height: f64) -> FrameDto { FrameDto { x, y, width, height } }

fn screens() -> Vec<ScreenDto> {
    vec![
        ScreenDto {
            id: 1,
            name: "Built-in Retina Display".to_owned(),
            is_main: true,
            is_builtin: true,
            scale_factor: 2.0,
            frame: frame(0.0, 0.0, 1512.0, 982.0),
            visible_frame: frame(0.0, 38.0, 1512.0, 944.0),
        },
        ScreenDto {
            id: 23,
            name: "日本語 Monitor".to_owned(),
            is_main: false,
            is_builtin: false,
            scale_factor: 1.5,
            frame: frame(-1920.9, 25.7, 1920.4, 1080.99),
            visible_frame: frame(-1920.9, 25.7, 1920.4, 1055.0),
        },
    ]
}

fn workspaces() -> Vec<WorkspaceDto> {
    vec![
        WorkspaceDto {
            id: "1".to_owned(),
            name: "coding".to_owned(),
            screen_name: "Built-in Retina Display".to_owned(),
            layout: "split_vertical".to_owned(),
            is_visible: true,
            is_focused: true,
            window_count: 12,
        },
        WorkspaceDto {
            id: "2".to_owned(),
            name: "web".to_owned(),
            screen_name: "Ext".to_owned(),
            layout: "dwindle".to_owned(),
            is_visible: false,
            is_focused: false,
            window_count: 0,
        },
    ]
}

fn windows() -> Vec<WindowDto> {
    vec![
        WindowDto {
            id: 101,
            pid: 4242,
            app_id: "com.example.VeryLongBundleIdentifier.Editor".to_owned(),
            app_name: "A Very Long Application Name Here".to_owned(),
            title: "main.rs — a title that is definitely longer than thirty-five".to_owned(),
            workspace: "coding".to_owned(),
            frame: frame(10.9, -5.2, 800.7, 600.2),
            is_minimized: false,
            is_fullscreen: false,
            is_floating: true,
            is_focused: true,
        },
        WindowDto {
            id: 7,
            pid: 9,
            app_id: "com.b".to_owned(),
            app_name: "B".to_owned(),
            title: String::new(),
            workspace: "web".to_owned(),
            frame: frame(0.0, 0.0, 1.0, 2.0),
            is_minimized: true,
            is_fullscreen: false,
            is_floating: false,
            is_focused: false,
        },
        WindowDto {
            id: 8,
            pid: 10,
            app_id: "com.c".to_owned(),
            app_name: "C".to_owned(),
            title: "Line\nbreak \u{1b}[31mred\u{1b}[0m \"q\" 🙂".to_owned(),
            workspace: "web".to_owned(),
            frame: frame(0.0, 0.0, 1.0, 2.0),
            is_minimized: false,
            is_fullscreen: true,
            is_floating: false,
            is_focused: false,
        },
    ]
}

fn apps() -> Vec<AppDto> {
    vec![
        AppDto {
            pid: 1,
            name: "Finder".to_owned(),
            bundle_id: "com.apple.finder".to_owned(),
            is_hidden: false,
        },
        AppDto {
            pid: 12345,
            name: "An application with a really long name".to_owned(),
            bundle_id: "com.example.an.extremely.long.bundle.identifier.value".to_owned(),
            is_hidden: true,
        },
    ]
}

/// Removes `ESC [ ... m` sequences.
fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn identity(json: &str, _ansi: bool) -> String { json.to_owned() }

const SCREENS: &str = "\
Screens (2)
╭────┬─────────────────────────┬────────────┬───────────┬───────┬──────┬──────────╮
│ ID │ Name                    │ Resolution │  Position │ Scale │ Main │ Built-in │
├────┼─────────────────────────┼────────────┼───────────┼───────┼──────┼──────────┤
│  1 │ Built-in Retina Display │   1512x982 │      0, 0 │    2x │  ✓   │    ✓     │
│ 23 │ 日本語 Monitor          │  1920x1080 │ -1920, 25 │  1.5x │  ✗   │    ✗     │
╰────┴─────────────────────────┴────────────┴───────────┴───────┴──────┴──────────╯
";

const WORKSPACES: &str = "\
Workspaces (2)
╭────────┬─────────────────┬────────────────┬─────────┬─────────┬─────────╮
│ Name   │ Screen          │ Layout         │ Windows │ Visible │ Focused │
├────────┼─────────────────┼────────────────┼─────────┼─────────┼─────────┤
│ coding │ Built-in Retin… │ split_vertical │      12 │    ✓    │    ✓    │
│ web    │ Ext             │ dwindle        │       0 │    ✗    │    ✗    │
╰────────┴─────────────────┴────────────────┴─────────┴─────────┴─────────╯
";

const WINDOWS: &str = "\
Windows (3)
╭─────┬──────────────────────┬─────────────────────────────────────┬───────────┬──────────────────┬─────────╮
│  ID │ App                  │ Title                               │ Workspace │            Frame │ Focused │
├─────┼──────────────────────┼─────────────────────────────────────┼───────────┼──────────────────┼─────────┤
│ 101 │ A Very Long Applica… │ main.rs — a title that is definite… │ coding    │ 800x600 @ 10, -5 │    ✓    │
│   7 │ B                    │ (no title)                          │ web       │       1x2 @ 0, 0 │    ✗    │
│   8 │ C                    │ Line\\nbreak \\u001b[31mred\\u001b[0m… │ web       │       1x2 @ 0, 0 │    ✗    │
╰─────┴──────────────────────┴─────────────────────────────────────┴───────────┴──────────────────┴─────────╯
";

const WINDOWS_DETAILED: &str = "\
Windows (3)
╭─────┬──────┬─────────────────┬───────────────────────────┬───────────────────────────┬───────────┬──────────────────┬─────┬───────┬───────╮
│  ID │  PID │ App             │ Bundle ID                 │ Title                     │ Workspace │ Frame            │ Min │ Float │ Focus │
├─────┼──────┼─────────────────┼───────────────────────────┼───────────────────────────┼───────────┼──────────────────┼─────┼───────┼───────┤
│ 101 │ 4242 │ A Very Long Ap… │ com.example.VeryLongBund… │ main.rs — a title that i… │ coding    │ 800x600 @ 10, -5 │  ✗  │   ✓   │   ✓   │
│   7 │    9 │ B               │ com.b                     │ (no title)                │ web       │ 1x2 @ 0, 0       │  ✓  │   ✗   │   ✗   │
│   8 │   10 │ C               │ com.c                     │ Line\\nbreak \\u001b[31mre… │ web       │ 1x2 @ 0, 0       │  ✗  │   ✗   │   ✗   │
╰─────┴──────┴─────────────────┴───────────────────────────┴───────────────────────────┴───────────┴──────────────────┴─────┴───────┴───────╯
";

const APPS: &str = "\
Running Apps (2)
╭───────┬───────────────────────────┬─────────────────────────────────────┬─────────╮
│   PID │ Name                      │ Bundle ID                           │ Visible │
├───────┼───────────────────────────┼─────────────────────────────────────┼─────────┤
│     1 │ Finder                    │ com.apple.finder                    │    ✓    │
│ 12345 │ An application with a re… │ com.example.an.extremely.long.bund… │    ✗    │
╰───────┴───────────────────────────┴─────────────────────────────────────┴─────────╯
";

#[test]
fn every_table_matches_its_golden() {
    assert_eq!(render::screens(&screens(), false), SCREENS);
    assert_eq!(render::workspaces(&workspaces(), false), WORKSPACES);
    assert_eq!(render::windows(&windows(), false, false), WINDOWS);
    assert_eq!(render::windows(&windows(), true, false), WINDOWS_DETAILED);
    assert_eq!(render::apps(&apps(), false), APPS);
}

#[test]
fn ansi_output_strips_to_the_plain_golden_and_plain_output_has_no_escapes() {
    let styled = [
        (render::screens(&screens(), true), SCREENS),
        (render::workspaces(&workspaces(), true), WORKSPACES),
        (render::windows(&windows(), false, true), WINDOWS),
        (render::windows(&windows(), true, true), WINDOWS_DETAILED),
        (render::apps(&apps(), true), APPS),
    ];
    for (text, plain) in styled {
        assert!(text.contains('\u{1b}'), "{text}");
        assert_eq!(strip(&text), plain);
    }
    for plain in [SCREENS, WORKSPACES, WINDOWS, WINDOWS_DETAILED, APPS] {
        assert!(!plain.contains('\u{1b}'));
    }
}

#[test]
fn ansi_paints_headings_and_glyphs_only() {
    let text = render::apps(&apps(), true);
    assert!(
        text.starts_with("\u{1b}[1mRunning Apps (2)\u{1b}[0m\n"),
        "{text}"
    );
    assert!(text.contains("\u{1b}[32m✓\u{1b}[0m"));
    assert!(text.contains("\u{1b}[31m✗\u{1b}[0m"));
    // padding stays outside the escape sequences
    assert!(text.contains("│    \u{1b}[32m✓\u{1b}[0m    │"), "{text}");
}

#[test]
fn empty_bodies_are_exact_and_dimmed_only_with_ansi() {
    assert_eq!(render::screens(&[], false), "No screens detected.\n");
    assert_eq!(render::workspaces(&[], false), "No workspaces found.\n");
    assert_eq!(render::windows(&[], false, false), "No windows found.\n");
    assert_eq!(render::windows(&[], true, false), "No windows found.\n");
    assert_eq!(render::apps(&[], false), "No running apps found.\n");
    assert_eq!(
        render::apps(&[], true),
        "\u{1b}[2mNo running apps found.\u{1b}[0m\n"
    );
    assert_eq!(strip(&render::screens(&[], true)), "No screens detected.\n");
}

#[test]
fn truncation_counts_unicode_scalars_and_keeps_the_ellipsis() {
    assert_eq!(render::truncate("hello", 10), "hello");
    assert_eq!(render::truncate("hello", 5), "hello");
    assert_eq!(render::truncate("hello world", 8), "hello w…");
    assert_eq!(render::truncate("hello", 1), "…");
    assert_eq!(render::truncate("hello", 0), "…");
    assert_eq!(render::truncate("日本語日本語", 4), "日本語…");
    // a combining sequence counts per scalar, never splitting a scalar
    assert_eq!(render::truncate("e\u{301}e\u{301}e\u{301}", 4), "e\u{301}e…");
}

#[test]
fn json_is_the_whole_pretty_array_in_the_frozen_field_order() {
    let rows = &screens()[..1];
    let text = render::json(rows, false, &identity).expect("json");
    assert_eq!(
        text,
        "[\n  {\n    \"id\": 1,\n    \"name\": \"Built-in Retina Display\",\n    \"isMain\": true,\n    \
         \"isBuiltin\": true,\n    \"scaleFactor\": 2.0,\n    \"frame\": {\n      \"x\": 0.0,\n      \
         \"y\": 0.0,\n      \"width\": 1512.0,\n      \"height\": 982.0\n    },\n    \
         \"visibleFrame\": {\n      \"x\": 0.0,\n      \"y\": 38.0,\n      \"width\": 1512.0,\n      \
         \"height\": 944.0\n    }\n  }\n]\n"
    );
    assert_eq!(
        render::json::<[AppDto]>(&[], false, &identity).expect("json"),
        "[]\n"
    );
    let window = render::json(&windows()[..1], false, &identity).expect("json");
    let keys: Vec<&str> = window
        .lines()
        .filter(|line| line.starts_with("    \""))
        .map(|line| line.trim().split('"').nth(1).expect("key"))
        .collect();
    assert_eq!(keys, [
        "id",
        "pid",
        "appId",
        "appName",
        "title",
        "workspace",
        "frame",
        "isMinimized",
        "isFullscreen",
        "isFloating",
        "isFocused"
    ]);
    let app = render::json(&apps()[..1], false, &identity).expect("json");
    assert!(app.find("\"pid\"") < app.find("\"name\""));
    assert!(app.find("\"bundleId\"") < app.find("\"isHidden\""));
    let workspace = render::json(&workspaces()[..1], false, &identity).expect("json");
    let order = [
        "id",
        "name",
        "screenName",
        "layout",
        "isVisible",
        "isFocused",
        "windowCount",
    ];
    let positions: Vec<usize> = order
        .iter()
        .map(|key| workspace.find(&format!("\"{key}\"")).expect("key"))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{workspace}");
}

#[test]
fn json_escapes_titles_and_applies_the_injected_highlighter() {
    let text = render::json(&windows()[2..], false, &identity).expect("json");
    assert!(
        text.contains(r#""title": "Line\nbreak \u001b[31mred\u001b[0m \"q\" 🙂""#),
        "{text}"
    );
    let styled = render::json(&apps()[..0], true, &|json, ansi| {
        assert!(ansi);
        format!("<{json}>")
    })
    .expect("json");
    assert_eq!(styled, "<[]>\n");
}

#[test]
fn query_failures_use_the_legacy_stdout_bodies() {
    assert_eq!(
        render::table_error("", true, false),
        "Stache app is not running.\n"
    );
    assert_eq!(
        render::table_error("", true, true),
        "\u{1b}[31mStache app is not running.\u{1b}[0m\n"
    );
    assert_eq!(
        render::table_error("Tiling is disabled", false, false),
        "Error: Tiling is disabled\n"
    );
    assert_eq!(
        render::table_error("boom", false, true),
        "\u{1b}[31mError:\u{1b}[0m boom\n"
    );
    assert_eq!(
        render::table_error("a\u{1b}[2Jb", false, true),
        "\u{1b}[31mError:\u{1b}[0m a\\u001b[2Jb\n"
    );
    assert_eq!(
        render::json_error("Stache app is not running"),
        "{\"error\":\"Stache app is not running\"}\n"
    );
    assert_eq!(
        render::json_error("say \"hi\"\n\u{1b}"),
        "{\"error\":\"say \\\"hi\\\"\\n\\u001b\"}\n"
    );
}

#[test]
fn the_status_table_names_every_reason_nothing_happens() {
    let status = crate::dto::StatusDto {
        phase: "incomplete".to_owned(),
        healthy: true,
        faults: vec!["Accessibility permission is missing".to_owned()],
        layout_active: false,
        config_revision: 3,
        plan_sequence: 7,
        apps: 12,
        tracked_apps: 10,
        suspended_windows: 0,
        windows: 4,
        failed_apps: vec![crate::dto::StatusAppDto {
            pid: 9,
            name: "Zoom".to_owned(),
            bundle_id: "us.zoom.xos".to_owned(),
            reason: "Unacknowledged".to_owned(),
            retries: 2,
        }],
        truncated_apps: Vec::new(),
        unconfirmed_apps: 1,
        inflight_apps: 0,
        no_frame: vec![crate::dto::StatusNoFrameDto {
            screen: "Built-in".to_owned(),
            reason: "UnknownDisplay".to_owned(),
        }],
        hidden_apps: Vec::new(),
        animation_fallbacks: vec![
            crate::dto::StatusAnimationFallbackDto {
                name: "WhatsApp".to_owned(),
                bundle_id: "net.whatsapp.WhatsApp".to_owned(),
                kind: "resize".to_owned(),
                reason: "writes fewer than 15 frames per second in 2 of its last 3 layout changes"
                    .to_owned(),
                retry_in_ms: Some(41_200),
            },
            crate::dto::StatusAnimationFallbackDto {
                name: "Zed".to_owned(),
                bundle_id: "dev.zed.Zed".to_owned(),
                kind: "move".to_owned(),
                reason: "turns Enhanced UI back on".to_owned(),
                retry_in_ms: None,
            },
        ],
        last_intent: Some("FocusWorkspace(\"music\") -> Applied".to_owned()),
        tracked_windows: Vec::new(),
        excluded_windows: Vec::new(),
        frame_attempts: 5,
        enumeration_failures: 1,
    };
    let table = render::status(&status, false);
    for expected in [
        "phase: incomplete",
        "fault: Accessibility permission is missing",
        "failed app: Zoom (us.zoom.xos) pid 9: Unacknowledged [retries 2]",
        "no frames on Built-in: UnknownDisplay",
        "last intent: FocusWorkspace",
        "no animation for resizes: WhatsApp (net.whatsapp.WhatsApp): writes fewer than 15 frames \
         per second in 2 of its last 3 layout changes; animates again after 42s",
        "no animation for moves: Zed (dev.zed.Zed): turns Enhanced UI back on; for this run",
    ] {
        assert!(table.contains(expected), "{expected}\n{table}");
    }
}
