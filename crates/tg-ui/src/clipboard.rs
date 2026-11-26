//! Clipboard shortcuts as `egui` delivers them.
//!
//! eframe's winit backend turns `Ctrl+C` (`Cmd+C`, `Ctrl+Insert`, the Copy key) into
//! [`egui::Event::Copy`] and sends no [`egui::Event::Key`] for it, so
//! `i.modifiers.command && i.key_pressed(Key::C)` never fires in the running app (TGB-3016).
//! On layouts without Latin letters (Russian, Ukrainian, Greek) the backend maps the shortcut
//! through the physical `C` key and still sends [`egui::Event::Copy`]. Qt's equivalent is
//! `QKeySequence::Copy` (`HistoryInner::keyPressEvent`).

use egui::{Event, InputState, Key, Modifiers};

/// `true` when this frame's input asks to copy: [`Event::Copy`] from the platform backend, or
/// a raw `Ctrl+C` / `Ctrl+Insert` / Copy key event from a host that does not translate
/// clipboard shortcuts (tests, `clipboard_shortcuts(false)`). A non-Latin layout counts when
/// the physical key is `C`.
#[must_use]
pub fn copy_requested(input: &InputState) -> bool {
    input.events.iter().any(is_copy_event)
}

/// `true` for one input event that means "copy" (see [`copy_requested`]).
#[must_use]
pub fn is_copy_event(event: &Event) -> bool {
    match event {
        Event::Copy => true,
        Event::Key {
            key,
            physical_key,
            pressed: true,
            modifiers,
            ..
        } => is_copy_chord(*key, *physical_key, *modifiers),
        _ => false,
    }
}

fn is_copy_chord(key: Key, physical_key: Option<Key>, modifiers: Modifiers) -> bool {
    if key == Key::Copy {
        return true;
    }
    if modifiers.command_only() && (key == Key::C || physical_key == Some(Key::C)) {
        return true;
    }
    cfg!(target_os = "windows") && modifiers.ctrl && !modifiers.shift && key == Key::Insert
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, physical_key: Option<Key>, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn the_backend_copy_event_counts() {
        assert!(is_copy_event(&Event::Copy));
    }

    #[test]
    fn a_raw_command_c_counts_on_latin_and_by_physical_key() {
        assert!(is_copy_event(&key(
            Key::C,
            Some(Key::C),
            Modifiers::COMMAND
        )));
        // A layout whose letter on the C key egui cannot name: only the physical key is `C`.
        assert!(is_copy_event(&key(
            Key::Questionmark,
            Some(Key::C),
            Modifiers::COMMAND
        )));
        assert!(is_copy_event(&key(Key::Copy, None, Modifiers::NONE)));
    }

    #[test]
    fn other_chords_do_not_copy() {
        assert!(!is_copy_event(&key(Key::C, Some(Key::C), Modifiers::NONE)));
        assert!(!is_copy_event(&key(
            Key::C,
            Some(Key::C),
            Modifiers::COMMAND | Modifiers::SHIFT
        )));
        assert!(!is_copy_event(&key(
            Key::V,
            Some(Key::V),
            Modifiers::COMMAND
        )));
        assert!(!is_copy_event(&Event::Paste("x".to_owned())));
    }
}
