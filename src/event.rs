//! Event bus. The UI thread blocks on a single channel; producer threads feed
//! it. For the Phase-A slice the only producer is the terminal input reader.
//! M3 adds filesystem-watch and git-worker producers that push `Fs` and
//! `GitRefreshed` variants onto this same channel.

use std::thread;

use crossbeam_channel::Sender;
use ratatui::crossterm::event::{self, Event as CtEvent};

pub enum Event {
    Input(CtEvent),
}

/// Spawn a thread that forwards terminal events onto the bus. It exits when the
/// receiver is dropped (i.e. when the app shuts down).
pub fn spawn_input(tx: Sender<Event>) {
    thread::spawn(move || {
        // Exits when `event::read()` errors or the receiver is dropped.
        while let Ok(ev) = event::read() {
            if tx.send(Event::Input(ev)).is_err() {
                break;
            }
        }
    });
}
