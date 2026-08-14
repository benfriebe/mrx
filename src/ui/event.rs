use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::time::Duration;

pub enum AppEvent {
    Key(KeyCode, KeyModifiers),
    Tick,
}

pub fn poll(timeout: Duration) -> Option<AppEvent> {
    if event::poll(timeout).ok()? {
        if let Event::Key(key) = event::read().ok()? {
            // Only respond to Press events (ignore Release on some terminals)
            if key.kind == crossterm::event::KeyEventKind::Press {
                return Some(AppEvent::Key(key.code, key.modifiers));
            }
        }
    }
    Some(AppEvent::Tick)
}
