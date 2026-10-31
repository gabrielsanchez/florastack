//! The UI's strings, through Fluent (Project Fluent's `fluent-bundle`): every string the app shows comes from
//! `locales/<language>/florastack.ftl`, English (`en-US`) for now. [`tr`] looks a message up by its key, [`tr_args`]
//! with arguments. A key the bundle lacks shows as itself, so a missing string is visible rather than blank; a test
//! checks that the code uses no key the file lacks, and the file holds no message the code doesn't use.
//!
//! Commands' titles and summaries come from the engine's registry, in English; a translation can override them with
//! the messages `command-<name>-title` and `command-<name>-summary` ([`command_title`], [`command_summary`]), the
//! name's dot written as a hyphen (`command-align-run-title`).

use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use unic_langid::langid;

/// The English strings, compiled in.
pub(crate) const EN_US: &str = include_str!("../locales/en-US/florastack.ftl");

fn bundle() -> &'static FluentBundle<FluentResource> {
    static BUNDLE: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let mut bundle = FluentBundle::new_concurrent(vec![langid!("en-US")]);
        // Isolation marks around arguments help right-to-left scripts, and show as boxes in some fonts: English has
        // no need of them.
        bundle.set_use_isolating(false);
        let resource = FluentResource::try_new(EN_US.to_owned()).unwrap_or_else(|(resource, errors)| {
            tracing::warn!("the English strings have {} syntax errors", errors.len());
            resource
        });
        if let Err(errors) = bundle.add_resource(resource) {
            tracing::warn!("the English strings have {} duplicate messages", errors.len());
        }
        bundle
    })
}

/// The message `key` with `args`, or the key itself when there is none.
fn format(key: &str, args: Option<&FluentArgs<'_>>) -> String {
    let bundle = bundle();
    let Some(pattern) = bundle.get_message(key).and_then(|m| m.value()) else { return key.to_owned() };
    let mut errors = Vec::new();
    let text = bundle.format_pattern(pattern, args, &mut errors);
    if !errors.is_empty() {
        tracing::debug!("the string {key} has {} formatting errors", errors.len());
    }
    text.into_owned()
}

/// The message `key`. See the module documentation.
pub fn tr(key: &str) -> String {
    format(key, None)
}

/// The message `key` with named arguments.
pub fn tr_args(key: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, value.clone());
    }
    format(key, Some(&fluent))
}

/// A command's message `kind` (`title` or `summary`) from the bundle, if a translation has one.
fn command_text(name: &str, kind: &str) -> Option<String> {
    let key = format!("command-{}-{kind}", name.replace('.', "-"));
    bundle().get_message(&key).and_then(|m| m.value()).map(|_| format(&key, None))
}

/// A command's title: the translation's, or the registry's English.
pub fn command_title(name: &str, registry: &str) -> String {
    command_text(name, "title").unwrap_or_else(|| registry.to_owned())
}

/// A command's summary: the translation's, or the registry's English.
pub fn command_summary(name: &str, registry: &str) -> String {
    command_text(name, "summary").unwrap_or_else(|| registry.to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    /// The source files' text, but this one's (whose tests name keys that don't exist).
    fn sources() -> Vec<String> {
        let mut texts = Vec::new();
        let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("i18n.rs") {
                    texts.push(std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        texts
    }

    /// The keys the sources pass to `tr` and `tr_args` as string literals.
    fn called_keys(sources: &[String]) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for text in sources {
            for call in ["tr(\"", "tr_args(\""] {
                for piece in text.split(call).skip(1) {
                    if let Some(end) = piece.find('"') {
                        keys.insert(piece[..end].to_owned());
                    }
                }
            }
        }
        keys
    }

    /// The string literals that name a key by its prefix: the settings' labels and tooltips, which pass through helpers
    /// on their way to `tr`.
    fn prefixed(sources: &[String]) -> BTreeSet<String> {
        const PREFIXES: [&str; 7] = ["tip-", "setting-", "align-", "framing-", "tone-", "decisions-", "brush-"];
        let key = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        sources
            .iter()
            .flat_map(|text| text.split('"').skip(1).step_by(2))
            .filter(|text| key(text) && PREFIXES.iter().any(|p| text.starts_with(p)))
            .map(str::to_owned)
            .collect()
    }

    /// The message ids the English file defines.
    fn defined_keys() -> BTreeSet<String> {
        EN_US
            .lines()
            .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
            .filter_map(|l| l.split_once('=').map(|(id, _)| id.trim().to_owned()))
            .collect()
    }

    #[test]
    fn the_english_strings_parse() {
        assert!(FluentResource::try_new(EN_US.to_owned()).is_ok(), "florastack.ftl has syntax errors");
    }

    #[test]
    fn every_key_the_code_uses_has_a_string_and_every_string_is_used() {
        let sources = sources();
        let defined = defined_keys();
        let mut used = called_keys(&sources);
        used.extend(crate::actions::Action::ALL.iter().map(|a| a.label_key().to_owned()));
        used.extend(crate::prefs::Tab::ALL.iter().map(|t| t.label_key().to_owned()));
        used.extend(crate::prefs::Method::ALL.iter().map(|m| m.label_key().to_owned()));
        used.extend(prefixed(&sources));
        let missing: Vec<&String> = used.difference(&defined).collect();
        assert!(missing.is_empty(), "keys without a string in florastack.ftl: {missing:?}");
        // A key can also be named where it is chosen (a label's, a setting's) and passed to `tr` later.
        let unused: Vec<&String> = defined.iter().filter(|key| !sources.iter().any(|t| t.contains(&format!("\"{key}\"")))).collect();
        assert!(unused.is_empty(), "strings in florastack.ftl that the code never names: {unused:?}");
    }

    #[test]
    fn arguments_are_filled_in_and_unknown_keys_show_as_themselves() {
        assert_eq!(tr_args("action-undo-named", &[("what", FluentValue::from("Rename project"))]), "Undo Rename project");
        assert_eq!(tr("no-such-key"), "no-such-key");
        assert_eq!(command_title("align.run", "Align frames"), "Align frames", "the registry's English is the fallback");
    }
}
