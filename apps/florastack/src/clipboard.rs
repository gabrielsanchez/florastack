//! The clipboard's shortcuts, handled by the app rather than egui-winit (`NativeOptions::clipboard_shortcuts` is off):
//! egui-winit pastes only text (or an image), so files copied in Explorer or Finder, which put no text on the
//! clipboard, gave no event at all. [`translate`] turns the shortcuts' key presses into egui's `Cut`, `Copy` and
//! `Paste` events, as egui-winit did, so that text fields work as before, and asks the clipboard for its files first
//! when the shortcut is a paste: files go to the app, which opens or adds them as dropped ones.
//!
//! The shortcuts are egui-winit's: Cmd/Ctrl+X, C and V, the dedicated Cut, Copy and Paste keys, and on Windows also
//! Shift+Delete, Ctrl+Insert and Shift+Insert.

use std::path::PathBuf;

/// What the clipboard holds, as a paste reads it.
pub(crate) trait Source {
    /// The files copied in a file manager; none when it holds no file list.
    fn files(&mut self) -> Vec<PathBuf>;
    /// Its text; none when it holds none.
    fn text(&mut self) -> Option<String>;
}

/// The system's clipboard, through arboard (which egui-winit uses too), opened at the first paste.
#[derive(Default)]
pub(crate) struct System {
    clipboard: Option<arboard::Clipboard>,
    failed: bool,
}

impl System {
    fn get(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() && !self.failed {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(e) => {
                    // Once: a clipboard that can't be opened stays so for the session.
                    tracing::warn!("the clipboard can't be read: {e}");
                    self.failed = true;
                }
            }
        }
        self.clipboard.as_mut()
    }
}

impl Source for System {
    fn files(&mut self) -> Vec<PathBuf> {
        self.get().and_then(|c| c.get().file_list().ok()).unwrap_or_default()
    }

    fn text(&mut self) -> Option<String> {
        self.get().and_then(|c| c.get_text().ok())
    }
}

/// Which clipboard shortcut a key press is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    Cut,
    Copy,
    Paste,
}

fn shortcut(modifiers: egui::Modifiers, key: egui::Key) -> Option<Shortcut> {
    use egui::Key;
    let windows = cfg!(target_os = "windows");
    if key == Key::Cut || (modifiers.command_only() && key == Key::X) || (windows && modifiers.shift && key == Key::Delete) {
        Some(Shortcut::Cut)
    } else if key == Key::Copy || (modifiers.command_only() && key == Key::C) || (windows && modifiers.ctrl && key == Key::Insert) {
        Some(Shortcut::Copy)
    } else if key == Key::Paste || (modifiers.command_only() && key == Key::V) || (windows && modifiers.shift && key == Key::Insert) {
        Some(Shortcut::Paste)
    } else {
        None
    }
}

/// Replaces the clipboard shortcuts' key presses in `events` with egui's `Cut`, `Copy` and `Paste` events, and returns
/// the files a paste found. While a text field has the keyboard (`typing`), a paste gives it text: the clipboard's,
/// or else its files' paths, one a line. Otherwise files copied in a file manager come back to be opened, and text
/// is pasted as before (the app looks for paths in it).
pub(crate) fn translate(events: &mut Vec<egui::Event>, typing: bool, source: &mut impl Source) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let taken = std::mem::take(events);
    for event in taken {
        // `key` is already the logical key, or the physical one for layouts without Latin letters (egui-winit's).
        let egui::Event::Key { key, pressed: true, modifiers, .. } = &event else {
            events.push(event);
            continue;
        };
        let Some(kind) = shortcut(*modifiers, *key) else {
            events.push(event);
            continue;
        };
        match kind {
            Shortcut::Cut => events.push(egui::Event::Cut),
            Shortcut::Copy => events.push(egui::Event::Copy),
            Shortcut::Paste => {
                let text = if typing {
                    source.text().or_else(|| {
                        let listed = source.files();
                        (!listed.is_empty()).then(|| listed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"))
                    })
                } else {
                    let listed = source.files();
                    if listed.is_empty() {
                        source.text()
                    } else {
                        files.extend(listed);
                        None
                    }
                };
                if let Some(text) = text.map(|t| t.replace("\r\n", "\n")).filter(|t| !t.is_empty()) {
                    events.push(egui::Event::Paste(text));
                }
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        files: Vec<PathBuf>,
        text: Option<String>,
    }

    impl Source for Fake {
        fn files(&mut self) -> Vec<PathBuf> {
            self.files.clone()
        }
        fn text(&mut self) -> Option<String> {
            self.text.clone()
        }
    }

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn copied_files_paste_as_files_and_text_as_text() {
        let paste = press(egui::Key::V, egui::Modifiers::COMMAND);
        let mut copied = Fake { files: vec![PathBuf::from("a.tif"), PathBuf::from("b.tif")], text: None };
        let mut events = vec![paste.clone()];
        assert_eq!(translate(&mut events, false, &mut copied), [PathBuf::from("a.tif"), PathBuf::from("b.tif")]);
        assert!(events.is_empty(), "{events:?}");
        // In a text field, the files' paths as text.
        let mut events = vec![paste.clone()];
        assert_eq!(translate(&mut events, true, &mut copied), Vec::<PathBuf>::new());
        assert_eq!(events, [egui::Event::Paste("a.tif\nb.tif".to_owned())]);
        // Text, with Windows' line ends made plain; nothing at all gives no event.
        let mut text = Fake { files: Vec::new(), text: Some("one\r\ntwo".to_owned()) };
        for typing in [false, true] {
            let mut events = vec![paste.clone()];
            assert_eq!(translate(&mut events, typing, &mut text), Vec::<PathBuf>::new());
            assert_eq!(events, [egui::Event::Paste("one\ntwo".to_owned())]);
        }
        let mut events = vec![paste];
        assert_eq!(translate(&mut events, false, &mut Fake { files: Vec::new(), text: None }), Vec::<PathBuf>::new());
        assert_eq!(events, Vec::<egui::Event>::new());
    }

    #[test]
    fn cut_and_copy_stay_egui_events_and_other_keys_pass() {
        let mut nothing = Fake { files: Vec::new(), text: None };
        let other = press(egui::Key::V, egui::Modifiers::NONE);
        let released = egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::COMMAND };
        let mut events = vec![
            press(egui::Key::X, egui::Modifiers::COMMAND),
            egui::Event::Text("x".to_owned()),
            press(egui::Key::C, egui::Modifiers::COMMAND),
            other.clone(),
            released.clone(),
            press(egui::Key::Copy, egui::Modifiers::NONE),
        ];
        translate(&mut events, true, &mut nothing);
        assert_eq!(events, [egui::Event::Cut, egui::Event::Text("x".to_owned()), egui::Event::Copy, other, released, egui::Event::Copy]);
    }
}
