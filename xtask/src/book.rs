//! `cargo xtask book`: builds the user and developer guide, the mdBook in `book/`, into `target/book/`, then checks
//! every link in it and in the repository's other Markdown files with lychee: links between pages and to their
//! headings, and links to the repository's own files. The pages write those as GitHub URLs, so that they work in the
//! published book too; lychee maps them onto the checkout's files (`--remap`) and checks those, with no network
//! request. Links to other sites are checked only with `--online`, so that a site that is down for a minute can't
//! fail CI. `--update-reference` writes the engine-command reference again, from the registry, by running the test
//! that checks it (`crates/engine/tests/reference.rs`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// The mdBook release CI builds the book with (`.github/workflows/ci.yml`).
pub const MDBOOK: &str = "0.5.4";
/// The lychee release CI checks the links with.
pub const LYCHEE: &str = "0.24.2";

pub const USAGE: &str = "\
usage: cargo xtask book [--online] [--update-reference]

Builds the user and developer guide (book/, an mdBook) into target/book/ and checks every link in it and in the
repository's Markdown files with lychee: links between pages, to their headings, and to the repository's own files
(written as GitHub URLs, checked against this checkout's files). Links to other sites are checked with --online only.

  --online             check links to other sites too
  --update-reference   write the engine-command reference (book/src/reference/commands.md) from the registry, after
                       a command changed, by running crates/engine/tests/reference.rs
  -h, --help           show this message

It needs mdBook 0.5.4 and lychee 0.24.2, the versions CI uses, on PATH:
  cargo install --locked mdbook --version 0.5.4
  cargo install --locked lychee --version 0.24.2
or their release builds (https://github.com/rust-lang/mdBook/releases, https://github.com/lycheeverse/lychee/releases).
";

/// The files whose links are checked besides the book's pages, as globs relative to the workspace root (lychee expands
/// them): the repository's Markdown, the book's sources (read on GitHub too), the notices and the issue forms.
const SOURCES: &[&str] = &["*.md", "NOTICE", "docs/**/*.md", "book/src/**/*.md", "corpus/README.md", "packaging/**/*.md", ".github/**/*.yml"];

/// Sites that `--online` doesn't check, with why: their links are checked by hand.
const UNCHECKABLE: &[(&str, &str)] = &[(r"^https://www\.usa\.canon\.com/", "answers every automated request with 403 Forbidden")];

/// The book's pages that aren't checked: the print page repeats every chapter with its links rewritten, and the 404
/// page's links are relative to the site's root, which a local build has not.
const SKIPPED_PAGES: &[&str] = &["print\\.html$", "404\\.html$"];

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    online: bool,
    update_reference: bool,
    help: bool,
}

fn parse_args(args: &[&str]) -> Result<Options, String> {
    let mut options = Options::default();
    for arg in args {
        match *arg {
            "--online" => options.online = true,
            "--update-reference" => options.update_reference = true,
            "-h" | "--help" => options.help = true,
            other => return Err(format!("unknown argument `{other}`\n\n{USAGE}")),
        }
    }
    Ok(options)
}

/// `cargo xtask book`, for the workspace that `meta` (the output of `cargo metadata`) describes.
pub fn run(meta: &Value, args: &[&str]) -> Result<(), String> {
    let options = parse_args(args)?;
    if options.help {
        print!("{USAGE}");
        return Ok(());
    }
    let root = PathBuf::from(meta["workspace_root"].as_str().ok_or("cargo metadata: no workspace_root")?);
    if options.update_reference {
        let status = crate::cargo()
            .args(["test", "-p", "florastack-engine", "--test", "reference", "--locked"])
            .env("FLORASTACK_UPDATE_REFERENCE", "1")
            .current_dir(&root)
            .status()
            .map_err(|e| format!("cannot run cargo: {e}"))?;
        if !status.success() {
            return Err(format!("writing the reference failed: {status}"));
        }
        eprintln!("book: wrote book/src/reference/commands.md");
        return Ok(());
    }
    let target = meta["target_directory"].as_str().map_or_else(|| root.join("target"), PathBuf::from);
    let out = target.join("book");
    let repository = repository(meta)?;
    check_tool("mdbook", MDBOOK)?;
    check_tool("lychee", LYCHEE)?;

    eprintln!("book: building book/ into {}", out.display());
    let status = Command::new("mdbook")
        .arg("build")
        .arg(root.join("book"))
        .arg("--dest-dir")
        .arg(&out)
        .status()
        .map_err(|e| format!("cannot run mdbook: {e}"))?;
    if !status.success() {
        return Err(format!("mdbook build failed: {status}"));
    }

    eprintln!("book: checking the links{}", if options.online { ", other sites' too" } else { " (--online checks other sites' too)" });
    let status = Command::new("lychee")
        .args(lychee_args(&root, &out, &repository, options.online))
        .current_dir(&root)
        .status()
        .map_err(|e| format!("cannot run lychee: {e}"))?;
    if !status.success() {
        return Err(format!("lychee found broken links ({status}); the list is above"));
    }
    Ok(())
}

/// The repository's URL, from the workspace's packages (`workspace.package.repository`), without a trailing slash.
fn repository(meta: &Value) -> Result<String, String> {
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|p| p["repository"].as_str())
        .map(|r| r.trim_end_matches('/').to_owned())
        .ok_or_else(|| "no workspace package names its repository".to_owned())
}

/// Checks that `program` runs, and says so when its version isn't the one CI uses.
fn check_tool(program: &str, version: &str) -> Result<(), String> {
    let output = Command::new(program).arg("--version").stdin(Stdio::null()).output();
    let installed = match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        _ => {
            return Err(format!(
                "{program} is not installed, or not on PATH: `cargo install --locked {program} --version {version}`, or a release build \
                 (`cargo xtask book --help`)"
            ));
        }
    };
    if !installed.split_whitespace().any(|word| word.trim_start_matches('v') == version) {
        eprintln!("book: warning: {installed} found; CI uses {program} {version}, whose results may differ");
    }
    Ok(())
}

/// `\` and the characters a regular expression gives a meaning to, escaped.
fn regex_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// A local folder as a `file://` URL ending in `/`: `file:///C:/work/florastack/` on Windows, `file:///work/...`
/// elsewhere. Spaces are escaped; the workspace's path has no other character a URL can't hold.
fn folder_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).replace(' ', "%20");
    let slash = if text.starts_with('/') { "" } else { "/" };
    format!("file://{slash}{}/", text.trim_end_matches('/'))
}

/// lychee's arguments: the book's pages in `out` and the [`SOURCES`], the repository's own GitHub URLs mapped onto the
/// files of the checkout at `root` (a file's view, its folder's and its edit page, on `main`) and its other URLs
/// (issues, releases) left out, and every request blocked unless `online`.
fn lychee_args(root: &Path, out: &Path, repository: &str, online: bool) -> Vec<String> {
    let repo = regex_escape(repository);
    let mut args: Vec<String> = vec![
        "--no-progress".into(),
        "--include-fragments".into(),
        "--remap".into(),
        format!("^{repo}/(?:blob|tree|edit)/main/ {}", folder_url(root)),
        "--exclude".into(),
        format!("^{repo}(?:[/?#]|$)"),
    ];
    for page in SKIPPED_PAGES {
        args.extend(["--exclude-path".to_owned(), (*page).to_owned()]);
    }
    if online {
        // A site that limits the rate of requests (429) is there, and its page most likely too.
        args.extend(["--max-retries", "3", "--retry-wait-time", "2", "--timeout", "30", "--accept", "100..=103,200..=299,429"].map(str::to_owned));
        for (site, _why) in UNCHECKABLE {
            args.extend(["--exclude".to_owned(), (*site).to_owned()]);
        }
    } else {
        args.push("--offline".into());
    }
    args.push(format!("{}/**/*.html", out.to_string_lossy().replace('\\', "/")));
    args.extend(SOURCES.iter().map(|s| (*s).to_owned()));
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        assert_eq!(parse_args(&[]).unwrap(), Options::default());
        let o = parse_args(&["--online", "--update-reference"]).unwrap();
        assert!(o.online && o.update_reference && !o.help);
        assert!(parse_args(&["-h"]).unwrap().help);
        assert!(parse_args(&["--offline"]).unwrap_err().contains("--offline"));
    }

    #[test]
    fn the_repositorys_links_are_checked_against_the_checkout() {
        let args = lychee_args(Path::new("/work/flora stack"), Path::new("/work/flora stack/target/book"), "https://github.com/o/r", false);
        let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(at("--remap"), Some(r"^https://github\.com/o/r/(?:blob|tree|edit)/main/ file:///work/flora%20stack/"));
        assert_eq!(at("--exclude"), Some(r"^https://github\.com/o/r(?:[/?#]|$)"));
        assert!(args.contains(&"--offline".to_owned()) && args.contains(&"--include-fragments".to_owned()));
        assert!(args.contains(&"/work/flora stack/target/book/**/*.html".to_owned()));
        assert!(args.contains(&"book/src/**/*.md".to_owned()));
        assert!(!lychee_args(Path::new("/w"), Path::new("/w/b"), "https://github.com/o/r", true).contains(&"--offline".to_owned()));
        assert_eq!(folder_url(Path::new(r"C:\work\florastack")), "file:///C:/work/florastack/");
        assert_eq!(folder_url(Path::new(r"\\?\C:\work\florastack\")), "file:///C:/work/florastack/");
        assert_eq!(regex_escape("a.b+c(d)"), r"a\.b\+c\(d\)");
    }

    #[test]
    fn every_chapter_is_in_the_summary() {
        let src = crate::workspace_root().unwrap().join("book/src");
        let summary = std::fs::read_to_string(src.join("SUMMARY.md")).unwrap();
        let mut pending = vec![src.clone()];
        let mut chapters = 0;
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|e| e == "md") && path.file_name().is_some_and(|n| n != "SUMMARY.md") {
                    let relative = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
                    assert!(summary.contains(&format!("]({relative})")), "book/src/{relative} isn't in book/src/SUMMARY.md");
                    chapters += 1;
                }
            }
        }
        assert!(chapters > 20, "{chapters} chapters");
    }

    #[test]
    fn the_crate_diagram_names_every_crate() {
        let svg = include_str!("../../book/src/images/crates.svg");
        for (name, _) in crate::layers::RULES.allowed {
            assert!(svg.contains(&format!(">{name}<")), "book/src/images/crates.svg doesn't show {name}: draw it in with its dependencies");
        }
    }
}
