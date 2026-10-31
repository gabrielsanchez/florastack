//! What a user can do from the menus, the keyboard and the command palette ([`Action`]), with each action's label
//! and keyboard shortcut. One table drives the keyboard and Help > Shortcuts, so that the two never disagree; most
//! actions dispatch one engine command (`app.rs`), the rest change only what the window shows.

use egui::{Key, KeyboardShortcut, Modifiers};

/// An action of the app. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    New,
    Open,
    AddFrames,
    ImportStacks,
    Save,
    SaveAs,
    Close,
    Quit,
    Undo,
    Redo,
    Palette,
    Align,
    Preview,
    Stack,
    CancelJobs,
    ZoomFit,
    Zoom100,
    Zoom200,
    ZoomIn,
    ZoomOut,
    NextFrame,
    PreviousFrame,
    RemoveFrame,
    ToggleTheme,
    Shortcuts,
    Preferences,
    /// The Retouch tab's brush, smaller and larger.
    BrushSmaller,
    BrushLarger,
}

impl Action {
    /// Every action, in the order Help > Shortcuts lists them.
    pub const ALL: [Action; 28] = [
        Action::New,
        Action::Open,
        Action::AddFrames,
        Action::ImportStacks,
        Action::Save,
        Action::SaveAs,
        Action::Close,
        Action::Quit,
        Action::Undo,
        Action::Redo,
        Action::Palette,
        Action::Align,
        Action::Preview,
        Action::Stack,
        Action::CancelJobs,
        Action::ZoomFit,
        Action::Zoom100,
        Action::Zoom200,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::NextFrame,
        Action::PreviousFrame,
        Action::RemoveFrame,
        Action::ToggleTheme,
        Action::Shortcuts,
        Action::Preferences,
        Action::BrushSmaller,
        Action::BrushLarger,
    ];

    /// The Fluent key of the action's label.
    pub fn label_key(self) -> &'static str {
        match self {
            Action::New => "action-new",
            Action::Open => "action-open",
            Action::AddFrames => "action-add-frames",
            Action::ImportStacks => "action-import-stacks",
            Action::Save => "action-save",
            Action::SaveAs => "action-save-as",
            Action::Close => "action-close",
            Action::Quit => "action-quit",
            Action::Undo => "action-undo",
            Action::Redo => "action-redo",
            Action::Palette => "action-palette",
            Action::Align => "action-align",
            Action::Preview => "action-preview",
            Action::Stack => "action-stack",
            Action::CancelJobs => "action-cancel-jobs",
            Action::ZoomFit => "action-zoom-fit",
            Action::Zoom100 => "action-zoom-100",
            Action::Zoom200 => "action-zoom-200",
            Action::ZoomIn => "action-zoom-in",
            Action::ZoomOut => "action-zoom-out",
            Action::NextFrame => "action-next-frame",
            Action::PreviousFrame => "action-previous-frame",
            Action::RemoveFrame => "action-remove-frame",
            Action::ToggleTheme => "action-toggle-theme",
            Action::Shortcuts => "action-shortcuts",
            Action::Preferences => "action-preferences",
            Action::BrushSmaller => "action-brush-smaller",
            Action::BrushLarger => "action-brush-larger",
        }
    }

    /// The action's label.
    pub fn label(self) -> String {
        crate::i18n::tr(self.label_key())
    }

    /// The action's keyboard shortcut. `COMMAND` is Ctrl, or Cmd on macOS.
    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let (modifiers, key) = match self {
            Action::New => (cmd, Key::N),
            Action::Open => (cmd, Key::O),
            Action::AddFrames => (cmd, Key::I),
            Action::ImportStacks => return None,
            Action::Save => (cmd, Key::S),
            Action::SaveAs => (cmd_shift, Key::S),
            Action::Close => (cmd, Key::W),
            Action::Quit => (cmd, Key::Q),
            Action::Undo => (cmd, Key::Z),
            Action::Redo => (cmd_shift, Key::Z),
            Action::Palette => (cmd, Key::K),
            Action::Align => (cmd, Key::L),
            Action::Preview => (cmd_shift, Key::Enter),
            Action::Stack => (cmd, Key::Enter),
            Action::CancelJobs => (cmd, Key::Period),
            Action::ZoomFit => (cmd, Key::Num0),
            Action::Zoom100 => (cmd, Key::Num1),
            Action::Zoom200 => (cmd, Key::Num2),
            Action::ZoomIn => (cmd, Key::Equals),
            Action::ZoomOut => (cmd, Key::Minus),
            Action::NextFrame => (Modifiers::NONE, Key::ArrowDown),
            Action::PreviousFrame => (Modifiers::NONE, Key::ArrowUp),
            Action::RemoveFrame => (Modifiers::NONE, Key::Delete),
            Action::ToggleTheme => (cmd_shift, Key::T),
            Action::Shortcuts => (Modifiers::NONE, Key::F1),
            Action::Preferences => (cmd, Key::Comma),
            Action::BrushSmaller => (Modifiers::NONE, Key::OpenBracket),
            Action::BrushLarger => (Modifiers::NONE, Key::CloseBracket),
        };
        Some(KeyboardShortcut::new(modifiers, key))
    }

    /// The actions with shortcuts, those with more modifiers first: egui matches a shortcut whatever extra Shift is
    /// held, so Ctrl+Shift+S must be looked for before Ctrl+S takes it.
    pub fn by_shortcut() -> Vec<(Action, KeyboardShortcut)> {
        let mut all: Vec<(Action, KeyboardShortcut)> = Action::ALL.iter().filter_map(|&a| a.shortcut().map(|s| (a, s))).collect();
        let weight = |m: Modifiers| u8::from(m.shift) + u8::from(m.alt) + u8::from(m.command || m.ctrl || m.mac_cmd);
        all.sort_by_key(|(_, s)| std::cmp::Reverse(weight(s.modifiers)));
        all
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn no_two_actions_share_a_shortcut() {
        let mut seen = BTreeSet::new();
        for (action, shortcut) in Action::by_shortcut() {
            let key = format!("{:?} {:?}", shortcut.modifiers, shortcut.logical_key);
            assert!(seen.insert(key.clone()), "{action:?} shares {key}");
        }
    }

    #[test]
    fn every_action_is_listed_once_with_a_label() {
        let listed: BTreeSet<&str> = Action::ALL.iter().map(|a| a.label_key()).collect();
        assert_eq!(listed.len(), Action::ALL.len());
        assert!(Action::ALL.iter().all(|a| a.label() != a.label_key()), "every label has its English string");
    }

    #[test]
    fn longer_shortcuts_are_matched_first() {
        let order = Action::by_shortcut();
        let at = |a: Action| order.iter().position(|(x, _)| *x == a).unwrap();
        assert!(at(Action::SaveAs) < at(Action::Save) && at(Action::Redo) < at(Action::Undo));
    }
}
