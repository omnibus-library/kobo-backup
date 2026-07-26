use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::event::{Event as CtEvent, KeyEvent, KeyEventKind};

use crate::worker::WorkerMsg;

/// Unified event stream consumed by the main UI loop.
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Resize,
    Tick,
    Worker(WorkerMsg),
}

pub struct Events {
    rx: Receiver<Event>,
    tx: Sender<Event>,
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Events {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        let input_tx = tx.clone();
        // Input thread: blocking crossterm reads, forwarded into the channel.
        std::thread::spawn(move || loop {
            match crossterm::event::read() {
                Ok(CtEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                    if input_tx.send(Event::Key(key)).is_err() {
                        break;
                    }
                }
                Ok(CtEvent::Resize(_, _)) => {
                    if input_tx.send(Event::Resize).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        });
        Events { rx, tx }
    }

    /// Sender handed to worker threads.
    pub fn worker_sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    /// Next event, or Tick if nothing arrives within the redraw interval.
    pub fn next(&self) -> Event {
        match self.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(_) => Event::Tick,
        }
    }
}
