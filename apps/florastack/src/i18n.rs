//! The UI's strings, through Fluent (Project Fluent's `fluent-bundle`): every string the app shows comes from
//! `locales/<language>/florastack.ftl`, compiled in, English (`en-US`) alone for now ([`LANGUAGES`]). [`tr`] looks a
//! message up by its key, [`tr_args`] with arguments, in the language chosen ([`set_language`], which takes effect at
//! once) and else in English, so that a translation missing a message shows the English one; a key no file has shows
//! as itself, so a missing string is visible rather than blank. Tests check that the code uses no key the English file
//! lacks, that the file holds no message the code doesn't use, and that every translation parses and has only
//! messages the English file has. `book/src/dev/translating.md` says how to add a language.
//!
//! Commands' titles, summaries and help texts come from the engine's registry, in English; a translation can override
//! them with the messages `command-<name>-title`, `command-<name>-summary` and `command-<name>-help`
//! ([`command_title`], [`command_summary`], [`command_help`]), the name's dot written as a hyphen
//! (`command-align-run-title`).

use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// The English strings, compiled in.
pub(crate) const EN_US: &str = include_str!("../locales/en-US/florastack.ftl");

/// The languages the app has strings for: each one's tag, its name in itself, as the Preferences dialog lists them,
/// and its strings, compiled in. English comes first: it is the default, and what the others fall back on.
pub const LANGUAGES: [(&str, &str, &str); 1] = [("en-US", "English", EN_US)];

type Bundle = FluentBundle<FluentResource>;
/// A language other than English, by its tag, and its strings.
type Chosen = Option<(String, Arc<Bundle>)>;

/// A bundle of the strings `text` for language `tag`; a syntax error or a duplicate leaves out what it spoils, with a
/// warning.
fn bundle_of(tag: &str, text: &str) -> Bundle {
    let id: LanguageIdentifier = tag.parse().unwrap_or_default();
    let mut bundle = FluentBundle::new_concurrent(vec![id]);
    // Isolation marks around arguments help right-to-left scripts, and show as boxes in some fonts: languages
    // written left to right have no need of them.
    bundle.set_use_isolating(is_right_to_left(tag));
    let resource = FluentResource::try_new(text.to_owned()).unwrap_or_else(|(resource, errors)| {
        tracing::warn!("the {tag} strings have {} syntax errors", errors.len());
        resource
    });
    if let Err(errors) = bundle.add_resource(resource) {
        tracing::warn!("the {tag} strings have {} duplicate messages", errors.len());
    }
    bundle
}

/// Whether language `tag` is written right to left.
fn is_right_to_left(tag: &str) -> bool {
    let language = tag.split(['-', '_']).next().unwrap_or_default();
    ["ar", "fa", "he", "ur", "ps", "yi", "dv", "ckb", "sd", "ug"].contains(&language)
}

/// The English bundle, which every other falls back on.
fn english() -> &'static Bundle {
    static ENGLISH: OnceLock<Bundle> = OnceLock::new();
    ENGLISH.get_or_init(|| bundle_of("en-US", EN_US))
}

/// The language chosen, when it isn't English: its tag and its bundle.
fn chosen() -> &'static RwLock<Chosen> {
    static CHOSEN: OnceLock<RwLock<Chosen>> = OnceLock::new();
    CHOSEN.get_or_init(|| RwLock::new(None))
}

/// Shows the UI in language `tag`, one of [`LANGUAGES`], from the next frame on; English when the app has no such
/// language, which it says by returning false.
pub fn set_language(tag: &str) -> bool {
    let Some((_, _, text)) = LANGUAGES.iter().find(|(t, ..)| *t == tag) else {
        use_strings("en-US", "");
        return false;
    };
    use_strings(tag, text);
    true
}

/// The language the UI is shown in.
pub fn language() -> String {
    chosen().read().unwrap_or_else(PoisonError::into_inner).as_ref().map_or_else(|| LANGUAGES[0].0.to_owned(), |(tag, _)| tag.clone())
}

/// Shows the UI in `tag` with the strings `text`, falling back on English; English alone for `en-US`.
fn use_strings(tag: &str, text: &str) {
    let bundle = (tag != LANGUAGES[0].0).then(|| (tag.to_owned(), Arc::new(bundle_of(tag, text))));
    *chosen().write().unwrap_or_else(PoisonError::into_inner) = bundle;
}

/// The message `key` with `args`: the chosen language's, or English, or the key itself when neither has it.
fn format(key: &str, args: Option<&FluentArgs<'_>>) -> String {
    let translated = chosen().read().unwrap_or_else(PoisonError::into_inner).as_ref().map(|(_, b)| Arc::clone(b));
    let bundles = translated.iter().map(|b| &**b).chain(std::iter::once(english()));
    for bundle in bundles {
        let Some(pattern) = bundle.get_message(key).and_then(|m| m.value()) else { continue };
        let mut errors = Vec::new();
        let text = bundle.format_pattern(pattern, args, &mut errors);
        if !errors.is_empty() {
            tracing::debug!("the string {key} has {} formatting errors", errors.len());
        }
        return text.into_owned();
    }
    key.to_owned()
}

/// Whether the chosen language or English has message `key`.
fn has(key: &str) -> bool {
    let translated = chosen().read().unwrap_or_else(PoisonError::into_inner).as_ref().map(|(_, b)| Arc::clone(b));
    translated.is_some_and(|b| b.has_message(key)) || english().has_message(key)
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

/// A command's message `kind` (`title`, `summary` or `help`) from the bundle, if a translation has one.
fn command_text(name: &str, kind: &str) -> Option<String> {
    let key = format!("command-{}-{kind}", name.replace('.', "-"));
    has(&key).then(|| format(&key, None))
}

/// A command's title: the translation's, or the registry's English.
pub fn command_title(name: &str, registry: &str) -> String {
    command_text(name, "title").unwrap_or_else(|| registry.to_owned())
}

/// A command's summary: the translation's, or the registry's English.
pub fn command_summary(name: &str, registry: &str) -> String {
    command_text(name, "summary").unwrap_or_else(|| registry.to_owned())
}

/// A command's help text, which the command palette shows: the translation's, or the registry's English.
pub fn command_help(name: &str, registry: &str) -> String {
    command_text(name, "help").unwrap_or_else(|| registry.to_owned())
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
        const PREFIXES: [&str; 15] = [
            "adjust-",
            "capture-",
            "tip-",
            "setting-",
            "align-",
            "framing-",
            "tone-",
            "decisions-",
            "brush-",
            "export-",
            "prefs-",
            "preset-",
            "compression-",
            "subsampling-",
            "dng-look-",
        ];
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
    fn every_translation_parses_and_has_only_the_english_messages() {
        let english = defined_keys();
        for (tag, name, text) in LANGUAGES {
            assert!(tag.parse::<LanguageIdentifier>().is_ok(), "{tag} isn't a language tag");
            assert!(!name.is_empty(), "{tag} has no name");
            assert!(FluentResource::try_new(text.to_owned()).is_ok(), "the {tag} strings have syntax errors");
            let keys: BTreeSet<String> = text
                .lines()
                .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
                .filter_map(|l| l.split_once('=').map(|(id, _)| id.trim().to_owned()))
                .collect();
            let extra: Vec<&String> = keys.iter().filter(|k| !english.contains(*k) && !k.starts_with("command-")).collect();
            assert!(extra.is_empty(), "{tag} has messages English doesn't: {extra:?}");
        }
    }

    #[test]
    fn a_language_takes_effect_at_once_and_falls_back_on_english() {
        // A translation of one message and one command's title, which no other test reads: tests run side by side.
        use_strings("de-DE", "test-only-greeting = Hallo { $name }\ncommand-test-only-title = Nur ein Test\n");
        assert_eq!(language(), "de-DE");
        assert_eq!(tr_args("test-only-greeting", &[("name", FluentValue::from("Welt"))]), "Hallo Welt");
        assert_eq!(command_title("test.only", "Only a test"), "Nur ein Test");
        assert_eq!(command_summary("test.only", "Tests."), "Tests.", "English where it has none");
        assert_eq!(tr("action-undo"), english_of("action-undo"), "English for a message it lacks");
        assert!(!set_language("xx-XX"), "a language the app doesn't have");
        assert_eq!(language(), "en-US");
        assert_eq!(tr("test-only-greeting"), "test-only-greeting");
        assert!(set_language("en-US"));
        assert!(is_right_to_left("ar-EG") && !is_right_to_left("de"));
    }

    /// Message `key` in English, read from the English bundle alone.
    fn english_of(key: &str) -> String {
        let bundle = english();
        let mut errors = Vec::new();
        bundle.format_pattern(bundle.get_message(key).unwrap().value().unwrap(), None, &mut errors).into_owned()
    }

    #[test]
    fn arguments_are_filled_in_and_unknown_keys_show_as_themselves() {
        assert_eq!(tr_args("action-undo-named", &[("what", FluentValue::from("Rename project"))]), "Undo Rename project");
        assert_eq!(tr("no-such-key"), "no-such-key");
        assert_eq!(command_title("align.run", "Align frames"), "Align frames", "the registry's English is the fallback");
        assert_eq!(command_help("align.run", "Aligns the frames."), "Aligns the frames.");
    }
}
