use std::sync::atomic::Ordering;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table};

use crate::app::{App, Mode, format_count, format_size};
use crate::procs::ScanProgress;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const BAR_WIDTH: usize = 10;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(f.area());

    draw_header(f, app, header_area);
    draw_table(f, app, table_area);
    draw_footer(f, app, footer_area);

    match &app.mode {
        Mode::Confirm { force, targets } => draw_confirm(f, app, *force, targets),
        Mode::Ending { count } => draw_ending(f, *count),
        Mode::ConfirmQuit => draw_confirm_quit(f),
        Mode::Browse => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let node = &app.tree.nodes[app.cwd];
    let mut text = format!(
        " {}  —  {}, {} processes  (RAM {} of {} in use)",
        app.tree.path_of(app.cwd),
        format_size(node.size),
        format_count(node.n_procs),
        format_size(app.tree.used_ram),
        format_size(app.tree.total_ram),
    );
    if app.tree.errors > 0 {
        text.push_str(&format!("  ({} unreadable)", format_count(app.tree.errors)));
    }
    // Keep the tail (current group + totals) visible when the path is long.
    let text = truncate_left(&text, area.width as usize);
    f.render_widget(
        Paragraph::new(text).style(
            Style::new()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        ),
        area,
    );
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let total = app.tree.nodes[app.cwd].size.max(1);
    // One line of the area is the header, so it cannot hold data rows.
    let window = app.visible_range(area.height.saturating_sub(1) as usize);

    let rows: Vec<Row> = app.rows[window.clone()]
        .iter()
        .map(|&idx| {
            let node = &app.tree.nodes[idx];
            let marked = app.marked.contains(&idx);
            let pct = node.size as f64 / total as f64 * 100.0;

            let mark_cell =
                Cell::from(if marked { "●" } else { " " }).style(Style::new().fg(Color::Yellow));
            let size_cell = Cell::from(format!("{:>10}", format_size(node.size)));
            let pct_cell = Cell::from(format!("{pct:>5.1}%"));
            let bar_cell = Cell::from(bar(pct)).style(Style::new().fg(Color::Green));
            let items_cell = Cell::from(if node.is_dir {
                format!("{:>10}", format_count(node.n_procs))
            } else {
                String::new()
            });
            let name_cell = if node.is_dir {
                Cell::from(format!("{}\\", node.name))
                    .style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            } else {
                Cell::from(node.name.as_str())
            };

            let row = Row::new(vec![
                mark_cell, size_cell, pct_cell, bar_cell, items_cell, name_cell,
            ]);
            if marked {
                row.style(Style::new().fg(Color::Yellow))
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(BAR_WIDTH as u16),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(vec!["", "    Memory", "     %", "", " Processes", "Name"]).style(
            Style::new()
                .add_modifier(Modifier::UNDERLINED)
                .fg(Color::Gray),
        ),
    )
    .row_highlight_style(Style::new().bg(Color::Blue));

    // The table only received the visible slice, so the highlight index must be
    // relative to the window, not to the full row list.
    app.table_state.select(if window.is_empty() {
        None
    } else {
        Some(app.selected.saturating_sub(window.start))
    });
    f.render_stateful_widget(table, area, &mut app.table_state);

    if app.rows.is_empty() {
        let msg = Paragraph::new("  (no processes)").style(Style::new().fg(Color::DarkGray));
        let inner = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        f.render_widget(msg, inner);
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let [info_area, keys_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let info: Line = if let Some(status) = &app.status {
        Line::from(Span::styled(
            format!(" {status}"),
            Style::new().fg(Color::Green),
        ))
    } else if !app.marked.is_empty() {
        let marked_size: u64 = app.marked.iter().map(|&i| app.tree.nodes[i].size).sum();
        Line::from(Span::styled(
            format!(
                " {} marked — {}",
                app.marked.len(),
                format_size(marked_size)
            ),
            Style::new().fg(Color::Yellow),
        ))
    } else {
        Line::default()
    };
    f.render_widget(Paragraph::new(info), info_area);

    let keys = " ↑↓ move  enter open  backspace up  space mark  c clear  d end process  D force kill  r refresh  q quit";
    f.render_widget(
        Paragraph::new(keys).style(Style::new().fg(Color::DarkGray)),
        keys_area,
    );
}

fn draw_confirm(f: &mut Frame, app: &App, force: bool, targets: &[usize]) {
    let total: u64 = targets.iter().map(|&t| app.tree.nodes[t].size).sum();
    let count: u64 = targets.iter().map(|&t| app.tree.nodes[t].n_procs).sum();
    let (title, color) = if force {
        (" Force kill ", Color::Red)
    } else {
        (" End process ", Color::Yellow)
    };

    let mut lines: Vec<Line> = Vec::new();
    let question = if force {
        format!(
            "FORCE KILL {count} process(es) ({})? Unsaved work will be lost.",
            format_size(total)
        )
    } else {
        format!("Ask {count} process(es) ({}) to close?", format_size(total))
    };
    lines.push(Line::from(question));
    lines.push(Line::default());

    const MAX_SHOWN: usize = 6;
    for &t in targets.iter().take(MAX_SHOWN) {
        // Full path, not just the name: marks are held across navigation, so a
        // bare name list renders three unrelated helpers as "helper, helper,
        // helper" with nothing to say which app each belongs to.
        let size = format_size(app.tree.nodes[t].size);
        let path = app.tree.path_of(t);
        let room = 66usize.saturating_sub(size.len() + 6);
        lines.push(Line::from(format!(
            "  {}  ({size})",
            truncate_left(&path, room)
        )));
    }
    if targets.len() > MAX_SHOWN {
        lines.push(Line::from(format!(
            "  … and {} more",
            targets.len() - MAX_SHOWN
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("[Y]", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("es    "),
        Span::styled("[N]", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("o"),
    ]));

    let height = lines.len() as u16 + 2;
    let area = centered(70, height, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(color)),
        ),
        area,
    );
}

fn draw_ending(f: &mut Frame, count: u64) {
    let area = centered(40, 3, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(format!("Ending {count} process(es)…"))
            .centered()
            .block(Block::bordered().border_style(Style::new().fg(Color::Yellow))),
        area,
    );
}

fn draw_confirm_quit(f: &mut Frame) {
    let area = centered(40, 3, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new("Esc to quit, Backspace to go back")
            .centered()
            .block(Block::bordered().title(" Quit? ")),
        area,
    );
}

pub fn draw_scanning(f: &mut Frame, progress: &ScanProgress, elapsed: Duration) {
    let area = centered(72, 9, f.area());
    let spinner = SPINNER[(elapsed.as_millis() / 80) as usize % SPINNER.len()];

    let processes = progress.processes.load(Ordering::Relaxed);
    let bytes = progress.bytes.load(Ordering::Relaxed);

    let lines = vec![
        Line::from(format!("{spinner}  Reading processes")).bold(),
        Line::default(),
        Line::from(format!(
            "{} processes    {}",
            format_count(processes),
            format_size(bytes)
        )),
        Line::default(),
        Line::default(),
        Line::from(format!("elapsed: {:.1} s", elapsed.as_secs_f64())),
        Line::default(),
        Line::from("press q to cancel").style(Style::new().fg(Color::DarkGray)),
    ];

    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" ramtree ")),
        area,
    );
}

/// Half-cell resolution: the eighth blocks (U+2589–U+258F) are missing from
/// common Windows console fonts and render as tofu, but █ and ▌ are in CP437.
fn bar(pct: f64) -> String {
    let halves = (pct / 100.0 * BAR_WIDTH as f64 * 2.0).round() as usize;
    let full = (halves / 2).min(BAR_WIDTH);
    let mut s = "█".repeat(full);
    if full < BAR_WIDTH && halves % 2 == 1 {
        s.push('▌');
    }
    while s.chars().count() < BAR_WIDTH {
        s.push(' ');
    }
    s
}

fn truncate_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_owned();
    }
    let skip = count - max.saturating_sub(1);
    format!("…{}", s.chars().skip(skip).collect::<String>())
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::{ProcInfo, build};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn bar_widths() {
        assert_eq!(bar(0.0), "          ");
        assert_eq!(bar(100.0), "██████████");
        assert_eq!(bar(50.0), "█████     ");
        assert_eq!(bar(55.0), "█████▌    ");
    }

    /// An `App` over `n` top-level processes of descending size.
    fn synthetic_app(n: usize) -> crate::app::App {
        let procs = (0..n)
            .map(|i| ProcInfo {
                pid: i as u32 + 1,
                parent: None,
                name: format!("f{i:06}"),
                // Descending so sort order is predictable and stable.
                memory: (n - i) as u64 * 10,
                start_time: 0,
            })
            .collect();
        crate::app::App::new(build(procs, 1 << 30, 1 << 29))
    }

    #[test]
    fn table_renders_bars_left_aligned() {
        let p = |pid, name: &str, memory| ProcInfo {
            pid,
            parent: None,
            name: name.to_owned(),
            memory,
            start_time: 0,
        };
        let mut app =
            crate::app::App::new(build(vec![p(1, "big", 8000), p(2, "small", 2000)], 0, 0));

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let line = |y: u16| -> String {
            (0..80)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<Vec<_>>()
                .join("")
        };
        // Largest process first, bar left-aligned right after the % column.
        assert!(line(2).contains("80.0% ████████   "), "row: {:?}", line(2));
        assert!(line(2).contains("big (1)"));
        assert!(line(3).contains("20.0% ██        "), "row: {:?}", line(3));
        assert!(line(3).contains("small (2)"));
    }

    /// Rendered data rows, in order, as trimmed name text.
    fn rendered_names(app: &mut crate::app::App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = Vec::new();
        // Row 0 is the header line, then data rows until the footer.
        for y in 2..h.saturating_sub(2) {
            let text: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            if let Some(tok) = text.split_whitespace().find(|t| t.starts_with('f')) {
                out.push(tok.to_owned());
            }
        }
        out
    }

    /// The highlight index is relative to the rendered window, so a scrolled view
    /// must still paint the selected row and no other.
    #[test]
    fn selected_row_is_highlighted_within_the_window() {
        for (n, selected) in [(5usize, 0usize), (5, 3), (1000, 0), (1000, 999)] {
            let mut app = synthetic_app(n);
            app.selected = selected;
            let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let buf = terminal.backend().buffer().clone();

            // Table occupies y=1 (header) then data rows from y=2.
            let highlighted: Vec<u16> = (2..10)
                .filter(|&y| buf[(2, y)].style().bg == Some(Color::Blue))
                .collect();
            assert_eq!(
                highlighted.len(),
                1,
                "n={n} selected={selected}: {} highlighted rows, expected 1",
                highlighted.len()
            );
            // It must be the row holding the selection.
            let expected_y = 2 + (app.selected - app.offset) as u16;
            assert_eq!(
                highlighted[0], expected_y,
                "n={n} selected={selected}: highlight on wrong row"
            );
        }
    }

    /// The proportional bar is the at-a-glance cue; it must actually be coloured.
    #[test]
    fn bars_render_in_green() {
        let mut app = synthetic_app(4);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();

        // Find any full-block glyph and check its foreground.
        let mut found = false;
        for y in 2..8u16 {
            for x in 0..80u16 {
                if buf[(x, y)].symbol() == "█" {
                    assert_eq!(
                        buf[(x, y)].style().fg,
                        Some(Color::Green),
                        "bar glyph at ({x},{y}) is not green"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "no bar glyphs rendered at all");
    }

    #[test]
    fn window_empty_group() {
        let mut app = synthetic_app(0);
        assert_eq!(app.visible_range(10), 0..0);
        assert_eq!(app.offset, 0);
        // Must not panic with no rows.
        let _ = rendered_names(&mut app, 80, 12);
    }

    #[test]
    fn window_single_entry() {
        let mut app = synthetic_app(1);
        assert_eq!(app.visible_range(10), 0..1);
        assert_eq!(rendered_names(&mut app, 80, 12), vec!["f000000"]);
    }

    #[test]
    fn window_selection_at_top_shows_first_rows() {
        let mut app = synthetic_app(1000);
        app.selected = 0;
        assert_eq!(app.visible_range(5), 0..5);
        assert_eq!(app.offset, 0);
    }

    #[test]
    fn window_selection_at_end_scrolls_to_bottom() {
        let mut app = synthetic_app(1000);
        app.selected = 999;
        let r = app.visible_range(5);
        assert_eq!(r, 995..1000);
        // The selection must land inside the drawn window.
        assert!(r.contains(&app.selected));
    }

    #[test]
    fn window_shrinks_when_kill_removes_rows() {
        let mut app = synthetic_app(20);
        app.selected = 19;
        app.visible_range(5);
        assert_eq!(app.offset, 15);
        // Simulate the list shrinking under a scrolled-down view.
        app.rows.truncate(6);
        let r = app.visible_range(5);
        assert!(r.end <= 6, "range {r:?} exceeds row count");
        assert!(app.selected < 6, "selection {} out of range", app.selected);
        assert!(r.contains(&app.selected));
    }

    #[test]
    fn window_reclamps_after_resize() {
        let mut app = synthetic_app(100);
        app.selected = 99;
        app.visible_range(5);
        assert_eq!(app.offset, 95);
        // A taller terminal must not leave blank space below the last row.
        let r = app.visible_range(40);
        assert_eq!(r, 60..100);
        assert!(r.contains(&app.selected));
        // And a shorter one must bring the selection back into view.
        let r = app.visible_range(3);
        assert!(r.contains(&app.selected), "range {r:?} lost selection");
    }

    #[test]
    fn window_renders_only_visible_slice() {
        let mut app = synthetic_app(100_000);
        // Sorted by size descending, so f000000 is first.
        let names = rendered_names(&mut app, 80, 12);
        assert!(names.len() <= 12, "drew {} rows into 12 lines", names.len());
        assert_eq!(names.first().map(String::as_str), Some("f000000"));
        assert_eq!(names.get(1).map(String::as_str), Some("f000001"));
    }
}
