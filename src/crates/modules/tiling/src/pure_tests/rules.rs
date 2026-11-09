//! Rule matching, preparation equivalence and exclusion predicates.

use crate::{
    ExclusionInput, ManagementExclusion, PIP_SUBROLE, PreparedRule, PreparedRules, WindowMetadata,
    WindowRule, find_matching_workspace, find_matching_workspace_prepared, is_pip_window,
    is_system_app_name, is_system_bundle, management_exclusion, matches_unprepared,
};

fn rule(app_id: Option<&str>, app_name: Option<&str>, title: Option<&str>) -> WindowRule {
    WindowRule {
        app_id: app_id.map(str::to_owned),
        app_name: app_name.map(str::to_owned),
        title: title.map(str::to_owned),
    }
}

fn window<'a>(app_id: &'a str, app_name: &'a str, title: &'a str) -> WindowMetadata<'a> {
    WindowMetadata { app_id, app_name, title }
}

/// Both the prepared and unprepared paths must agree; returns the shared answer.
#[track_caller]
fn matches(rule: &WindowRule, window: &WindowMetadata<'_>) -> bool {
    let unprepared = matches_unprepared(rule, window);
    assert_eq!(
        unprepared,
        PreparedRule::new(rule).matches(window),
        "{rule:?} vs {window:?}"
    );
    unprepared
}

#[test]
fn app_id_fallback_bug_is_fixed() {
    // Legacy rules/mod.rs:52-54 compared the *shadowed rule* app id with itself
    // (always true). A rule for Finder must not match another application.
    let finder = rule(Some("com.apple.finder"), None, None);
    assert!(!matches(
        &finder,
        &window("com.example.other", "Other", "Window")
    ));
    assert!(!matches(&finder, &window("", "Finder", "Window")));
    assert!(matches(&finder, &window("com.apple.finder", "x", "y")));
}

#[test]
fn app_id_is_exact_and_case_insensitive() {
    let r = rule(Some("Com.Apple.Safari"), None, None);
    assert!(matches(&r, &window("COM.APPLE.SAFARI", "", "")));
    assert!(matches(&r, &window("com.apple.safari", "", "")));
    // Exact, not a substring or prefix.
    assert!(!matches(
        &r,
        &window("com.apple.safari.technologypreview", "", "")
    ));
    assert!(!matches(&r, &window("com.apple.safar", "", "")));
}

#[test]
fn name_and_title_are_case_insensitive_substrings() {
    let r = rule(None, Some("code"), None);
    assert!(matches(&r, &window("", "Visual Studio Code", "")));
    assert!(!matches(&r, &window("", "Terminal", "")));
    let r = rule(None, None, Some("SETTINGS"));
    assert!(matches(&r, &window("", "", "Safari settings")));
    assert!(!matches(&r, &window("", "", "Preferences")));
}

#[test]
fn criteria_are_anded() {
    let r = rule(Some("com.apple.Safari"), None, Some("settings"));
    assert!(matches(
        &r,
        &window("com.apple.safari", "Safari", "Safari Settings")
    ));
    assert!(!matches(&r, &window("com.apple.safari", "Safari", "Start Page")));
    assert!(!matches(&r, &window("com.other", "Safari", "Safari Settings")));
    let all = rule(Some("a"), Some("b"), Some("c"));
    assert!(matches(&all, &window("A", "xBx", "xcx")));
    assert!(!matches(&all, &window("A", "xBx", "x")));
}

#[test]
fn rule_without_criteria_never_matches() {
    let empty = WindowRule::default();
    assert!(!PreparedRule::new(&empty).has_criteria());
    assert!(!matches(&empty, &window("", "", "")));
    assert!(!matches(&empty, &window("com.apple.finder", "Finder", "Title")));
}

#[test]
fn missing_metadata_is_the_empty_string() {
    // An absent window value fails any non-empty criterion...
    assert!(!matches(&rule(Some("x"), None, None), &window("", "n", "t")));
    assert!(!matches(&rule(None, Some("n"), None), &window("a", "", "t")));
    assert!(!matches(&rule(None, None, Some("t")), &window("a", "n", "")));
    // ...while an empty criterion is a criterion that is satisfied (an empty
    // substring is contained in everything; an empty id equals an empty id).
    assert!(matches(&rule(None, None, Some("")), &window("", "", "")));
    assert!(matches(&rule(None, Some(""), None), &window("a", "n", "t")));
    assert!(matches(&rule(Some(""), None, None), &window("", "n", "t")));
    assert!(!matches(&rule(Some(""), None, None), &window("a", "n", "t")));
}

#[test]
fn unicode_lowercasing_is_explicit() {
    // Full Unicode lowercase on both sides.
    assert!(matches(
        &rule(None, None, Some("ÜBER")),
        &window("", "", "über uns")
    ));
    assert!(matches(
        &rule(None, None, Some("über")),
        &window("", "", "ÜBER UNS")
    ));
    assert!(matches(
        &rule(None, Some("ЖУРНАЛ"), None),
        &window("", "Мой журнал", "")
    ));
    // Final sigma is part of the std lowercase mapping on both sides.
    assert!(matches(
        &rule(None, None, Some("ΟΔΥΣΣΕΥΣ")),
        &window("", "", "οδυσσευς")
    ));
    // Lowercasing is not case *folding*: sharp s does not equal "ss".
    assert!(!matches(
        &rule(None, None, Some("STRASSE")),
        &window("", "", "straße")
    ));
    assert!(matches(
        &rule(None, None, Some("STRAßE")),
        &window("", "", "straße")
    ));
    // Non-BMP text and combining sequences survive.
    assert!(matches(
        &rule(None, None, Some("📁 DOCS")),
        &window("", "", "My 📁 docs")
    ));
}

fn corpus() -> Vec<&'static str> {
    vec![
        "",
        "Finder",
        "com.apple.finder",
        "Über",
        "über",
        "ΟΔΥΣΣΕΥΣ",
        "İ",
    ]
}

#[test]
fn prepared_and_unprepared_agree_with_an_independent_reference() {
    fn reference(rule: &WindowRule, w: &WindowMetadata<'_>) -> bool {
        if rule.app_id.is_none() && rule.app_name.is_none() && rule.title.is_none() {
            return false;
        }
        let id = rule.app_id.as_ref().is_none_or(|r| r.to_lowercase() == w.app_id.to_lowercase());
        let name = rule
            .app_name
            .as_ref()
            .is_none_or(|r| w.app_name.to_lowercase().contains(&r.to_lowercase()));
        let title = rule
            .title
            .as_ref()
            .is_none_or(|r| w.title.to_lowercase().contains(&r.to_lowercase()));
        id && name && title
    }

    let values = corpus();
    let options: Vec<Option<&str>> =
        std::iter::once(None).chain(values.iter().copied().map(Some)).collect();
    let mut checked = 0_u32;
    let mut matched = 0_u32;
    for id in &options {
        for name in &options {
            for title in &options {
                let r = rule(*id, *name, *title);
                let prepared = PreparedRule::new(&r);
                for wid in &values {
                    for wname in &values {
                        for wtitle in &values {
                            let w = window(wid, wname, wtitle);
                            let expected = reference(&r, &w);
                            assert_eq!(matches_unprepared(&r, &w), expected, "{r:?} {w:?}");
                            assert_eq!(prepared.matches(&w), expected, "{r:?} {w:?}");
                            assert_eq!(prepared.matches_lowered(&w.lowered()), expected);
                            checked += 1;
                            matched += u32::from(expected);
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 100_000);
    assert!(matched > 1_000, "the corpus must exercise positive matches too");
}

#[test]
fn first_matching_workspace_and_rule_win() {
    let workspaces: Vec<Vec<WindowRule>> = vec![
        vec![rule(None, Some("nomatch"), None)],
        vec![
            rule(None, Some("nomatch"), None),
            rule(None, Some("term"), None),
        ],
        vec![rule(None, Some("terminal"), None)],
    ];
    let w = window("com.apple.Terminal", "Terminal", "zsh");
    let found = find_matching_workspace(&w, workspaces.iter().map(Vec::as_slice)).unwrap();
    assert_eq!((found.workspace_index, found.rule_index), (1, 1));

    let prepared: Vec<PreparedRules> = workspaces.iter().map(|r| PreparedRules::new(r)).collect();
    assert_eq!(
        find_matching_workspace_prepared(&w, prepared.iter()),
        Some(found)
    );
    assert!(
        find_matching_workspace(&window("", "Mail", ""), workspaces.iter().map(Vec::as_slice))
            .is_none()
    );
    assert_eq!(prepared[1].first_match(&w), Some(1));
    assert!(!prepared[0].any_match(&w));
}

#[test]
fn system_and_pip_predicates() {
    assert!(is_system_bundle("com.apple.dock"));
    assert!(is_system_bundle("COM.APPLE.DOCK"));
    assert!(!is_system_bundle("com.apple.dock.helper"));
    assert!(!is_system_bundle(""));
    assert!(is_system_app_name("dock"));
    assert!(is_system_app_name("Control Center"));
    assert!(!is_system_app_name("Dockyard"));
    assert!(!is_system_app_name(""));
    assert!(is_pip_window(Some(PIP_SUBROLE)));
    assert!(!is_pip_window(Some("AXStandardWindow")));
    assert!(!is_pip_window(Some("axfloatingwindow")));
    assert!(!is_pip_window(None));
}

#[test]
fn management_exclusions_and_their_order() {
    const SELF_ID: &str = "dev.example.stache-next";
    let ignore = PreparedRules::new(&[
        rule(None, Some("nomatch"), None),
        rule(Some("com.example.ignored"), None, None),
    ]);
    let check = |input: ExclusionInput<'_>| management_exclusion(&input, SELF_ID, &ignore);
    let input = |id, name, title, subrole| ExclusionInput {
        metadata: window(id, name, title),
        subrole,
    };

    assert_eq!(
        check(input("Dev.Example.Stache-Next", "S", "", None)),
        Some(ManagementExclusion::SelfApplication)
    );
    assert_eq!(
        check(input("com.apple.dock", "Dock", "", None)),
        Some(ManagementExclusion::SystemBundle)
    );
    // Name-only exclusion applies when the bundle id is unknown.
    assert_eq!(
        check(input("", "Dock", "", None)),
        Some(ManagementExclusion::SystemAppName)
    );
    assert_eq!(
        check(input("com.pip.app", "Player", "", Some(PIP_SUBROLE))),
        Some(ManagementExclusion::PictureInPicture)
    );
    assert_eq!(
        check(input("com.example.ignored", "Ig", "", None)),
        Some(ManagementExclusion::IgnoreRule { index: 1 })
    );
    assert_eq!(
        check(input("com.example.app", "App", "Doc", Some("AXStandardWindow"))),
        None
    );
    // System wins over a later ignore match; self wins over system.
    assert_eq!(
        management_exclusion(&input(SELF_ID, "Dock", "", None), SELF_ID, &ignore),
        Some(ManagementExclusion::SelfApplication)
    );
    // An empty own id disables the self check and does not exclude unknown windows.
    assert_eq!(
        management_exclusion(&input("", "Mail", "", None), "", &ignore),
        None
    );
}
