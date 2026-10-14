//! Native terminal frontend sharing the desktop document and Mermaid renderer.

mod app;
mod graph;
mod view;
mod vim;

#[cfg(test)]
mod vim_integration_tests;

use std::error::Error;
use std::io::{self, IsTerminal};
use std::panic::{self, PanicHookInfo};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::{CrosstermBackend, TestBackend},
};

use crate::document::Document;
use app::App;

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

struct TerminalGuard {
    previous_hook: Option<Arc<PanicHook>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        let previous = Arc::new(panic::take_hook());
        let handler = previous.clone();
        let ui_thread = std::thread::current().id();
        panic::set_hook(Box::new(move |info| {
            // The renderer catches malformed-diagram panics on its worker.
            // Those must not tear down a healthy terminal session.
            if std::thread::current().id() == ui_thread {
                restore_terminal();
                handler(info);
            } else if std::thread::current().name() != Some("fin-tui-renderer") {
                handler(info);
            }
        }));
        let guard = Self {
            previous_hook: Some(previous),
        };
        enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        if !std::thread::panicking() {
            // Taking our hook drops its Arc, leaving the original owned here.
            drop(panic::take_hook());
            if let Some(previous) = self.previous_hook.take()
                && let Ok(previous) = Arc::try_unwrap(previous)
            {
                panic::set_hook(previous);
            }
        }
    }
}

fn restore_terminal() {
    let _ = execute!(
        io::stdout(),
        Show,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

fn load_document(path: Option<PathBuf>) -> Result<Document, String> {
    let mut document = Document::new(crate::EXAMPLE.to_owned());
    if let Some(path) = path {
        document.load(&path)?;
    }
    Ok(document)
}

/// Run the terminal editor without initializing the desktop UI or its backend.
pub fn run(path: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("Fin TUI needs an interactive terminal. Use --help or --snapshot for noninteractive output.".into());
    }
    let document = load_document(path)?;
    let mut app = App::new(document);
    app.start_worker();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    while !app.should_quit {
        app.poll_render();
        terminal.draw(|frame| view::draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(40))? {
            app.handle_event(event::read()?);
        }
    }
    Ok(())
}

/// Render a deterministic plain-text frame for documentation and headless checks.
pub fn snapshot(path: Option<PathBuf>, width: u16, height: u16) -> Result<String, Box<dyn Error>> {
    if !(40..=300).contains(&width) || !(12..=120).contains(&height) {
        return Err("Snapshot size must be 40–300 columns by 12–120 rows.".into());
    }
    let mut app = App::new(load_document(path)?);
    app.render_sync();
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| view::draw(frame, &mut app))?;
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..height {
        let mut row = String::new();
        let mut x = 0;
        while x < width {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        output.push_str(row.trim_end());
        output.push('\n');
    }
    Ok(output)
}
