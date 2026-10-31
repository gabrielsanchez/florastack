//! The command palette (Ctrl/Cmd+K, [`Palette`]): searches the app's actions and every command of the engine's
//! registry by title and name. An action or a command that needs no parameters runs as it is picked; any other opens
//! its parameters as JSON, starting from the command's first example, checked against the command's schema
//! (`Registry::check`) before it runs.

use egui::{Key, Modal};
use florastack_engine::{CommandInfo, Registry, Value};

use crate::actions::Action;
use crate::i18n::{command_summary, command_title, tr};

/// What the palette picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Picked {
    Action(Action),
    Command { name: String, params: Value },
}

/// An entry of the list.
#[derive(Clone, Debug)]
struct Entry {
    title: String,
    detail: String,
    target: Target,
}

#[derive(Clone, Debug)]
enum Target {
    Action(Action),
    Command(&'static str),
}

/// A command's parameters being written.
struct Editing {
    name: &'static str,
    title: String,
    text: String,
    error: Option<String>,
}

/// See the module documentation.
#[derive(Default)]
pub struct Palette {
    open: bool,
    query: String,
    selected: usize,
    editing: Option<Editing>,
    focus: bool,
}

/// Whether every character of `needle` appears in `hay` in order (case ignored), and how well: a match at the start
/// of the text, or of a word, scores higher. `None` when it doesn't match.
fn score(needle: &str, hay: &str) -> Option<i32> {
    let (needle, hay) = (needle.to_lowercase(), hay.to_lowercase());
    if needle.is_empty() {
        return Some(0);
    }
    if hay.starts_with(&needle) {
        return Some(1000);
    }
    if hay.contains(&needle) {
        let word = hay.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&needle));
        return Some(if word { 800 } else { 600 });
    }
    let mut chars = hay.chars();
    let mut gaps = 0;
    for c in needle.chars() {
        let mut skipped = 0;
        loop {
            match chars.next() {
                Some(h) if h == c => break,
                Some(_) => skipped += 1,
                None => return None,
            }
        }
        gaps += skipped;
    }
    Some(400 - gaps.min(399))
}

/// Whether a command's parameters schema requires any field.
fn needs_parameters(info: &CommandInfo) -> bool {
    info.params["required"].as_array().is_some_and(|r| !r.is_empty())
}

impl Palette {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.editing = None;
        self.focus = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The entries matching `query`, best first.
    fn entries(query: &str, registry: &Registry) -> Vec<Entry> {
        let actions = Action::ALL.iter().map(|&a| Entry { title: a.label(), detail: String::new(), target: Target::Action(a) });
        let commands = registry.list().into_iter().map(|c| Entry {
            title: command_title(c.name, c.title),
            detail: format!("{} · {}", c.name, command_summary(c.name, c.summary)),
            target: Target::Command(c.name),
        });
        let mut scored: Vec<(i32, Entry)> = actions
            .chain(commands)
            .filter_map(|e| {
                let name = match e.target {
                    Target::Command(n) => n,
                    Target::Action(_) => "",
                };
                let best = score(query, &e.title).max(score(query, name));
                best.map(|s| (s, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        scored.into_iter().map(|(_, e)| e).collect()
    }

    /// Draws the palette when it is open; what was picked, if anything.
    pub fn ui(&mut self, ctx: &egui::Context, registry: &Registry) -> Option<Picked> {
        if !self.open {
            return None;
        }
        let mut picked = None;
        let mut back = false;
        let modal = Modal::new(egui::Id::unique("command-palette")).show(ctx, |ui| {
            ui.set_width(560.0);
            if let Some(editing) = &mut self.editing {
                ui.heading(&editing.title);
                ui.label(tr("palette-parameters"));
                ui.add(egui::TextEdit::multiline(&mut editing.text).code_editor().desired_rows(8).desired_width(f32::INFINITY));
                if let Some(error) = &editing.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("palette-run")).clicked() {
                        match serde_json::from_str::<Value>(&editing.text) {
                            Ok(params) => match registry.check(editing.name, params.clone()) {
                                Ok(()) => picked = Some(Picked::Command { name: editing.name.to_owned(), params }),
                                Err(e) => editing.error = Some(e.info().message),
                            },
                            Err(e) => editing.error = Some(e.to_string()),
                        }
                    }
                    if ui.button(tr("palette-back")).clicked() {
                        back = true;
                    }
                });
                return;
            }
            let search = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text(tr("palette-hint")).desired_width(f32::INFINITY));
            if self.focus {
                search.request_focus();
                self.focus = false;
            }
            if search.changed() {
                self.selected = 0;
            }
            let entries = Palette::entries(&self.query, registry);
            let (down, up, enter) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter)));
            if down {
                self.selected = (self.selected + 1).min(entries.len().saturating_sub(1));
            }
            if up {
                self.selected = self.selected.saturating_sub(1);
            }
            let mut chosen = enter.then_some(self.selected);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (i, entry) in entries.iter().enumerate().take(40) {
                    let row = ui.selectable_label(i == self.selected, &entry.title);
                    if !entry.detail.is_empty() {
                        ui.weak(&entry.detail);
                    }
                    if row.clicked() {
                        chosen = Some(i);
                    }
                    if i == self.selected && (down || up) {
                        row.scroll_to_me(None);
                    }
                }
                if entries.is_empty() {
                    ui.weak(tr("palette-nothing"));
                }
            });
            if let Some(entry) = chosen.and_then(|i| entries.get(i)) {
                match entry.target {
                    Target::Action(a) => picked = Some(Picked::Action(a)),
                    Target::Command(name) => match registry.get(name) {
                        Some(info) if needs_parameters(info) => {
                            let example = info.examples.first().cloned().unwrap_or(Value::Object(serde_json::Map::new()));
                            let text = serde_json::to_string_pretty(&example).unwrap_or_else(|_| "{}".to_owned());
                            self.editing = Some(Editing { name, title: entry.title.clone(), text, error: None });
                        }
                        _ => picked = Some(Picked::Command { name: name.to_owned(), params: Value::Object(serde_json::Map::new()) }),
                    },
                }
            }
        });
        if back {
            self.focus = true;
            self.editing = None;
        }
        if picked.is_some() || modal.should_close() {
            self.open = false;
            self.editing = None;
        }
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rank_prefixes_then_words_then_scattered_letters() {
        assert_eq!(score("", "Align frames"), Some(0));
        assert!(score("ali", "Align frames") > score("fra", "Align frames"));
        assert!(score("fra", "Align frames") > score("agn", "Align frames"));
        assert_eq!(score("xyz", "Align frames"), None);
        assert!(score("aln", "align.run").is_some(), "letters in order");
    }

    #[test]
    fn every_registry_command_can_be_found() {
        let registry = Registry::standard();
        let all = Palette::entries("", &registry);
        for name in registry.names() {
            assert!(all.iter().any(|e| matches!(e.target, Target::Command(n) if n == name)), "{name}");
            let found = Palette::entries(name, &registry);
            assert!(matches!(found.first().map(|e| &e.target), Some(Target::Command(n)) if *n == name), "{name} is found first by its name");
        }
    }
}
