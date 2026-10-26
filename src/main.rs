use std::io;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use ramtree::app::App;
use ramtree::procs::{self, ScanProgress, Tree};
use ramtree::ui;

#[derive(Parser, Debug)]
#[command(
    name = "ramtree",
    version,
    about = "Memory usage TUI: see which processes use your RAM, and end them"
)]
struct Args {}

fn main() -> io::Result<()> {
    Args::parse();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let Some(tree) = run_scan(terminal)? else {
        return Ok(());
    };
    let mut app = App::new(tree);
    run_browse(terminal, &mut app)
}

/// Scan on a background thread while showing live progress.
/// Returns None if the user cancelled.
fn run_scan(terminal: &mut DefaultTerminal) -> io::Result<Option<Tree>> {
    let progress = Arc::new(ScanProgress::default());
    let (tx, rx) = mpsc::channel();

    {
        let progress = Arc::clone(&progress);
        thread::spawn(move || {
            let _ = tx.send(procs::scan(&progress));
        });
    }

    let start = Instant::now();
    loop {
        if let Ok(tree) = rx.try_recv() {
            return Ok(Some(tree));
        }
        terminal.draw(|f| ui::draw_scanning(f, &progress, start.elapsed()))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            if key.kind == KeyEventKind::Press
                && (key.code == KeyCode::Char('q') || key.code == KeyCode::Esc || ctrl_c)
            {
                return Ok(None);
            }
        }
    }
}

fn run_browse(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        // Confirmed kills run here so the "Ending…" frame above is visible
        // while waiting for processes to exit.
        if let Some((targets, force)) = app.pending_kill.take() {
            app.perform_kill(&targets, force);
            continue;
        }

        // Reading the process table takes well under a second, so a refresh
        // keeps the current view rather than going back through the scan screen.
        if app.rescan {
            app.rescan = false;
            app.reload(procs::scan(&ScanProgress::default()));
            continue;
        }

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }
        if app.quit {
            return Ok(());
        }
    }
}
