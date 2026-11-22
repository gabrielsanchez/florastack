//! The Stack tab's guide, bundled with the app: `book/src/user/stack-tab.md`, built in at compile time, so that a
//! setting's "?" opens its section in a window, without a network and in the words of the version running. The page
//! is plain Markdown (headings, paragraphs, lists, bold, italics, code and links), which this draws itself: a link
//! to another section of the page opens it in the window, and a link elsewhere opens in the browser, as does the
//! whole guide online.

use egui::RichText;

use crate::i18n::tr;

/// The Stack tab's page of the guide.
pub const STACK_TAB: &str = include_str!("../../../book/src/user/stack-tab.md");

/// Where the window's open section is kept, between frames: egui's memory, since the "?" buttons that open it are
/// drawn deep in the tab with no access to the app's state.
fn id() -> egui::Id {
    egui::Id::unique("stack-tab help")
}

/// Opens the section `anchor` of the Stack tab's page in the help window.
pub fn open(ctx: &egui::Context, anchor: &str) {
    ctx.data_mut(|d| d.insert_temp(id(), anchor.to_owned()));
}

/// The section the help window shows, if it is open.
pub fn shown(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(id()))
}

/// GitHub's anchor for a heading, which the book's links and the "?" buttons use: lower case, spaces as hyphens,
/// other punctuation dropped.
pub fn anchor(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' => Some(c),
            _ => None,
        })
        .collect()
}

/// The section of `page` whose `## ` heading has `anchor`: its heading and its text, up to the next such heading.
pub fn section<'a>(page: &'a str, anchor: &str) -> Option<(&'a str, &'a str)> {
    let mut found: Option<(&str, usize)> = None;
    let mut at = 0;
    for line in page.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let Some(heading) = line.strip_prefix("## ") else { continue };
        if let Some((title, from)) = found {
            return Some((title, page.get(from..start)?.trim()));
        }
        if self::anchor(heading) == anchor {
            found = Some((heading.trim(), at));
        }
    }
    found.and_then(|(title, from)| Some((title, page.get(from..)?.trim())))
}

/// A paragraph or a list item of a section.
#[derive(Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(String),
    /// A list item, with its marker: "•" for a bullet, or its number ("1.").
    Item(String, String),
}

/// `text`'s paragraphs and list items, each joined into one line.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let number = trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if trimmed.is_empty() {
            blocks.push(Block::Paragraph(String::new()));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            blocks.push(Block::Item("•".to_owned(), item.to_owned()));
        } else if let Some((n, item)) = number {
            blocks.push(Block::Item(format!("{n}."), item.to_owned()));
        } else {
            // A line goes on the paragraph or the item before it.
            match blocks.last_mut() {
                Some(Block::Paragraph(p) | Block::Item(_, p)) if !p.is_empty() => {
                    p.push(' ');
                    p.push_str(trimmed);
                }
                _ => blocks.push(Block::Paragraph(trimmed.to_owned())),
            }
        }
    }
    blocks.retain(|b| !matches!(b, Block::Paragraph(p) if p.is_empty()));
    blocks
}

/// A piece of a line of text.
#[derive(Debug, PartialEq, Eq)]
pub enum Span<'a> {
    Text(&'a str),
    Strong(&'a str),
    Emphasis(&'a str),
    Code(&'a str),
    Link { text: &'a str, target: &'a str },
}

/// `line`'s pieces: `**bold**`, `*italics*` or `_italics_`, `` `code` `` and `[links](target)`; anything else, an
/// unmatched marker included, is text.
pub fn spans(line: &str) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let found = [("**", "**"), ("`", "`"), ("[", ")"), ("*", "*"), ("_", "_")]
            .iter()
            .filter_map(|(open, close)| {
                let start = rest.find(open)?;
                let after = start + open.len();
                let end = after + rest.get(after..)?.find(close)?;
                Some((start, after, end, *open, *close))
            })
            // The first marker, the longer one first where two start at once.
            .min_by_key(|(start, _, _, open, _)| (*start, usize::MAX - open.len()));
        let Some((start, after, end, open, close)) = found else {
            spans.push(Span::Text(rest));
            break;
        };
        let inner = rest.get(after..end).unwrap_or_default();
        let span = match open {
            "**" => Some(Span::Strong(inner)),
            "`" => Some(Span::Code(inner)),
            "[" => inner.split_once("](").map(|(text, target)| Span::Link { text, target }),
            _ => Some(Span::Emphasis(inner)),
        };
        // Emphasis hugs its text, as Markdown's does: "2 * 3 * 4" has none.
        let hugs = matches!(open, "`" | "[") || inner.trim() == inner;
        let Some(span) = span.filter(|_| !inner.is_empty() && hugs) else {
            // Not a span: the marker is text.
            spans.push(Span::Text(rest.get(..after).unwrap_or_default()));
            rest = rest.get(after..).unwrap_or_default();
            continue;
        };
        if start > 0 {
            spans.push(Span::Text(rest.get(..start).unwrap_or_default()));
        }
        spans.push(span);
        rest = rest.get(end + close.len()..).unwrap_or_default();
    }
    spans
}

/// Draws a line of text with its pieces, wrapped; a link clicked opens its section here, or its page in the browser.
fn line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in spans(text) {
            match span {
                Span::Text(t) => {
                    ui.label(t);
                }
                Span::Strong(t) => {
                    ui.label(RichText::new(t).strong());
                }
                Span::Emphasis(t) => {
                    ui.label(RichText::new(t).italics());
                }
                Span::Code(t) => {
                    ui.label(RichText::new(t).code());
                }
                Span::Link { text, target } => {
                    if ui.link(text).clicked() {
                        follow(ui.ctx(), target);
                    }
                }
            }
        }
    });
}

/// Follows a link of the page: to a section of it, in the window, or elsewhere in the guide or on the web, in the
/// browser (the guide's other pages on GitHub, as `docs_url` links the page).
fn follow(ctx: &egui::Context, target: &str) {
    if let Some(section) = target.strip_prefix('#') {
        open(ctx, section);
    } else if target.starts_with("https://") || target.starts_with("http://") {
        ctx.open_url(egui::OpenUrl::new_tab(target));
    } else {
        let base = crate::stack_tab::docs_url("");
        let folder = base.split("stack-tab.md").next().unwrap_or_default();
        ctx.open_url(egui::OpenUrl::new_tab(format!("{folder}{target}")));
    }
}

/// Draws the help window, when a section is open.
pub fn show(ctx: &egui::Context) {
    let Some(shown) = shown(ctx) else { return };
    let mut open = true;
    let section = section(STACK_TAB, &shown);
    egui::Window::new(tr("help-title")).id(id().with("window")).open(&mut open).default_width(420.0).collapsible(false).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            let Some((heading, text)) = section else {
                ui.label(tr("help-missing"));
                return;
            };
            ui.heading(heading);
            for block in blocks(text) {
                match block {
                    Block::Paragraph(p) => line(ui, &p),
                    Block::Item(marker, item) => {
                        ui.horizontal_top(|ui| {
                            ui.label(marker);
                            ui.vertical(|ui| line(ui, &item));
                        });
                    }
                }
                ui.add_space(4.0);
            }
        });
        ui.separator();
        if ui.link(tr("help-online")).clicked() {
            ctx.open_url(egui::OpenUrl::new_tab(crate::stack_tab::docs_url(&shown)));
        }
    });
    if !open {
        ctx.data_mut(|d| d.remove::<String>(id()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_runs_from_its_heading_to_the_next() {
        let page = "# Page\n\nIntro.\n\n## First one\n\nText.\n\n- an item\n\n## Last\n\nEnd.\n";
        assert_eq!(section(page, "first-one"), Some(("First one", "Text.\n\n- an item")));
        assert_eq!(section(page, "last"), Some(("Last", "End.")));
        assert_eq!(section(page, "page"), None, "only `## ` headings");
        assert_eq!(section(page, "missing"), None);
        assert_eq!(anchor("Out of focus"), "out-of-focus");
        assert_eq!(anchor("Match score!"), "match-score");
    }

    #[test]
    fn paragraphs_and_items_are_joined_lines() {
        let text = "One line\nand the next.\n\n1. **Frames.** Drop them\n   here.\n2. Align.\n\n- a\n- b\ncontinued";
        assert_eq!(
            blocks(text),
            [
                Block::Paragraph("One line and the next.".into()),
                Block::Item("1.".into(), "**Frames.** Drop them here.".into()),
                Block::Item("2.".into(), "Align.".into()),
                Block::Item("•".into(), "a".into()),
                Block::Item("•".into(), "b continued".into()),
            ]
        );
    }

    #[test]
    fn a_line_splits_into_its_pieces() {
        assert_eq!(
            spans("**Pyramid** picks, *gently*, `code` and [a link](#contrast-threshold) end"),
            [
                Span::Strong("Pyramid"),
                Span::Text(" picks, "),
                Span::Emphasis("gently"),
                Span::Text(", "),
                Span::Code("code"),
                Span::Text(" and "),
                Span::Link { text: "a link", target: "#contrast-threshold" },
                Span::Text(" end"),
            ]
        );
        // Markers that open nothing stay as text.
        for line in ["2 * 3 and 4 * 5", "a_b", "[not a link] **", "**"] {
            let pieces = spans(line);
            assert!(pieces.iter().all(|s| matches!(s, Span::Text(_))), "{line}: {pieces:?}");
            let text: String = pieces.iter().map(|s| if let Span::Text(t) = s { *t } else { "" }).collect();
            assert_eq!(text, line);
        }
        assert_eq!(spans(""), []);
    }
}
