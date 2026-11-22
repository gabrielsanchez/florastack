# Translating the app

The desktop app's strings are Fluent messages ([Project Fluent](https://projectfluent.org/)) in
`apps/florastack/locales/<language>/florastack.ftl`, compiled into the app. English (`en-US`) is the reference: every
message the app shows is there, and a test fails when the code uses a key the file lacks or the file has a message the
code never names.

## Adding a language

1. Copy `apps/florastack/locales/en-US/florastack.ftl` to `apps/florastack/locales/<tag>/florastack.ftl`, where
   `<tag>` is the language's BCP 47 tag (`de-DE`, `fr-FR`, `pt-BR`).
2. Translate the messages' values; keep the keys, the `{ $argument }` placeholders and the selectors' variants
   (`[one]`, `*[other]`), adding the variants your language's plural rules need.
3. Add a row to `LANGUAGES` in `apps/florastack/src/i18n.rs`, after English: the tag, the language's name in itself,
   and `include_str!` of the new file.
4. Run `cargo test -p florastack --lib i18n`: it checks that every file parses and has only messages the English file
   has.

A translation need not be complete: a message it lacks shows in English. It may also give a command's title, summary
and help text, which otherwise come from the engine's registry in English, as `command-<name>-title`,
`command-<name>-summary` and `command-<name>-help`, the name's dot written as a hyphen (`command-align-run-title`).

The language is chosen in **File > Preferences > Appearance** and takes effect at once; the app remembers it. Languages
written right to left get Unicode isolation marks around the values filled into messages.
