//! `THIRD-PARTY-LICENSES.txt`: the license texts of the crates compiled into a package, which every package carries
//! next to FloraStack's own LICENSE-MIT, LICENSE-APACHE and NOTICE.
//!
//! The crates are those `cargo metadata` resolves for the package's targets and features, reached from the packages
//! that build its binaries through normal dependencies (build dependencies run on the build machine and ship
//! nothing). A crate's texts are the license files at the top of its package, or of its git repository; those
//! `packaging/licenses/crates.toml` names; or, for a crate that ships none, the standard text of a license it
//! offers. A crate none of these covers fails the build, so that no dependency ships without its notice. Identical
//! texts are written once, with every crate that ships them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// The file's name in the packages.
pub const FILE: &str = "THIRD-PARTY-LICENSES.txt";

/// Where `crates.toml` and the standard texts are, below the workspace root.
const CONFIG: &str = "packaging/licenses";

/// The licenses whose standard text a crate that ships none is given, in the order they are preferred when it offers
/// several: Apache-2.0 and BSL-1.0 need no copyright line in their text, MIT does (from the crate's authors).
const STANDARD: [(&str, &str); 3] =
    [("Apache-2.0", "LICENSE-APACHE"), ("BSL-1.0", "packaging/licenses/BSL-1.0.txt"), ("MIT", "packaging/licenses/MIT.txt")];

/// What a package contains: the packages that build its binaries, for which Rust targets (both of a universal macOS
/// build), with which features (`package/feature`, as `cargo build --features` takes them).
pub struct Scope<'a> {
    /// What the file is about, in its first line: `FloraStack 0.8.0, windows-x64`.
    pub title: String,
    pub roots: Vec<&'a str>,
    pub triples: Vec<&'a str>,
    pub features: Option<String>,
}

/// Where a crate's texts are, when not at the top of its package (`packaging/licenses/crates.toml`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Place {
    /// Files in the crate's package, relative to its folder.
    files: Option<Vec<String>>,
    /// Another crate, of the same repository, whose top-level license files are this one's.
    from: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Text {
    /// Where it comes from: `LICENSE-MIT`, `LICENSE (of alloc-no-stdlib 2.0.4)`.
    label: String,
    body: String,
}

#[derive(Debug)]
struct Crate {
    name: String,
    version: String,
    license: String,
    /// The repository the crate names, or its git source.
    source: String,
    texts: Vec<Text>,
}

/// Writes the notices of `scope` into `path`, and returns how many crates it lists.
pub fn write(root: &Path, scope: &Scope, path: &Path) -> Result<usize, String> {
    let crates = gather(root, scope)?;
    super::write(path, render(&scope.title, &crates).as_bytes())?;
    Ok(crates.len())
}

/// Every crate of the scope with its texts, sorted by name and version.
fn gather(root: &Path, scope: &Scope) -> Result<Vec<Crate>, String> {
    let places = load_places(root)?;
    let mut crates: BTreeMap<(String, String), Crate> = BTreeMap::new();
    for triple in &scope.triples {
        let meta = metadata(root, triple, scope.features.as_deref())?;
        let packages = meta["packages"].as_array().ok_or("cargo metadata lists no packages")?;
        for id in shipped(&meta, &scope.roots)? {
            let package = packages.iter().find(|p| p["id"] == id).ok_or_else(|| format!("cargo metadata doesn't describe {id}"))?;
            let name = string(package, "name")?;
            let version = string(package, "version")?;
            if crates.contains_key(&(name.clone(), version.clone())) {
                continue;
            }
            let texts = texts(root, package, packages, &places).map_err(|e| format!("{name} {version}: {e}"))?;
            let license = package["license"].as_str().unwrap_or("(none declared)").to_owned();
            let source = package["repository"]
                .as_str()
                .or_else(|| package["source"].as_str().filter(|s| s.starts_with("git+")))
                .unwrap_or_default()
                .to_owned();
            crates.insert((name.clone(), version.clone()), Crate { name, version, license, source, texts });
        }
    }
    Ok(crates.into_values().collect())
}

fn string(package: &Value, field: &str) -> Result<String, String> {
    package[field].as_str().map(str::to_owned).ok_or_else(|| format!("a package has no {field}"))
}

fn metadata(root: &Path, triple: &str, features: Option<&str>) -> Result<Value, String> {
    let mut cargo = crate::cargo();
    cargo.current_dir(root).args(["metadata", "--format-version", "1", "--locked", "--filter-platform", triple]);
    if let Some(features) = features {
        cargo.args(["--features", features]);
    }
    let text = super::output(&mut cargo, &format!("list the crates built for {triple}"))?;
    serde_json::from_str(&text).map_err(|e| format!("cargo metadata printed something that isn't JSON: {e}"))
}

/// The ids of the packages that `roots` reach through normal dependencies, the workspace's own left out.
fn shipped(meta: &Value, roots: &[&str]) -> Result<BTreeSet<String>, String> {
    let members: BTreeSet<&str> =
        meta["workspace_members"].as_array().ok_or("cargo metadata lists no workspace members")?.iter().filter_map(Value::as_str).collect();
    let nodes: BTreeMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata resolved no dependencies")?
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();
    let packages = meta["packages"].as_array().ok_or("cargo metadata lists no packages")?;
    let mut todo = Vec::new();
    for name in roots {
        let id = packages
            .iter()
            .find_map(|p| p["id"].as_str().filter(|id| p["name"] == *name && members.contains(id)))
            .ok_or_else(|| format!("the workspace has no package `{name}`"))?;
        todo.push(id);
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = nodes.get(id).ok_or_else(|| format!("cargo metadata didn't resolve {id}"))?;
        for dep in node["deps"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let normal = dep["dep_kinds"].as_array().is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()));
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                todo.push(pkg);
            }
        }
    }
    Ok(seen.into_iter().filter(|id| !members.contains(id)).map(str::to_owned).collect())
}

fn load_places(root: &Path) -> Result<BTreeMap<String, Place>, String> {
    let path = root.join(CONFIG).join("crates.toml");
    let places: BTreeMap<String, Place> = toml::from_str(&super::read_text(&path)?).map_err(|e| format!("{}: {e}", path.display()))?;
    for (name, place) in &places {
        if place.files.is_some() == place.from.is_some() {
            return Err(format!("{}: [{name}] needs either files or from", path.display()));
        }
    }
    Ok(places)
}

/// A crate's license texts.
fn texts(root: &Path, package: &Value, packages: &[Value], places: &BTreeMap<String, Place>) -> Result<Vec<Text>, String> {
    let dir = package_dir(package)?;
    let name = string(package, "name")?;
    if let Some(place) = places.get(&name) {
        if let Some(files) = &place.files {
            return files.iter().map(|file| read(&dir.join(file), file.clone())).collect();
        }
        if let Some(from) = &place.from {
            let other = packages
                .iter()
                .find(|p| p["name"] == from.as_str())
                .ok_or_else(|| format!("crates.toml takes its texts from {from}, which the lock file doesn't have"))?;
            let other_version = string(other, "version")?;
            let found = license_files(&package_dir(other)?)?;
            if found.is_empty() {
                return Err(format!("crates.toml takes its texts from {from} {other_version}, which ships none"));
            }
            return found.into_iter().map(|(file, path)| read(&path, format!("{file} (of {from} {other_version})"))).collect();
        }
    }
    let mut found = license_files(&dir)?;
    // A crate of a git repository may keep its texts at the repository's top, which Cargo's checkout marks with
    // `.cargo-ok`.
    if found.is_empty() && package["source"].as_str().is_some_and(|s| s.starts_with("git+")) {
        let mut up = dir.as_path();
        while !up.join(".cargo-ok").is_file() {
            let Some(parent) = up.parent() else { break };
            up = parent;
            found = license_files(up)?.into_iter().map(|(file, path)| (format!("{file} (of its repository)"), path)).collect();
            if !found.is_empty() {
                break;
            }
        }
    }
    if !found.is_empty() {
        return found.into_iter().map(|(file, path)| read(&path, file)).collect();
    }
    let license = package["license"].as_str().ok_or("it ships no license text and declares no license: list it in packaging/licenses/crates.toml")?;
    let available: Vec<&str> = STANDARD.iter().map(|(id, _)| *id).collect();
    let chosen = choose(license, &available).ok_or_else(|| {
        format!("it ships no license text, and `{license}` offers none of {}: list it in packaging/licenses/crates.toml", available.join(", "))
    })?;
    let authors: Vec<String> = package["authors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(|author| author.split_once('<').map_or(author, |(name, _)| name).trim().to_owned())
        .filter(|author| !author.is_empty())
        .collect();
    let copyright = if authors.is_empty() { format!("Copyright (c) the {name} authors") } else { format!("Copyright (c) {}", authors.join(", ")) };
    chosen
        .iter()
        .map(|id| {
            let file = STANDARD.iter().find(|(standard, _)| standard == id).map(|(_, file)| *file).unwrap_or_default();
            let text = super::fill(&super::read_template(&root.join(file))?, &[("COPYRIGHT", &copyright)])?;
            Ok(Text { label: format!("the standard {id} text, as it ships none"), body: tidy(&text) })
        })
        .collect()
}

fn package_dir(package: &Value) -> Result<PathBuf, String> {
    let manifest = package["manifest_path"].as_str().ok_or("a package has no manifest path")?;
    Path::new(manifest).parent().map(Path::to_path_buf).ok_or_else(|| format!("{manifest} has no folder"))
}

fn read(path: &Path, label: String) -> Result<Text, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(Text { label, body: tidy(&String::from_utf8_lossy(&bytes)) })
}

/// Unix line endings, no blank lines around, no trailing spaces.
fn tidy(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let start = lines.iter().position(|line| !line.is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|line| !line.is_empty()).map_or(start, |i| i + 1);
    lines[start..end].join("\n")
}

/// The license and notice files at the top of a folder, by name.
fn license_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_license_file(&name) && entry.file_type().is_ok_and(|t| t.is_file()) {
            found.push((name, entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

/// `LICENSE`, `LICENSE-MIT`, `LICENCE.txt`, `COPYING`, `NOTICE`, `COPYRIGHT`, `UNLICENSE` and the like, but not code
/// or manifests that happen to share a name.
fn is_license_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let code =
        Path::new(&lower).extension().is_some_and(|ext| matches!(ext.to_str(), Some("rs" | "toml" | "json" | "lock" | "py" | "sh" | "yml" | "yaml")));
    !code && ["licen", "copying", "notice", "copyright", "unlicense"].iter().any(|start| lower.starts_with(start))
}

/// The licenses to give a crate whose declared `expression` (SPDX, or Cargo's old `MIT/Apache-2.0`) is satisfied by
/// the `available` texts: all of an AND, and of an OR the alternative with the fewest texts, then the most preferred
/// (earliest in `available`). `None` when nothing available satisfies it, or it can't be read.
fn choose(expression: &str, available: &[&str]) -> Option<Vec<String>> {
    let spaced = expression.replace('(', " ( ").replace(')', " ) ").replace('/', " OR ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut parser = Parser { tokens: &tokens, at: 0, available };
    let chosen = parser.or().ok()?;
    if parser.at == tokens.len() { chosen } else { None }
}

/// An expression that isn't SPDX.
struct Malformed;

/// The licenses chosen for part of an expression, or `None` when nothing available satisfies it.
type Parsed = Result<Option<Vec<String>>, Malformed>;

struct Parser<'a> {
    tokens: &'a [&'a str],
    at: usize,
    available: &'a [&'a str],
}

impl Parser<'_> {
    fn next_is(&self, word: &str) -> bool {
        self.tokens.get(self.at).is_some_and(|token| token.eq_ignore_ascii_case(word))
    }

    fn or(&mut self) -> Parsed {
        let mut options = vec![self.and()?];
        while self.next_is("OR") {
            self.at += 1;
            options.push(self.and()?);
        }
        let rank = |licenses: &Vec<String>| {
            let worst = licenses.iter().filter_map(|id| self.available.iter().position(|a| a == id)).max().unwrap_or(usize::MAX);
            (licenses.len(), worst)
        };
        Ok(options.into_iter().flatten().min_by_key(rank))
    }

    fn and(&mut self) -> Parsed {
        let mut all = self.atom()?;
        while self.next_is("AND") {
            self.at += 1;
            let more = self.atom()?;
            all = match (all, more) {
                (Some(mut all), Some(more)) => {
                    all.extend(more);
                    Some(all)
                }
                _ => None,
            };
        }
        Ok(all)
    }

    fn atom(&mut self) -> Parsed {
        let token = *self.tokens.get(self.at).ok_or(Malformed)?;
        self.at += 1;
        if token == "(" {
            let inner = self.or()?;
            if !self.next_is(")") {
                return Err(Malformed);
            }
            self.at += 1;
            return Ok(inner);
        }
        if [")", "AND", "OR", "WITH"].iter().any(|word| token.eq_ignore_ascii_case(word)) {
            return Err(Malformed);
        }
        if self.next_is("WITH") {
            // An exception changes the license's text: no standard text covers it.
            self.at += 2;
            if self.at > self.tokens.len() {
                return Err(Malformed);
            }
            return Ok(None);
        }
        Ok(self.available.contains(&token).then(|| vec![token.to_owned()]))
    }
}

/// The file: the crates, then each text once with the crates that ship it.
fn render(title: &str, crates: &[Crate]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Third-party licenses: {title}\n");
    out.push_str(&wrap(
        &format!(
            "The license of this software is in LICENSE-MIT, LICENSE-APACHE and NOTICE, next to this file. Its programs also contain the {} \
             crates (Rust libraries) listed below, each published by its authors under the license it declares, which the texts after the \
             list give. Each text is written once, with every crate that ships it.",
            crates.len()
        ),
        100,
    ));
    out.push_str("\n\nCRATES\n\n");
    let width = crates.iter().map(|c| c.name.len() + c.version.len() + 1).max().unwrap_or(0);
    let license_width = crates.iter().map(|c| c.license.len()).max().unwrap_or(0).min(48);
    for c in crates {
        let line = format!("{:<width$}  {:<license_width$}  {}", format!("{} {}", c.name, c.version), c.license, c.source);
        let _ = writeln!(out, "{}", line.trim_end());
    }
    // Each distinct text, in the order of the first crate that ships it, with the crates that do.
    let mut texts: Vec<(&str, Vec<String>)> = Vec::new();
    for c in crates {
        for text in &c.texts {
            let who = format!("{} {} ({})", c.name, c.version, text.label);
            match texts.iter_mut().find(|(body, _)| *body == text.body) {
                Some((_, users)) => users.push(who),
                None => texts.push((&text.body, vec![who])),
            }
        }
    }
    out.push_str("\n\nLICENSE TEXTS\n");
    let rule = "=".repeat(100);
    for (body, users) in texts {
        let _ = write!(out, "\n{rule}\n{}\n{rule}\n\n{body}\n", wrap(&users.join(", "), 100));
    }
    out
}

/// Words joined into lines of at most `width` characters (a longer word has a line of its own).
fn wrap(text: &str, width: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.len() + 1 + word.len() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVAILABLE: [&str; 3] = ["Apache-2.0", "BSL-1.0", "MIT"];

    fn chosen(expression: &str) -> Option<Vec<String>> {
        choose(expression, &AVAILABLE)
    }

    #[test]
    fn a_crate_without_texts_gets_one_license_it_offers() {
        assert_eq!(chosen("MIT"), Some(vec!["MIT".to_owned()]));
        assert_eq!(chosen("MIT OR Apache-2.0"), Some(vec!["Apache-2.0".to_owned()]));
        assert_eq!(chosen("MIT/Apache-2.0"), Some(vec!["Apache-2.0".to_owned()]));
        assert_eq!(chosen("Zlib OR Apache-2.0 OR MIT"), Some(vec!["Apache-2.0".to_owned()]));
        assert_eq!(chosen("Apache-2.0 AND MIT"), Some(vec!["Apache-2.0".to_owned(), "MIT".to_owned()]));
        assert_eq!(chosen("(MIT OR Apache-2.0) AND BSL-1.0"), Some(vec!["Apache-2.0".to_owned(), "BSL-1.0".to_owned()]));
        assert_eq!(chosen("BSD-3-Clause OR (MIT AND BSL-1.0)"), Some(vec!["MIT".to_owned(), "BSL-1.0".to_owned()]));
        // Nothing available, or a text an exception changes, or nonsense: no choice.
        assert_eq!(chosen("BSD-3-Clause"), None);
        assert_eq!(chosen("(MIT OR Apache-2.0) AND OFL-1.1"), None);
        assert_eq!(chosen("Apache-2.0 WITH LLVM-exception"), None);
        assert_eq!(chosen("Apache-2.0 WITH LLVM-exception OR MIT"), Some(vec!["MIT".to_owned()]));
        for bad in ["", "MIT OR", "(MIT", "MIT)", "AND MIT", "MIT Apache-2.0"] {
            assert_eq!(chosen(bad), None, "{bad}");
        }
    }

    #[test]
    fn license_files_are_told_from_code() {
        for name in ["LICENSE", "LICENSE-MIT", "license-apache", "LICENCE.txt", "COPYING", "NOTICE", "COPYRIGHT", "UNLICENSE", "LICENSE.Apache-2.0"] {
            assert!(is_license_file(name), "{name}");
        }
        for name in ["license.rs", "README.md", "Cargo.toml", "notices.json", "src"] {
            assert!(!is_license_file(name), "{name}");
        }
    }

    #[test]
    fn each_text_is_written_once_with_the_crates_that_ship_it() {
        let text = |label: &str, body: &str| Text { label: label.to_owned(), body: body.to_owned() };
        let crates = [
            Crate {
                name: "a".into(),
                version: "1.0.0".into(),
                license: "MIT".into(),
                source: "https://a".into(),
                texts: vec![text("LICENSE", "same")],
            },
            Crate {
                name: "b".into(),
                version: "2.0.0".into(),
                license: "MIT OR Apache-2.0".into(),
                source: String::new(),
                texts: vec![text("LICENSE-MIT", "same"), text("LICENSE-APACHE", "other")],
            },
        ];
        let out = render("App 1.0, test", &crates);
        assert!(out.starts_with("Third-party licenses: App 1.0, test\n"));
        assert!(out.contains("the 2 crates"));
        assert!(out.contains(&format!("a 1.0.0  {:<17}  https://a\nb 2.0.0  MIT OR Apache-2.0\n", "MIT")), "{out}");
        assert_eq!(out.matches("\nsame\n").count(), 1, "{out}");
        assert!(out.contains("a 1.0.0 (LICENSE), b 2.0.0 (LICENSE-MIT)\n"), "{out}");
        assert!(out.contains("b 2.0.0 (LICENSE-APACHE)\n"), "{out}");
        assert_eq!(tidy("\r\n\nline  \r\n\r\nend\r\n\n"), "line\n\nend");
        assert_eq!(wrap("aa bb cc", 5), "aa bb\ncc");
    }

    /// Every crate that a target's packages contain has its license texts: a new dependency that ships none, under
    /// a license with no standard text here, fails this test rather than a release.
    #[test]
    fn every_shipped_crate_has_its_texts() {
        let root = crate::workspace_root().unwrap();
        let scopes = [
            (vec!["florastack", "florastack-cli"], vec!["x86_64-pc-windows-msvc"], None),
            (
                vec!["florastack", "florastack-cli"],
                vec!["aarch64-apple-darwin", "x86_64-apple-darwin"],
                Some("florastack/gphoto,florastack-cli/gphoto"),
            ),
            (vec!["florastack", "florastack-cli"], vec!["x86_64-unknown-linux-gnu"], Some("florastack/gphoto,florastack-cli/gphoto")),
            (vec!["florastack-python"], vec!["aarch64-unknown-linux-gnu"], Some("florastack-python/extension-module")),
        ];
        for (roots, triples, features) in scopes {
            let scope = Scope { title: "test".to_owned(), roots, triples, features: features.map(str::to_owned) };
            let crates = gather(&root, &scope).unwrap();
            assert!(crates.len() > 100, "{} crates for {:?}", crates.len(), scope.triples);
            for c in &crates {
                assert!(!c.texts.is_empty() && c.texts.iter().all(|t| !t.body.is_empty()), "{} {} has no text", c.name, c.version);
            }
            let names: Vec<&str> = crates.iter().map(|c| c.name.as_str()).collect();
            assert!(names.contains(&"serde") && !names.contains(&"florastack-core"), "the workspace's own crates aren't listed");
            if scope.roots == ["florastack", "florastack-cli"] {
                assert!(names.contains(&"lightcraft-raw"), "the git crates are listed");
            }
        }
    }
}
