//! hunkr — a terminal git diff reviewer for AI coding workflows.
//!
//! Architecture: a single owned [`app::App`] is the source of truth, mutated
//! only here on the UI thread in response to events from a channel. Rendering
//! is event-driven and dirty-flagged — we draw only when state changed, so the
//! app sits at zero CPU while idle.

mod app;
mod cli;
mod event;
mod git;
mod model;
mod render;
mod terminal;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::unbounded;
use ratatui::crossterm::event::Event as CtEvent;

use crate::app::App;
use crate::event::Event;

fn main() -> Result<()> {
    let args = cli::Args::parse();
    let start = match args.path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let repo_root = git::repo::discover(&start)?;
    let mut app = App::new(repo_root)?;

    terminal::install_panic_hook();
    let mut tui = terminal::init()?;
    let result = run(&mut tui, &mut app);
    terminal::restore()?;
    result
}

fn run(tui: &mut terminal::Tui, app: &mut App) -> Result<()> {
    let (tx, rx) = unbounded::<Event>();
    event::spawn_input(tx);

    while !app.should_quit {
        if app.dirty {
            tui.draw(|f| ui::render(f, app))?;
            app.dirty = false;
        }
        // Block until something happens — zero idle CPU.
        match rx.recv() {
            Ok(Event::Input(ev)) => handle_terminal_event(app, ev),
            Err(_) => break, // all senders dropped
        }
    }
    Ok(())
}

fn handle_terminal_event(app: &mut App, ev: CtEvent) {
    match ev {
        CtEvent::Key(key) => app.on_key(key),
        CtEvent::Resize(_, _) => app.dirty = true,
        _ => {}
    }
}
