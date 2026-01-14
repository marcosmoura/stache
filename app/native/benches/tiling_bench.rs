//! Benchmarks for tiling window manager performance-critical operations.
//!
//! Run with: `cargo bench -p stache`
//!
//! Results are saved to `target/criterion/` with HTML reports.
//!
//! ## Benchmark Groups
//!
//! - `layouts`: Core layout algorithms at various window counts
//! - `layouts_4k`: Layout algorithms on 4K screens
//! - `layouts_stress`: Large window counts (32, 64 windows)
//! - `layouts_ratios`: Layout calculations with custom split ratios
//! - `gaps`: Gap configuration resolution
//! - `state`: State operations (hash, workspace, cache)
//! - `rules`: Window rule matching
//! - `geometry`: Rect operations

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use stache_lib::config::{
    GapValue, GapsConfig, GapsConfigValue, LayoutType, MasterPosition, WindowRule,
};
use stache_lib::tiling::layout::{
    Gaps, calculate_layout_with_gaps, calculate_layout_with_gaps_and_ratios,
};
use stache_lib::tiling::rules::{any_rule_matches, find_matching_workspace, matches_window};
use stache_lib::tiling::state::{
    LayoutCache, Point, Rect, TilingState, Workspace, compute_layout_hash,
};
use stache_lib::tiling::window::WindowInfo;

// ============================================================================
// Test Data
// ============================================================================

/// Creates a standard 1080p screen frame for benchmarks.
fn screen_1080p() -> Rect {
    Rect::new(0.0, 25.0, 1920.0, 1055.0) // With menu bar offset
}

/// Creates a 4K screen frame for benchmarks.
fn screen_4k() -> Rect { Rect::new(0.0, 25.0, 3840.0, 2135.0) }

/// Creates standard gap configuration for benchmarks.
fn standard_gaps() -> Gaps { Gaps::uniform(8.0, 12.0) }

/// Creates a vector of sequential window IDs.
fn window_ids(count: usize) -> Vec<u32> { (1..=count as u32).collect() }

/// Creates a test WindowInfo for rule matching benchmarks.
fn test_window(bundle_id: &str, app_name: &str, title: &str) -> WindowInfo {
    WindowInfo::new(
        1,
        1234,
        bundle_id.to_string(),
        app_name.to_string(),
        title.to_string(),
        Rect::new(0.0, 0.0, 800.0, 600.0),
        false, // is_minimized
        false, // is_hidden
        true,  // is_main
        true,  // is_focused
    )
}

/// Creates test window rules for benchmarking.
fn test_rules() -> Vec<WindowRule> {
    vec![
        WindowRule {
            app_id: Some("com.apple.finder".to_string()),
            app_name: None,
            title: None,
        },
        WindowRule {
            app_id: Some("com.apple.Safari".to_string()),
            app_name: None,
            title: Some("Settings".to_string()),
        },
        WindowRule {
            app_id: None,
            app_name: Some("Terminal".to_string()),
            title: None,
        },
        WindowRule {
            app_id: Some("com.microsoft.VSCode".to_string()),
            app_name: None,
            title: None,
        },
        WindowRule {
            app_id: Some("com.googlecode.iterm2".to_string()),
            app_name: None,
            title: None,
        },
    ]
}

// ============================================================================
// Layout Benchmarks
// ============================================================================

fn bench_layouts(c: &mut Criterion) {
    let mut group = c.benchmark_group("layouts");
    let screen = screen_1080p();
    let gaps = standard_gaps();

    // Test with varying window counts
    for count in [1, 2, 4, 8, 12, 16] {
        let windows = window_ids(count);

        group.bench_with_input(BenchmarkId::new("dwindle", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Dwindle),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("master", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps_and_ratios(
                    black_box(LayoutType::Master),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                    black_box(&[]),
                    black_box(MasterPosition::Left),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("grid", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Grid),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("split", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Split),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("monocle", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Monocle),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });
    }

    group.finish();
}

fn bench_layout_4k(c: &mut Criterion) {
    let mut group = c.benchmark_group("layouts_4k");
    let screen = screen_4k();
    let gaps = standard_gaps();

    // Only test larger window counts on 4K
    for count in [8, 16] {
        let windows = window_ids(count);

        group.bench_with_input(BenchmarkId::new("dwindle", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Dwindle),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("grid", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Grid),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });
    }

    group.finish();
}

// ============================================================================
// Gaps Resolution Benchmarks
// ============================================================================

fn bench_gaps(c: &mut Criterion) {
    let mut group = c.benchmark_group("gaps");

    // Global gaps config with uniform values
    let global_uniform = GapsConfigValue::Global(GapsConfig {
        inner: GapValue::Uniform(8),
        outer: GapValue::Uniform(12),
    });

    // Global gaps config with per-axis values
    let global_per_axis = GapsConfigValue::Global(GapsConfig {
        inner: GapValue::PerAxis { horizontal: 8, vertical: 6 },
        outer: GapValue::PerSide {
            top: 10,
            right: 8,
            bottom: 8,
            left: 8,
        },
    });

    group.bench_function("gaps_uniform", |b| {
        b.iter(|| {
            Gaps::from_config(
                black_box(&global_uniform),
                black_box("main"),
                black_box(true),
                black_box(25.0),
            )
        });
    });

    group.bench_function("gaps_per_axis", |b| {
        b.iter(|| {
            Gaps::from_config(
                black_box(&global_per_axis),
                black_box("main"),
                black_box(true),
                black_box(25.0),
            )
        });
    });

    group.finish();
}

// ============================================================================
// State Operations Benchmarks
// ============================================================================

fn bench_state_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("state");

    // Layout hash computation
    let window_ids: Vec<u32> = (1..=10).collect();
    let screen = screen_1080p();
    let gaps = standard_gaps();

    group.bench_function("compute_layout_hash", |b| {
        b.iter(|| {
            compute_layout_hash(
                black_box(LayoutType::Dwindle),
                black_box(&window_ids),
                black_box(&screen),
                black_box(0.5),
                black_box(&[]),
                black_box(gaps.compute_hash()),
            )
        });
    });

    // Workspace creation
    group.bench_function("workspace_new", |b| {
        b.iter(|| {
            Workspace::new(
                black_box("test".to_string()),
                black_box(1),
                black_box(LayoutType::Dwindle),
            )
        });
    });

    // Layout cache operations
    let mut cache = LayoutCache::new();
    let hash = compute_layout_hash(
        LayoutType::Dwindle,
        &window_ids,
        &screen,
        0.5,
        &[],
        gaps.compute_hash(),
    );
    let positions =
        calculate_layout_with_gaps(LayoutType::Dwindle, &window_ids, &screen, 0.5, &gaps);
    cache.update(hash, positions);

    group.bench_function("layout_cache_is_valid_hit", |b| {
        b.iter(|| black_box(&cache).is_valid(black_box(hash)));
    });

    group.bench_function("layout_cache_is_valid_miss", |b| {
        b.iter(|| black_box(&cache).is_valid(black_box(hash + 1)));
    });

    // Workspace lookup with index
    let mut state = TilingState::new();
    for i in 0..20 {
        state.add_workspace(Workspace::new(format!("workspace-{i}"), 1, LayoutType::Dwindle));
    }

    group.bench_function("workspace_lookup_indexed", |b| {
        b.iter(|| black_box(&state).workspace_by_name(black_box("workspace-15")));
    });

    group.finish();
}

// ============================================================================
// Stress Test Benchmarks (Large Window Counts)
// ============================================================================

fn bench_layouts_stress(c: &mut Criterion) {
    let mut group = c.benchmark_group("layouts_stress");
    let screen = screen_1080p();
    let gaps = standard_gaps();

    // Stress test with large window counts
    for count in [32, 64] {
        let windows = window_ids(count);

        group.bench_with_input(BenchmarkId::new("dwindle", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Dwindle),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("grid", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps(
                    black_box(LayoutType::Grid),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                )
            });
        });

        group.bench_with_input(BenchmarkId::new("master", count), &count, |b, _| {
            b.iter(|| {
                calculate_layout_with_gaps_and_ratios(
                    black_box(LayoutType::Master),
                    black_box(&windows),
                    black_box(&screen),
                    black_box(0.5),
                    black_box(&gaps),
                    black_box(&[]),
                    black_box(MasterPosition::Left),
                )
            });
        });
    }

    group.finish();
}

// ============================================================================
// Layout with Custom Ratios Benchmarks
// ============================================================================

fn bench_layouts_ratios(c: &mut Criterion) {
    let mut group = c.benchmark_group("layouts_ratios");
    let screen = screen_1080p();
    let gaps = standard_gaps();
    let windows = window_ids(8);

    // Custom split ratios (cumulative: 0.2, 0.35, 0.5, 0.65, 0.8, 0.9, 1.0)
    let split_ratios: Vec<f64> = vec![0.2, 0.35, 0.5, 0.65, 0.8, 0.9];

    group.bench_function("split_with_ratios", |b| {
        b.iter(|| {
            calculate_layout_with_gaps_and_ratios(
                black_box(LayoutType::Split),
                black_box(&windows),
                black_box(&screen),
                black_box(0.5),
                black_box(&gaps),
                black_box(&split_ratios),
                black_box(MasterPosition::Left),
            )
        });
    });

    group.bench_function("master_with_ratios", |b| {
        b.iter(|| {
            calculate_layout_with_gaps_and_ratios(
                black_box(LayoutType::Master),
                black_box(&windows),
                black_box(&screen),
                black_box(0.6), // 60% master
                black_box(&gaps),
                black_box(&split_ratios),
                black_box(MasterPosition::Left),
            )
        });
    });

    group.bench_function("master_position_top", |b| {
        b.iter(|| {
            calculate_layout_with_gaps_and_ratios(
                black_box(LayoutType::Master),
                black_box(&windows),
                black_box(&screen),
                black_box(0.5),
                black_box(&gaps),
                black_box(&[]),
                black_box(MasterPosition::Top),
            )
        });
    });

    group.finish();
}

// ============================================================================
// Window Rule Matching Benchmarks
// ============================================================================

fn bench_rules(c: &mut Criterion) {
    let mut group = c.benchmark_group("rules");

    let rules = test_rules();
    let matching_window = test_window("com.apple.finder", "Finder", "Documents");
    let non_matching_window = test_window("com.example.unknown", "Unknown App", "Window");

    // Single rule match (hit)
    group.bench_function("matches_window_hit", |b| {
        b.iter(|| matches_window(black_box(&rules[0]), black_box(&matching_window)));
    });

    // Single rule match (miss)
    group.bench_function("matches_window_miss", |b| {
        b.iter(|| matches_window(black_box(&rules[0]), black_box(&non_matching_window)));
    });

    // Any rule matches (early exit)
    group.bench_function("any_rule_matches_early", |b| {
        b.iter(|| any_rule_matches(black_box(&rules), black_box(&matching_window)));
    });

    // Any rule matches (full scan)
    group.bench_function("any_rule_matches_full", |b| {
        b.iter(|| any_rule_matches(black_box(&rules), black_box(&non_matching_window)));
    });

    // Find matching workspace with multiple workspaces
    let workspace_rules: Vec<(&str, &[WindowRule])> = vec![
        ("code", &rules[3..4]),     // VSCode rule
        ("terminal", &rules[2..3]), // Terminal rule
        ("browser", &rules[1..2]),  // Safari rule
        ("files", &rules[0..1]),    // Finder rule
    ];

    let finder_window = test_window("com.apple.finder", "Finder", "Documents");

    group.bench_function("find_workspace_last", |b| {
        b.iter(|| {
            find_matching_workspace(
                black_box(&finder_window),
                black_box(workspace_rules.iter().map(|(n, r)| (*n, *r))),
            )
        });
    });

    let vscode_window = test_window("com.microsoft.VSCode", "Code", "main.rs");

    group.bench_function("find_workspace_first", |b| {
        b.iter(|| {
            find_matching_workspace(
                black_box(&vscode_window),
                black_box(workspace_rules.iter().map(|(n, r)| (*n, *r))),
            )
        });
    });

    group.finish();
}

// ============================================================================
// Geometry Operations Benchmarks
// ============================================================================

fn bench_geometry(c: &mut Criterion) {
    let mut group = c.benchmark_group("geometry");

    let rect = Rect::new(100.0, 200.0, 800.0, 600.0);
    let point_inside = Point::new(500.0, 400.0);
    let point_outside = Point::new(50.0, 50.0);

    group.bench_function("rect_contains_inside", |b| {
        b.iter(|| black_box(&rect).contains(black_box(point_inside)));
    });

    group.bench_function("rect_contains_outside", |b| {
        b.iter(|| black_box(&rect).contains(black_box(point_outside)));
    });

    group.bench_function("rect_center", |b| {
        b.iter(|| black_box(&rect).center());
    });

    group.bench_function("rect_area", |b| {
        b.iter(|| black_box(&rect).area());
    });

    group.bench_function("rect_new", |b| {
        b.iter(|| {
            Rect::new(
                black_box(0.0),
                black_box(25.0),
                black_box(1920.0),
                black_box(1055.0),
            )
        });
    });

    group.finish();
}

// ============================================================================
// Criterion Configuration
// ============================================================================

criterion_group!(
    benches,
    bench_layouts,
    bench_layout_4k,
    bench_layouts_stress,
    bench_layouts_ratios,
    bench_gaps,
    bench_state_operations,
    bench_rules,
    bench_geometry,
);

criterion_main!(benches);
