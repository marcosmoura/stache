//! Human and JSON rendering of Tiling query results.
//!
//! The layouts reproduce the legacy `tabled` output: rounded borders, one space
//! of padding, header cells aligned like their column, no rules between data
//! rows and widths measured in terminal columns (`unicode-width`, like
//! `tabled`). ANSI escapes are emitted only when `ansi` is true; with `ansi`
//! false the text is byte-identical to the ANSI-stripped styled text.
//!
//! Strings from the desktop (titles, names) are made printable first: control
//! characters become visible escapes (`\n`, `\u001b`), then the text is
//! truncated by Unicode scalar count (`max - 1` plus `…`), so no window title
//! can write a raw escape sequence or break a row.

use std::fmt::Write as _;

use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use crate::dto::{AppDto, FrameDto, ScreenDto, StatusDto, WindowDto, WorkspaceDto};

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

/// How a cell is painted when ANSI is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paint {
    Plain,
    Green,
    Red,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    Left,
    Right,
    Center,
}

struct Cell {
    text: String,
    paint: Paint,
}

impl Cell {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            paint: Paint::Plain,
        }
    }

    /// A bounded, printable string cell.
    fn text(text: &str, max: Option<usize>) -> Self {
        let printable = printable(text);
        match max {
            Some(max) => Self::plain(truncate(&printable, max)),
            None => Self::plain(printable),
        }
    }

    fn boolean(value: bool) -> Self {
        if value {
            Self {
                text: "✓".to_owned(),
                paint: Paint::Green,
            }
        } else {
            Self {
                text: "✗".to_owned(),
                paint: Paint::Red,
            }
        }
    }
}

/// Escapes control characters so they print as visible text.
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// Truncates to `max` Unicode scalars: `max - 1` of them and `…` when longer.
pub(super) fn truncate(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_owned()
    } else if max <= 1 {
        "…".to_owned()
    } else {
        let mut out: String = text.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn paint(text: &str, color: Paint, ansi: bool) -> String {
    match (ansi, color) {
        (true, Paint::Green) => format!("{GREEN}{text}{RESET}"),
        (true, Paint::Red) => format!("{RED}{text}{RESET}"),
        _ => text.to_owned(),
    }
}

fn aligned(cell: &Cell, width: usize, align: Align, ansi: bool) -> String {
    let gap = width.saturating_sub(cell.text.width());
    let (left, right) = match align {
        Align::Left => (0, gap),
        Align::Right => (gap, 0),
        Align::Center => (gap / 2, gap - gap / 2),
    };
    format!(
        "{}{}{}",
        " ".repeat(left),
        paint(&cell.text, cell.paint, ansi),
        " ".repeat(right)
    )
}

fn rule(widths: &[usize], left: &str, mid: &str, right: &str) -> String {
    let parts: Vec<String> = widths.iter().map(|width| "─".repeat(width + 2)).collect();
    format!("{left}{}{right}", parts.join(mid))
}

fn row_line(cells: &[String]) -> String {
    let parts: Vec<String> = cells.iter().map(|cell| format!(" {cell} ")).collect();
    format!("│{}│", parts.join("│"))
}

/// The rounded table, without a trailing newline.
fn table(headers: &[(&str, Align)], rows: &[Vec<Cell>], ansi: bool) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(index, (header, _))| {
            rows.iter().map(|row| row[index].text.width()).fold(header.width(), usize::max)
        })
        .collect();
    let mut lines = vec![rule(&widths, "╭", "┬", "╮")];
    let header_cells: Vec<String> = headers
        .iter()
        .zip(&widths)
        .map(|((header, align), width)| aligned(&Cell::plain(*header), *width, *align, ansi))
        .collect();
    lines.push(row_line(&header_cells));
    lines.push(rule(&widths, "├", "┼", "┤"));
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .zip(headers)
            .zip(&widths)
            .map(|((cell, (_, align)), width)| aligned(cell, *width, *align, ansi))
            .collect();
        lines.push(row_line(&cells));
    }
    lines.push(rule(&widths, "╰", "┴", "╯"));
    lines.join("\n")
}

fn heading(text: &str, ansi: bool) -> String {
    if ansi {
        format!("{BOLD}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

fn dimmed(text: &str, ansi: bool) -> String {
    if ansi {
        format!("{DIM}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

/// `WxH @ x, y` with the legacy integer truncation.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn frame_text(frame: &FrameDto) -> String {
    format!(
        "{}x{} @ {}, {}",
        frame.width as u32, frame.height as u32, frame.x as i32, frame.y as i32
    )
}

const NO_TITLE: &str = "(no title)";

/// The `screens` table output (heading, table, newline) or the empty body.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
pub fn screens(rows: &[ScreenDto], ansi: bool) -> String {
    if rows.is_empty() {
        return format!("{}\n", dimmed("No screens detected.", ansi));
    }
    let cells: Vec<Vec<Cell>> = rows
        .iter()
        .map(|screen| {
            vec![
                Cell::plain(screen.id.to_string()),
                Cell::text(&screen.name, None),
                Cell::plain(format!(
                    "{}x{}",
                    screen.frame.width as u32, screen.frame.height as u32
                )),
                Cell::plain(format!("{}, {}", screen.frame.x as i32, screen.frame.y as i32)),
                Cell::plain(format!("{}x", screen.scale_factor)),
                Cell::boolean(screen.is_main),
                Cell::boolean(screen.is_builtin),
            ]
        })
        .collect();
    let headers = [
        ("ID", Align::Right),
        ("Name", Align::Left),
        ("Resolution", Align::Right),
        ("Position", Align::Right),
        ("Scale", Align::Right),
        ("Main", Align::Center),
        ("Built-in", Align::Center),
    ];
    format!(
        "{}\n{}\n",
        heading(&format!("Screens ({})", rows.len()), ansi),
        table(&headers, &cells, ansi)
    )
}

/// The `workspaces` table output or the empty body.
#[must_use]
pub fn workspaces(rows: &[WorkspaceDto], ansi: bool) -> String {
    if rows.is_empty() {
        return format!("{}\n", dimmed("No workspaces found.", ansi));
    }
    let cells: Vec<Vec<Cell>> = rows
        .iter()
        .map(|workspace| {
            vec![
                Cell::text(&workspace.name, None),
                Cell::text(&workspace.screen_name, Some(15)),
                Cell::text(&workspace.layout, None),
                Cell::plain(workspace.window_count.to_string()),
                Cell::boolean(workspace.is_visible),
                Cell::boolean(workspace.is_focused),
            ]
        })
        .collect();
    let headers = [
        ("Name", Align::Left),
        ("Screen", Align::Left),
        ("Layout", Align::Left),
        ("Windows", Align::Right),
        ("Visible", Align::Center),
        ("Focused", Align::Center),
    ];
    format!(
        "{}\n{}\n",
        heading(&format!("Workspaces ({})", rows.len()), ansi),
        table(&headers, &cells, ansi)
    )
}

fn title_cell(title: &str, max: usize) -> Cell {
    Cell::text(if title.is_empty() { NO_TITLE } else { title }, Some(max))
}

/// The `windows` table output (compact or detailed) or the empty body.
#[must_use]
pub fn windows(rows: &[WindowDto], detailed: bool, ansi: bool) -> String {
    if rows.is_empty() {
        return format!("{}\n", dimmed("No windows found.", ansi));
    }
    let head = heading(&format!("Windows ({})", rows.len()), ansi);
    let body = if detailed {
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .map(|window| {
                vec![
                    Cell::plain(window.id.to_string()),
                    Cell::plain(window.pid.to_string()),
                    Cell::text(&window.app_name, Some(15)),
                    Cell::text(&window.app_id, Some(25)),
                    title_cell(&window.title, 25),
                    Cell::text(&window.workspace, None),
                    Cell::plain(frame_text(&window.frame)),
                    Cell::boolean(window.is_minimized),
                    Cell::boolean(window.is_floating),
                    Cell::boolean(window.is_focused),
                ]
            })
            .collect();
        let headers = [
            ("ID", Align::Right),
            ("PID", Align::Right),
            ("App", Align::Left),
            ("Bundle ID", Align::Left),
            ("Title", Align::Left),
            ("Workspace", Align::Left),
            ("Frame", Align::Left),
            ("Min", Align::Center),
            ("Float", Align::Center),
            ("Focus", Align::Center),
        ];
        table(&headers, &cells, ansi)
    } else {
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .map(|window| {
                vec![
                    Cell::plain(window.id.to_string()),
                    Cell::text(&window.app_name, Some(20)),
                    title_cell(&window.title, 35),
                    Cell::text(&window.workspace, None),
                    Cell::plain(frame_text(&window.frame)),
                    Cell::boolean(window.is_focused),
                ]
            })
            .collect();
        let headers = [
            ("ID", Align::Right),
            ("App", Align::Left),
            ("Title", Align::Left),
            ("Workspace", Align::Left),
            ("Frame", Align::Right),
            ("Focused", Align::Center),
        ];
        table(&headers, &cells, ansi)
    };
    format!("{head}\n{body}\n")
}

/// The `apps` table output (same columns detailed or not) or the empty body.
#[must_use]
pub fn apps(rows: &[AppDto], ansi: bool) -> String {
    if rows.is_empty() {
        return format!("{}\n", dimmed("No running apps found.", ansi));
    }
    let cells: Vec<Vec<Cell>> = rows
        .iter()
        .map(|app| {
            vec![
                Cell::plain(app.pid.to_string()),
                Cell::text(&app.name, Some(25)),
                Cell::text(&app.bundle_id, Some(35)),
                Cell::boolean(!app.is_hidden),
            ]
        })
        .collect();
    let headers = [
        ("PID", Align::Right),
        ("Name", Align::Left),
        ("Bundle ID", Align::Left),
        ("Visible", Align::Center),
    ];
    format!(
        "{}\n{}\n",
        heading(&format!("Running Apps ({})", rows.len()), ansi),
        table(&headers, &cells, ansi)
    )
}

/// The `status` summary: phase and faults first, then every reason nothing
/// happens, one line each.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one linear table of status lines")]
pub fn status(status: &StatusDto, ansi: bool) -> String {
    use std::fmt::Write as _;
    let mut out = format!("{}\n", heading("Tiling status", ansi));
    let _ = writeln!(
        out,
        "phase: {}   healthy: {}   layout active: {}",
        status.phase, status.healthy, status.layout_active
    );
    let _ = writeln!(
        out,
        "config revision: {}   plan: {}   apps: {} ({} tracked)   windows: {}",
        status.config_revision,
        status.plan_sequence,
        status.apps,
        status.tracked_apps,
        status.windows
    );
    if status.suspended_windows > 0 {
        let _ = writeln!(
            out,
            "suspended: {} inactive tab windows",
            status.suspended_windows
        );
    }
    let _ = writeln!(
        out,
        "waiting: {} unconfirmed, {} enumerating",
        status.unconfirmed_apps, status.inflight_apps
    );
    for fault in &status.faults {
        let _ = writeln!(out, "fault: {}", printable(fault));
    }
    for (label, apps) in [
        ("failed", &status.failed_apps),
        ("truncated", &status.truncated_apps),
    ] {
        for app in apps {
            let _ = writeln!(
                out,
                "{label} app: {} ({}) pid {}: {} [retries {}]",
                printable(&app.name),
                printable(&app.bundle_id),
                app.pid,
                printable(&app.reason),
                app.retries
            );
        }
    }
    for entry in &status.no_frame {
        let _ = writeln!(
            out,
            "no frames on {}: {}",
            printable(&entry.screen),
            printable(&entry.reason)
        );
    }
    for hidden in &status.hidden_apps {
        let _ = writeln!(
            out,
            "hidden by Stache: {} ({}) {}{}",
            printable(&hidden.name),
            printable(&hidden.bundle_id),
            hidden.ownership,
            hidden
                .reason
                .as_ref()
                .map_or_else(String::new, |reason| format!(": {}", printable(reason)))
        );
    }
    for window in &status.tracked_windows {
        let _ = writeln!(
            out,
            "window {} {} ({}) \"{}\" subrole {} frame {} workspace {}{}{}",
            window.id.map_or_else(|| "-".to_owned(), |id| id.to_string()),
            printable(&window.app_name),
            printable(&window.bundle_id),
            printable(&window.title),
            window.subrole.as_deref().map_or_else(|| "-".to_owned(), printable),
            window.frame.as_deref().unwrap_or("-"),
            window.workspace.as_deref().map_or_else(|| "-".to_owned(), printable),
            if window.floating { " floating" } else { "" },
            if window.focused { " focused" } else { "" },
        );
    }
    for window in &status.excluded_windows {
        let _ = writeln!(
            out,
            "excluded {} ({}): {}",
            printable(&window.app_name),
            printable(&window.bundle_id),
            printable(&window.reason)
        );
    }
    if let Some(intent) = &status.last_intent {
        let _ = writeln!(out, "last intent: {}", printable(intent));
    }
    let _ = writeln!(
        out,
        "frame attempts: {}   enumeration failures: {}",
        status.frame_attempts, status.enumeration_failures
    );
    out
}

/// The whole result array, pretty-printed in the frozen DTO field order, with
/// a trailing newline. The highlighter is the injected terminal policy.
///
/// # Errors
///
/// The serialization error text when the rows cannot be encoded.
pub fn json<T: Serialize + ?Sized>(
    rows: &T,
    ansi: bool,
    highlight: &dyn Fn(&str, bool) -> String,
) -> Result<String, String> {
    let text = serde_json::to_string_pretty(rows).map_err(|error| error.to_string())?;
    Ok(format!("{}\n", highlight(&text, ansi)))
}

/// A query failure in table mode: the offline line or `Error: {body}`.
#[must_use]
pub fn table_error(body: &str, offline: bool, ansi: bool) -> String {
    if offline {
        format!("{}\n", paint("Stache app is not running.", Paint::Red, ansi))
    } else {
        format!("{} {}\n", paint("Error:", Paint::Red, ansi), printable(body))
    }
}

/// A query failure in JSON mode: `{"error":"..."}` with serde escaping.
#[must_use]
pub fn json_error(body: &str) -> String { format!("{}\n", serde_json::json!({ "error": body })) }
