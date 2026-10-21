//! FloraStack's product identity, read from `branding.toml` at the repository root. florastack-core's build script reads
//! the same file into Rust constants with the same strict schema, so an unknown or missing key fails in both places.

use std::path::Path;

use serde::Deserialize;

/// The contents of `branding.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branding {
    pub display_name: String,
    pub project_extension: String,
    pub app_id: String,
    pub binaries: Binaries,
    pub xmp: Xmp,
}

/// Executable names of the apps.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binaries {
    pub gui: String,
    pub cli: String,
}

/// The XMP namespace of the properties FloraStack writes into its output files.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Xmp {
    pub namespace: String,
    pub prefix: String,
}

impl Branding {
    /// Every executable name the file declares.
    pub fn binary_names(&self) -> [&str; 2] {
        [&self.binaries.gui, &self.binaries.cli]
    }
}

/// Reads `branding.toml` from the workspace root.
pub fn load(workspace_root: &Path) -> Result<Branding, String> {
    let path = workspace_root.join("branding.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(text: &str) -> Result<Branding, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// The repository's own `branding.toml`, for tests.
#[cfg(test)]
pub fn repository() -> Branding {
    parse(include_str!("../../branding.toml")).expect("branding.toml parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_file_is_well_formed() {
        let branding = repository();
        assert!(branding.project_extension.starts_with('.'), "the project extension includes its dot");
        assert_ne!(branding.binaries.gui, branding.binaries.cli);
        // An XML namespace name: an absolute URI, and a prefix that is an XML name without a colon.
        assert!(branding.xmp.namespace.starts_with("https://") && branding.xmp.namespace.ends_with('/'), "{}", branding.xmp.namespace);
        let prefix = &branding.xmp.prefix;
        assert!(prefix.starts_with(|c: char| c.is_ascii_lowercase()) && prefix.chars().all(|c| c.is_ascii_alphanumeric()), "{prefix}");
    }

    #[test]
    fn unknown_and_missing_keys_are_rejected() {
        let valid = "display_name = \"A\"\nproject_extension = \".a\"\napp_id = \"io.github.x.A\"\n\n[binaries]\ngui = \"a\"\ncli = \"a-cli\"\n\n\
                     [xmp]\nnamespace = \"https://example.org/a/\"\nprefix = \"a\"\n";
        assert!(parse(valid).is_ok());
        assert!(parse(&format!("version = 1\n{valid}")).is_err(), "unknown top-level key");
        assert!(parse(&valid.replace("cli = \"a-cli\"\n", "cli = \"a-cli\"\nweb = \"a-web\"\n")).is_err(), "unknown binary");
        assert!(parse(&valid.replace("app_id = \"io.github.x.A\"\n", "")).is_err(), "missing key");
        assert!(parse(&valid.replace("prefix = \"a\"\n", "")).is_err(), "missing XMP prefix");
    }
}
