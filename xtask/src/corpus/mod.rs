//! `cargo xtask corpus`: the corpus of real focus stacks and format fixtures, in the gitignored `corpus/` folder.
//!
//! - `corpus/manifest.toml` (tracked) pins stacks to download: each file with its URL, SHA-256 and size.
//!   `cargo xtask corpus` downloads them into `corpus/stacks/<name>/`, verifying every file before it is moved into
//!   place, and leaves files that already verify alone, so a rerun is cheap.
//! - The same manifest pins format fixtures (`[[fixture]]`): files from other writers for the codec tests, downloaded
//!   into `corpus/fixtures/<name>/`, one by one or from one archive. An archive (a `.tar.gz`, as PngSuite ships) is
//!   checked against its own SHA-256 before it is extracted, and only the members the manifest lists, each checked
//!   again, are installed.
//! - `corpus/local/<name>/` holds stacks that can't or needn't be downloaded (the user's own shots), each described
//!   by a `stack.toml` (subject, frame order, optional ground-truth notes). xtask checks and lists them.
//! - `corpus/README.md` (tracked) explains all three.
//!
//! Downloads run `curl` with a generic User-Agent, as PhotoCraft's xtask does, rather than an HTTP and TLS stack
//! compiled into xtask; the SHA-256 checks make the transport's integrity irrelevant. Archives are extracted by the
//! system's `tar` (part of Windows 10 and later, macOS and Linux), run in the staging folder with relative paths so that
//! GNU tar doesn't read a drive letter as a remote host. The download-verify-install flow follows PhotoCraft's
//! `xtask/src/corpus.rs` and `pinned.rs`, and the archive flow (archive hash first, then every member) its PngSuite
//! fetcher (MIT OR Apache-2.0; see ATTRIBUTION.md).

mod local;
mod manifest;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use manifest::{Archive, Fixture, MAX_FILE_BYTES, Manifest, RemoteFile, RemoteStack};

use crate::sha256;

pub const USAGE: &str = "\
usage: cargo xtask corpus [--check] [--fixtures] [--manifest FILE] [NAME...]
       cargo xtask corpus --hash [--inline] PATH...

Downloads the stacks and format fixtures pinned in corpus/manifest.toml into corpus/stacks/<name>/ and
corpus/fixtures/<name>/, checking every file's sha256 before it is moved into place and keeping files that already
match, then lists them and the local stacks in corpus/local/<name>/ (each described by a stack.toml). An archive is
checked before it is extracted, and only its listed members are installed. corpus/README.md says how to add a stack
or a fixture. Everything but the manifest and the README is gitignored.

  NAME...          only these stacks and fixtures (default: every stack that is not a placeholder, and every fixture)
  --check          download nothing: verify the files that are there, and fail if any is missing or wrong
  --fixtures       only the format fixtures (what the codec tests need), not the stacks
  --manifest FILE  read FILE instead of corpus/manifest.toml; its files go into stacks/ and fixtures/ next to it
  --hash PATH...   print a [[stack.files]] entry (name, sha256, size) for each file, or each file in a folder
  --inline         with --hash: one-line entries, for a fixture's `files` or an archive's `members` list
  -h, --help       show this message

Downloads use curl, and archives tar (both part of Windows 10 and later, macOS and most Linux systems); curl sends
a generic User-Agent.
";

/// The User-Agent of every download: generic, with no personal details.
const USER_AGENT: &str = "florastack-xtask";

/// Folder names inside the corpus folder.
const STACKS: &str = "stacks";
const FIXTURES: &str = "fixtures";
const LOCAL: &str = "local";
const STAGING: &str = ".staging";

/// Downloads `url` into the file `to`, failing past `max_bytes`.
type Download<'a> = dyn FnMut(&str, &Path, u64) -> Result<(), String> + 'a;

/// Extracts the archive file into the (empty) folder, both in the same staging folder.
type Extract<'a> = dyn FnMut(&Path, &Path) -> Result<(), String> + 'a;

#[derive(Debug, PartialEq)]
enum Mode {
    Help,
    Fetch,
    Check,
    Hash,
}

#[derive(Debug, PartialEq)]
struct Options<'a> {
    mode: Mode,
    manifest: Option<&'a str>,
    /// Stack and fixture names, or the paths to hash.
    names: Vec<&'a str>,
    fixtures_only: bool,
    inline: bool,
}

fn parse_args<'a>(args: &[&'a str]) -> Result<Options<'a>, String> {
    let mut options = Options { mode: Mode::Fetch, manifest: None, names: Vec::new(), fixtures_only: false, inline: false };
    let mut it = args.iter();
    while let Some(&arg) = it.next() {
        match arg {
            "-h" | "--help" => return Ok(Options { mode: Mode::Help, ..options }),
            "--check" | "--hash" => {
                if options.mode != Mode::Fetch {
                    return Err("use only one of --check and --hash".to_owned());
                }
                options.mode = if arg == "--check" { Mode::Check } else { Mode::Hash };
            }
            "--manifest" => options.manifest = Some(it.next().ok_or("--manifest needs a file")?),
            "--fixtures" => options.fixtures_only = true,
            "--inline" => options.inline = true,
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            name => options.names.push(name),
        }
    }
    if options.mode == Mode::Hash && (options.names.is_empty() || options.manifest.is_some() || options.fixtures_only) {
        return Err("--hash takes one or more files or folders, and no --manifest or --fixtures".to_owned());
    }
    if options.inline && options.mode != Mode::Hash {
        return Err("--inline goes with --hash".to_owned());
    }
    Ok(options)
}

/// `cargo xtask corpus`, for the workspace at `workspace`.
pub fn run(workspace: &Path, args: &[&str]) -> Result<(), String> {
    let options = parse_args(args)?;
    match options.mode {
        Mode::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Mode::Hash => return hash(&options.names, options.inline),
        Mode::Fetch | Mode::Check => {}
    }
    let manifest_path = options.manifest.map_or_else(|| workspace.join("corpus").join("manifest.toml"), PathBuf::from);
    let root = manifest_path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest = manifest::parse(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let problems = manifest.problems();
    if !problems.is_empty() {
        return Err(format!("{} has {} problem(s):\n  - {}", manifest_path.display(), problems.len(), problems.join("\n  - ")));
    }
    let selected = select(&manifest, &options.names, options.fixtures_only)?;
    let check = options.mode == Mode::Check;
    let failures = report(&root, &manifest, &selected, check, &mut curl, &mut tar_extract)?;
    if failures.is_empty() {
        Ok(())
    } else if check {
        Err(format!("{} item(s) are missing or fail their check: {} (`cargo xtask corpus` downloads them)", failures.len(), failures.join(", ")))
    } else {
        Err(format!("{} item(s) failed: {}", failures.len(), failures.join(", ")))
    }
}

/// The manifest's stacks and fixtures to fetch or check.
#[derive(Debug, Default, PartialEq)]
struct Selection<'m> {
    stacks: Vec<&'m str>,
    fixtures: Vec<&'m str>,
}

/// The stacks and fixtures named, or every stack that isn't a placeholder and every fixture (only the fixtures with
/// `fixtures_only`).
fn select<'m>(manifest: &'m Manifest, names: &[&str], fixtures_only: bool) -> Result<Selection<'m>, String> {
    if names.is_empty() {
        let stacks = if fixtures_only { Vec::new() } else { manifest.stacks.iter().filter(|s| !s.placeholder).map(|s| s.name.as_str()).collect() };
        return Ok(Selection { stacks, fixtures: manifest.fixtures.iter().map(|f| f.name.as_str()).collect() });
    }
    let mut selection = Selection::default();
    for &name in names {
        if let Some(fixture) = manifest.fixtures.iter().find(|f| f.name == name) {
            selection.fixtures.push(fixture.name.as_str());
            continue;
        }
        match manifest.stacks.iter().find(|s| s.name == name) {
            Some(_) if fixtures_only => return Err(format!("`{name}` is a stack, and --fixtures asks for fixtures only")),
            Some(stack) if stack.placeholder => return Err(format!("`{name}` is a placeholder: give it files in the manifest first")),
            Some(stack) => selection.stacks.push(stack.name.as_str()),
            None => return Err(format!("the manifest has no stack or fixture `{name}`")),
        }
    }
    Ok(selection)
}

/// Fetches (or with `check`, verifies) the selected stacks and fixtures and prints the state of every one, local
/// stacks included. Returns the names of those that failed.
fn report(
    root: &Path,
    manifest: &Manifest,
    selected: &Selection<'_>,
    check: bool,
    download: &mut Download<'_>,
    extract: &mut Extract<'_>,
) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();
    println!("corpus: {}", root.display());
    println!("manifest stacks, in {STACKS}/<name>/:");
    let width = manifest.stacks.iter().map(|s| s.name.len()).chain(manifest.fixtures.iter().map(|f| f.name.len())).max().unwrap_or(0).max(12);
    for stack in &manifest.stacks {
        let what = format!("{}, {}", stack.subject.as_str(), stack.frame_order.as_str());
        if stack.placeholder {
            println!("  {:<width$}  placeholder  {what}. {}", stack.name, stack.description);
            continue;
        }
        if !selected.stacks.contains(&stack.name.as_str()) {
            println!("  {:<width$}  not selected", stack.name);
            continue;
        }
        let frames = stack.files.len().saturating_sub(usize::from(stack.ground_truth.as_ref().is_some_and(|t| t.reference.is_some())));
        let outcome = if check { verify_stack(root, stack).map(|()| "verified".to_owned()) } else { fetch_stack(root, stack, download) };
        match outcome {
            Ok(done) => {
                let credit = credit(stack.license.as_deref(), stack.attribution.as_deref());
                println!("  {:<width$}  ok           {what}, {}{credit}: {done}", stack.name, count(frames, "frame"));
            }
            Err(e) => {
                println!("  {:<width$}  FAILED       {e}", stack.name);
                failures.push(stack.name.clone());
            }
        }
    }
    if manifest.stacks.iter().any(|s| s.placeholder) {
        println!("  (a placeholder is a slot for a stack still to be chosen; corpus/README.md says how to fill it)");
    }
    println!("format fixtures, in {FIXTURES}/<name>/:");
    if manifest.fixtures.is_empty() {
        println!("  none");
    }
    for fixture in &manifest.fixtures {
        if !selected.fixtures.contains(&fixture.name.as_str()) {
            println!("  {:<width$}  not selected", fixture.name);
            continue;
        }
        let outcome =
            if check { verify_fixture(root, fixture).map(|()| "verified".to_owned()) } else { fetch_fixture(root, fixture, download, extract) };
        let credit = credit(Some(&fixture.license), fixture.attribution.as_deref());
        match outcome {
            Ok(done) => {
                let description = if fixture.description.is_empty() { String::new() } else { format!(". {}", fixture.description) };
                println!("  {:<width$}  ok           {}{credit}: {done}{description}", fixture.name, count(fixture_files(fixture).len(), "file"));
            }
            Err(e) => {
                println!("  {:<width$}  FAILED       {e}", fixture.name);
                failures.push(format!("{FIXTURES}/{}", fixture.name));
            }
        }
    }
    println!("local stacks, in {LOCAL}/<name>/:");
    let locals = local::load_all(&root.join(LOCAL))?;
    if locals.is_empty() {
        println!("  none (a folder of frames with a stack.toml; see corpus/README.md)");
    }
    for stack in locals {
        match stack {
            Ok(stack) => {
                let info = &stack.info;
                let truth = info
                    .ground_truth
                    .as_ref()
                    .map_or("", |t| if t.reference.is_some() { ", ground truth: notes and a reference" } else { ", ground truth: notes" });
                let credit = credit(info.license.as_deref(), info.attribution.as_deref());
                let description = if info.description.is_empty() { String::new() } else { format!(". {}", info.description) };
                println!(
                    "  {:<width$}  ok           {}, {}, {}{credit}{truth}{description}",
                    stack.name,
                    info.subject.as_str(),
                    info.frame_order.as_str(),
                    count(stack.frames.len(), "frame")
                );
            }
            Err((name, problem)) => {
                println!("  {name:<width$}  FAILED       {problem}");
                failures.push(format!("{LOCAL}/{name}"));
            }
        }
    }
    Ok(failures)
}

/// `1 frame`, `2 frames`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// ` (license, by attribution)`, or what of it is known.
fn credit(license: Option<&str>, attribution: Option<&str>) -> String {
    match (license, attribution) {
        (Some(license), Some(by)) => format!(" ({license}, by {by})"),
        (Some(license), None) => format!(" ({license})"),
        (None, Some(by)) => format!(" (by {by})"),
        (None, None) => String::new(),
    }
}

/// Why `path` doesn't hold exactly the file with this `sha256` (and `size`, if known), if it doesn't.
fn verify_file(path: &Path, sha256: &str, size: Option<u64>) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "missing".to_owned() } else { e.to_string() })?;
    if !meta.is_file() {
        return Err("not a regular file".to_owned());
    }
    if let Some(size) = size
        && meta.len() != size
    {
        return Err(format!("{} bytes instead of {size}", meta.len()));
    }
    let (hex, _) = sha256::file(path).map_err(|e| format!("cannot read it: {e}"))?;
    if hex != sha256 {
        return Err(format!("sha256 mismatch: expected {sha256}, got {hex}"));
    }
    Ok(())
}

/// Every file a fixture installs, as (name, sha256, size).
fn fixture_files(fixture: &Fixture) -> Vec<(&str, &str, Option<u64>)> {
    match &fixture.archive {
        Some(archive) => archive.members.iter().map(|m| (m.name.as_str(), m.sha256.as_str(), m.size)).collect(),
        None => fixture.files.iter().map(|f| (f.name.as_str(), f.sha256.as_str(), f.size)).collect(),
    }
}

/// The problems of the files in `dir`, joined, or `Ok` when every one verifies.
fn verify_all<'f>(dir: &Path, files: impl IntoIterator<Item = (&'f str, &'f str, Option<u64>)>) -> Result<(), String> {
    let bad: Vec<String> =
        files.into_iter().filter_map(|(name, sha, size)| verify_file(&dir.join(name), sha, size).err().map(|e| format!("{name}: {e}"))).collect();
    if bad.is_empty() { Ok(()) } else { Err(bad.join("; ")) }
}

/// Verifies a downloaded stack without downloading anything.
fn verify_stack(root: &Path, stack: &RemoteStack) -> Result<(), String> {
    verify_all(&root.join(STACKS).join(&stack.name), stack.files.iter().map(|f| (f.name.as_str(), f.sha256.as_str(), f.size)))
}

/// Verifies a downloaded fixture without downloading anything.
fn verify_fixture(root: &Path, fixture: &Fixture) -> Result<(), String> {
    verify_all(&root.join(FIXTURES).join(&fixture.name), fixture_files(fixture))
}

/// Downloads every file of `files` that doesn't verify in `dir` yet into `staging`, checks it, and moves it into
/// `dir`. A file that fails its check is deleted, never installed. Returns what it did.
fn fetch_files(dir: &Path, staging: &Path, files: &[RemoteFile], download: &mut Download<'_>) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let (mut downloaded, mut kept) = (0, 0);
    let result = (|| {
        for file in files {
            let target = dir.join(&file.name);
            if verify_file(&target, &file.sha256, file.size).is_ok() {
                kept += 1;
                continue;
            }
            std::fs::create_dir_all(staging).map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
            let part = staging.join(format!("{}.part", file.name));
            remove_if_present(&part)?;
            let fetched = download(&file.url, &part, file.size.unwrap_or(MAX_FILE_BYTES)).and_then(|()| verify_file(&part, &file.sha256, file.size));
            if let Err(e) = fetched {
                remove_if_present(&part)?;
                return Err(format!("{}: {e}; nothing was installed", file.name));
            }
            std::fs::rename(&part, &target).map_err(|e| format!("cannot move {} into place: {e}", file.name))?;
            downloaded += 1;
        }
        Ok(())
    })();
    // The staging folder is empty now, or holds nothing worth keeping.
    let _ = std::fs::remove_dir_all(staging);
    result.map(|()| format!("{downloaded} downloaded, {kept} already verified"))
}

/// Downloads every file of `stack` that doesn't verify yet, as [`fetch_files`] does, into `stacks/<name>/`.
fn fetch_stack(root: &Path, stack: &RemoteStack, download: &mut Download<'_>) -> Result<String, String> {
    let result = fetch_files(&root.join(STACKS).join(&stack.name), &root.join(STAGING).join(&stack.name), &stack.files, download);
    let _ = std::fs::remove_dir(root.join(STAGING));
    result
}

/// Downloads a fixture's files, or extracts them from its archive, into `fixtures/<name>/`.
fn fetch_fixture(root: &Path, fixture: &Fixture, download: &mut Download<'_>, extract: &mut Extract<'_>) -> Result<String, String> {
    let dir = root.join(FIXTURES).join(&fixture.name);
    let staging = root.join(STAGING).join(format!("{FIXTURES}-{}", fixture.name));
    let result = match &fixture.archive {
        None => fetch_files(&dir, &staging, &fixture.files, download),
        Some(archive) => fetch_archive(&dir, &staging, archive, download, extract),
    };
    let _ = std::fs::remove_dir(root.join(STAGING));
    result
}

/// Installs an archive's members into `dir` unless they all verify there already: the archive is downloaded into
/// `staging` and checked, extracted there, and each member checked before it is moved into place. Nothing is
/// installed from an archive that fails its check, and a member that fails its own leaves the others' installation
/// to a later run (they are checked one by one).
fn fetch_archive(dir: &Path, staging: &Path, archive: &Archive, download: &mut Download<'_>, extract: &mut Extract<'_>) -> Result<String, String> {
    let members = || archive.members.iter().map(|m| (m.name.as_str(), m.sha256.as_str(), m.size));
    if verify_all(dir, members()).is_ok() {
        return Ok(format!("{} already verified", archive.members.len()));
    }
    let result = (|| {
        let _ = std::fs::remove_dir_all(staging);
        std::fs::create_dir_all(staging).map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
        let file = staging.join("archive.tar.gz");
        download(&archive.url, &file, archive.size.unwrap_or(MAX_FILE_BYTES))
            .and_then(|()| verify_file(&file, &archive.sha256, archive.size))
            .map_err(|e| format!("the archive: {e}; nothing was extracted"))?;
        let extracted = staging.join("extracted");
        std::fs::create_dir(&extracted).map_err(|e| format!("cannot create {}: {e}", extracted.display()))?;
        extract(&file, &extracted)?;
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let (mut installed, mut kept) = (0, 0);
        for member in &archive.members {
            let target = dir.join(&member.name);
            if verify_file(&target, &member.sha256, member.size).is_ok() {
                kept += 1;
                continue;
            }
            let source = extracted.join(member.path.as_deref().unwrap_or(&member.name));
            verify_file(&source, &member.sha256, member.size).map_err(|e| format!("{} in the archive: {e}; it was not installed", member.name))?;
            std::fs::rename(&source, &target).map_err(|e| format!("cannot move {} into place: {e}", member.name))?;
            installed += 1;
        }
        Ok(format!("{installed} extracted, {kept} already verified"))
    })();
    let _ = std::fs::remove_dir_all(staging);
    result
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// Downloads with curl: following redirects over HTTP(S) only, retrying transient failures, and stopping past
/// `max_bytes` (checked again afterwards, for servers that send no length).
fn curl(url: &str, to: &Path, max_bytes: u64) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--retry", "3", "--proto", "=https,http,file", "--proto-redir", "=https,http"])
        .args(["--user-agent", USER_AGENT, "--max-filesize", &max_bytes.to_string(), "--output"])
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl is not installed (it comes with Windows 10 and later, macOS and most Linux systems)".to_owned()
            } else {
                format!("cannot run curl: {e}")
            }
        })?;
    if !status.success() {
        return Err(format!("curl failed ({status}) for {url}"));
    }
    match std::fs::metadata(to) {
        Ok(meta) if meta.len() > max_bytes => Err(format!("{url} is larger than {max_bytes} bytes")),
        Ok(_) => Ok(()),
        Err(e) => Err(format!("curl wrote no file for {url}: {e}")),
    }
}

/// Extracts a `.tar.gz` with the system's tar, run in the folder that holds both the archive and the target folder,
/// with relative names: GNU tar takes `C:` in an absolute Windows path for a remote host.
fn tar_extract(archive: &Path, into: &Path) -> Result<(), String> {
    let folder = archive.parent().ok_or("the archive has no folder")?;
    let name = |p: &Path| p.file_name().map(std::ffi::OsStr::to_owned).ok_or_else(|| format!("{} has no file name", p.display()));
    if into.parent() != Some(folder) {
        return Err("the archive and the folder to extract into must be side by side".to_owned());
    }
    let status =
        Command::new("tar").current_dir(folder).arg("-xzf").arg(name(archive)?).arg("-C").arg(name(into)?).stdin(Stdio::null()).status().map_err(
            |e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "tar is not installed (it comes with Windows 10 and later, macOS and Linux)".to_owned()
                } else {
                    format!("cannot run tar: {e}")
                }
            },
        )?;
    if status.success() { Ok(()) } else { Err(format!("tar failed ({status}) extracting the archive")) }
}

/// A manifest entry for one file: a `[[stack.files]]` table, or one line for a fixture's inline `files` or `members`.
fn hash_entry(name: &str, hex: &str, size: u64, inline: bool) -> String {
    if inline {
        format!("    {{ name = {name:?}, sha256 = \"{hex}\", size = {size} }},")
    } else {
        format!("[[stack.files]]\nname = {name:?}\nurl = \"https://\"\nsha256 = \"{hex}\"\nsize = {size}\n")
    }
}

/// `--hash`: prints a manifest entry for every file named, or in a folder named, in name order.
fn hash(paths: &[&str], inline: bool) -> Result<(), String> {
    for path in paths.iter().map(Path::new) {
        let mut files = Vec::new();
        if path.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| format!("cannot read {}: {e}", path.display()))? {
                let entry = entry.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                if entry.file_type().is_ok_and(|t| t.is_file()) {
                    files.push(entry.path());
                }
            }
            files.sort();
        } else {
            files.push(path.to_path_buf());
        }
        for file in files {
            let (hex, size) = sha256::file(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !manifest::is_file_name(&name) {
                eprintln!("note: rename {name:?} before listing it: manifest file names are letters, digits, `.`, `_`, `-` and `+`");
            }
            println!("{}", hash_entry(&name, &hex, size, inline));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A fresh folder under the system temp directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!("florastack-corpus-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(name: &str, bytes: &[u8]) -> String {
        format!(
            "[[stack.files]]\nname = \"{name}\"\nurl = \"https://example.org/{name}\"\nsha256 = \"{}\"\nsize = {}\n",
            sha256::hex_of(bytes),
            bytes.len()
        )
    }

    fn two_frame_manifest() -> Manifest {
        let text = format!(
            "version = 1\n[[stack]]\nname = \"bee\"\nsubject = \"insect\"\nframe_order = \"near-to-far\"\nsource = \"https://example.org\"\n\
             license = \"CC0-1.0\"\n{}{}\
             [[stack]]\nname = \"later\"\nplaceholder = true\nsubject = \"coin\"\nframe_order = \"unknown\"\n",
            entry("f1.jpg", b"frame one"),
            entry("f2.jpg", b"frame two")
        );
        manifest::parse(&text).unwrap()
    }

    /// A manifest with one fixture of two files and one archive fixture of two members.
    fn fixture_manifest() -> Manifest {
        let member = |name: &str, bytes: &[u8]| format!("{{ name = \"{name}\", sha256 = \"{}\", size = {} }}", sha256::hex_of(bytes), bytes.len());
        let text = format!(
            "version = 1\n\
             [[fixture]]\nname = \"tiff\"\nsource = \"https://example.org/tiff\"\nlicense = \"MIT\"\n\
             [[fixture.files]]\nname = \"a.tif\"\nurl = \"https://example.org/a.tif\"\nsha256 = \"{}\"\nsize = 5\n\
             [[fixture.files]]\nname = \"b.tif\"\nurl = \"https://example.org/b.tif\"\nsha256 = \"{}\"\nsize = 5\n\
             [[fixture]]\nname = \"suite\"\nsource = \"https://example.org/suite\"\nlicense = \"LicenseRef-Suite\"\nattribution = \"S. Uite\"\n\
             [fixture.archive]\nurl = \"https://example.org/suite.tgz\"\nsha256 = \"{}\"\nsize = 7\n\
             members = [{}, {}]\n",
            sha256::hex_of(b"tiffa"),
            sha256::hex_of(b"tiffb"),
            sha256::hex_of(b"archive"),
            member("one.png", b"png one"),
            member("two.png", b"png two"),
        );
        let m = manifest::parse(&text).unwrap();
        assert_eq!(m.problems(), Vec::<String>::new());
        m
    }

    /// A downloader serving `files` (by URL), counting its calls.
    fn server(files: HashMap<String, Vec<u8>>, calls: &mut u32) -> impl FnMut(&str, &Path, u64) -> Result<(), String> + '_ {
        move |url, to, max| {
            *calls += 1;
            let bytes = files.get(url).ok_or_else(|| format!("404 {url}"))?;
            if bytes.len() as u64 > max {
                return Err("too large".to_owned());
            }
            std::fs::write(to, bytes).map_err(|e| e.to_string())
        }
    }

    fn served(pairs: &[(&str, &[u8])]) -> HashMap<String, Vec<u8>> {
        pairs.iter().map(|(name, bytes)| (format!("https://example.org/{name}"), bytes.to_vec())).collect()
    }

    /// An "extractor" that writes these files, as the archive would hold them.
    fn unpacker(files: Vec<(&'static str, &'static [u8])>) -> impl FnMut(&Path, &Path) -> Result<(), String> {
        move |_, into| {
            for (name, bytes) in &files {
                std::fs::write(into.join(name), bytes).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }

    fn no_extract(_: &Path, _: &Path) -> Result<(), String> {
        Err("no archives here".to_owned())
    }

    #[test]
    fn fetching_downloads_verifies_and_then_keeps_verified_files() {
        let tmp = TempDir::new();
        let manifest = two_frame_manifest();
        let stack = &manifest.stacks[0];
        let mut calls = 0;
        let mut download = server(served(&[("f1.jpg", b"frame one"), ("f2.jpg", b"frame two")]), &mut calls);
        assert_eq!(fetch_stack(&tmp.0, stack, &mut download).unwrap(), "2 downloaded, 0 already verified");
        assert_eq!(fetch_stack(&tmp.0, stack, &mut download).unwrap(), "0 downloaded, 2 already verified");
        let dir = tmp.0.join(STACKS).join("bee");
        std::fs::write(dir.join("f2.jpg"), b"frame 2").unwrap();
        assert!(verify_stack(&tmp.0, stack).unwrap_err().contains("f2.jpg: 7 bytes instead of 9"));
        assert_eq!(fetch_stack(&tmp.0, stack, &mut download).unwrap(), "1 downloaded, 1 already verified");
        drop(download);
        assert_eq!(calls, 3);
        assert_eq!(std::fs::read(dir.join("f2.jpg")).unwrap(), b"frame two");
        assert_eq!(verify_stack(&tmp.0, stack), Ok(()));
        assert!(!tmp.0.join(STAGING).exists(), "staging is cleaned up");
    }

    #[test]
    fn a_download_that_fails_its_check_is_never_installed() {
        let tmp = TempDir::new();
        let manifest = two_frame_manifest();
        let stack = &manifest.stacks[0];
        let mut calls = 0;
        // Same size, wrong bytes: only the hash can tell.
        let mut download = server(served(&[("f1.jpg", b"frame one"), ("f2.jpg", b"frame 2!!")]), &mut calls);
        let e = fetch_stack(&tmp.0, stack, &mut download).unwrap_err();
        assert!(e.contains("f2.jpg: sha256 mismatch") && e.contains("nothing was installed"), "{e}");
        let dir = tmp.0.join(STACKS).join("bee");
        assert!(dir.join("f1.jpg").is_file() && !dir.join("f2.jpg").exists());
        assert!(!tmp.0.join(STAGING).exists());
        drop(download);
        let mut missing = server(HashMap::new(), &mut calls);
        assert!(fetch_stack(&tmp.0, stack, &mut missing).unwrap_err().contains("404 https://example.org/f2.jpg"));
        let mut checks = 0;
        let mut big = server(served(&[("f2.jpg", b"frame two and more")]), &mut checks);
        assert!(fetch_stack(&tmp.0, stack, &mut big).unwrap_err().contains("too large"));
    }

    #[test]
    fn fixture_files_and_archive_members_are_fetched_and_verified() {
        let tmp = TempDir::new();
        let manifest = fixture_manifest();
        let (tiff, suite) = (&manifest.fixtures[0], &manifest.fixtures[1]);
        let mut calls = 0;
        let mut download = server(served(&[("a.tif", b"tiffa"), ("b.tif", b"tiffb"), ("suite.tgz", b"archive")]), &mut calls);
        assert_eq!(fetch_fixture(&tmp.0, tiff, &mut download, &mut no_extract).unwrap(), "2 downloaded, 0 already verified");
        let mut unpack = unpacker(vec![("one.png", b"png one"), ("two.png", b"png two"), ("extra.txt", b"not listed")]);
        assert_eq!(fetch_fixture(&tmp.0, suite, &mut download, &mut unpack).unwrap(), "2 extracted, 0 already verified");
        let dir = tmp.0.join(FIXTURES).join("suite");
        assert!(dir.join("one.png").is_file() && !dir.join("extra.txt").exists(), "only the listed members");
        // Verified members aren't downloaded again.
        assert_eq!(fetch_fixture(&tmp.0, suite, &mut download, &mut unpack).unwrap(), "2 already verified");
        assert_eq!(verify_fixture(&tmp.0, suite), Ok(()));
        std::fs::remove_file(dir.join("two.png")).unwrap();
        assert!(verify_fixture(&tmp.0, suite).unwrap_err().contains("two.png: missing"));
        assert_eq!(fetch_fixture(&tmp.0, suite, &mut download, &mut unpack).unwrap(), "1 extracted, 1 already verified");
        drop(download);
        assert_eq!(calls, 4);
        assert!(!tmp.0.join(STAGING).exists(), "staging is cleaned up");
    }

    #[test]
    fn a_bad_archive_or_member_is_never_installed() {
        let tmp = TempDir::new();
        let manifest = fixture_manifest();
        let suite = &manifest.fixtures[1];
        let mut calls = 0;
        // The archive itself is wrong: nothing is extracted.
        let mut wrong = server(served(&[("suite.tgz", b"archivX")]), &mut calls);
        let mut unpack = unpacker(vec![("one.png", b"png one"), ("two.png", b"png two")]);
        let e = fetch_fixture(&tmp.0, suite, &mut wrong, &mut unpack).unwrap_err();
        assert!(e.contains("the archive: sha256 mismatch") && e.contains("nothing was extracted"), "{e}");
        assert!(!tmp.0.join(FIXTURES).join("suite").join("one.png").exists());
        drop(wrong);
        // A member is wrong: it isn't installed.
        let mut right = server(served(&[("suite.tgz", b"archive")]), &mut calls);
        let mut bad_member = unpacker(vec![("one.png", b"png one"), ("two.png", b"png 2!!")]);
        let e = fetch_fixture(&tmp.0, suite, &mut right, &mut bad_member).unwrap_err();
        assert!(e.contains("two.png in the archive: sha256 mismatch"), "{e}");
        assert!(!tmp.0.join(FIXTURES).join("suite").join("two.png").exists());
        assert!(!tmp.0.join(STAGING).exists());
    }

    #[test]
    fn the_system_tar_extracts_an_archive_by_relative_paths() {
        let tmp = TempDir::new();
        let source = tmp.0.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.png"), b"member a").unwrap();
        // The archive is made with tar too: this test checks the extraction call, not tar itself.
        let made = Command::new("tar").current_dir(&tmp.0).args(["-czf", "archive.tar.gz", "-C", "source", "a.png"]).status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("tar is not available here: the extraction test can't run");
            return;
        }
        let into = tmp.0.join("extracted");
        std::fs::create_dir(&into).unwrap();
        tar_extract(&tmp.0.join("archive.tar.gz"), &into).unwrap();
        assert_eq!(std::fs::read(into.join("a.png")).unwrap(), b"member a");
        assert!(tar_extract(&tmp.0.join("archive.tar.gz"), &source.join("deeper")).is_err(), "side by side only");
    }

    #[test]
    fn selection_and_reports_cover_placeholders_fixtures_and_local_stacks() {
        let tmp = TempDir::new();
        let manifest = two_frame_manifest();
        assert_eq!(select(&manifest, &[], false).unwrap(), Selection { stacks: vec!["bee"], fixtures: vec![] });
        assert!(select(&manifest, &["later"], false).unwrap_err().contains("placeholder"));
        assert!(select(&manifest, &["wasp"], false).unwrap_err().contains("no stack or fixture `wasp`"));
        assert!(select(&manifest, &["bee"], true).unwrap_err().contains("--fixtures"));
        let fixtures = fixture_manifest();
        assert_eq!(select(&fixtures, &[], true).unwrap(), Selection { stacks: vec![], fixtures: vec!["tiff", "suite"] });
        assert_eq!(select(&fixtures, &["suite"], false).unwrap(), Selection { stacks: vec![], fixtures: vec!["suite"] });
        let mut calls = 0;
        let mut download = server(served(&[("f1.jpg", b"frame one"), ("f2.jpg", b"frame two")]), &mut calls);
        let selected = select(&manifest, &["bee"], false).unwrap();
        // Checking before anything is downloaded fails; fetching repairs it.
        assert_eq!(report(&tmp.0, &manifest, &selected, true, &mut download, &mut no_extract).unwrap(), ["bee"]);
        assert_eq!(report(&tmp.0, &manifest, &selected, false, &mut download, &mut no_extract).unwrap(), Vec::<String>::new());
        assert_eq!(report(&tmp.0, &manifest, &selected, true, &mut download, &mut no_extract).unwrap(), Vec::<String>::new());
        // A local stack without its stack.toml is reported.
        std::fs::create_dir_all(tmp.0.join(LOCAL).join("coin")).unwrap();
        assert_eq!(report(&tmp.0, &manifest, &Selection::default(), true, &mut download, &mut no_extract).unwrap(), ["local/coin"]);
        // A fixture that isn't there fails a check.
        let all = select(&fixtures, &[], true).unwrap();
        assert_eq!(report(&tmp.0, &fixtures, &all, true, &mut download, &mut no_extract).unwrap(), ["fixtures/tiff", "fixtures/suite", "local/coin"]);
    }

    #[test]
    fn local_stacks_list_their_frames_in_order() {
        let tmp = TempDir::new();
        let local = tmp.0.join(LOCAL);
        let make = |name: &str, toml: Option<&str>, files: &[&str]| {
            let dir = local.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some(toml) = toml {
                std::fs::write(dir.join("stack.toml"), toml).unwrap();
            }
            for file in files {
                std::fs::write(dir.join(file), b"pixels").unwrap();
            }
        };
        make(
            "a-insect",
            Some("subject = \"insect\"\nframe_order = \"far-to-near\"\n[ground_truth]\nnotes = \"antennae\"\nreference = \"ref.tif\"\n"),
            &["b.JPG", "a.jpg", "ref.tif", "notes.txt"],
        );
        make("b-coin", Some("subject = \"coin\"\nframe_order = \"unknown\"\nframes = [\"z.tif\", \"y.tif\"]\n"), &["y.tif", "z.tif"]);
        make("c-none", None, &["a.jpg"]);
        make("d-missing", Some("subject = \"flower\"\nframe_order = \"near-to-far\"\nframes = [\"gone.jpg\"]\n"), &[]);
        make("e-empty", Some("subject = \"landscape\"\nframe_order = \"near-to-far\"\n"), &["readme.txt"]);
        make("f-typo", Some("subject = \"insect\"\nframe_order = \"near-to-far\"\nframe = []\n"), &["a.jpg"]);
        make(".hidden", None, &[]);
        let stacks = local::load_all(&local).unwrap();
        assert_eq!(stacks.len(), 6);
        let a = stacks[0].as_ref().unwrap();
        assert_eq!(a.frames, ["a.jpg", "b.JPG"], "images by name, without the reference");
        assert_eq!(a.info.ground_truth.as_ref().unwrap().notes, "antennae");
        assert_eq!(stacks[1].as_ref().unwrap().frames, ["z.tif", "y.tif"], "the listed order");
        let problem = |i: usize| stacks[i].as_ref().unwrap_err().1.clone();
        assert!(problem(2).contains("no stack.toml"));
        assert!(problem(3).contains("`gone.jpg`, which is not a file"));
        assert!(problem(4).contains("no frames"));
        assert!(problem(5).contains("unknown field `frame`"), "{}", problem(5));
        assert!(local::load_all(&tmp.0.join("nowhere")).unwrap().is_empty(), "no local folder, no stacks");
    }

    #[test]
    fn arguments_are_parsed_strictly() {
        let fetch = Options { mode: Mode::Fetch, manifest: None, names: vec![], fixtures_only: false, inline: false };
        assert_eq!(parse_args(&[]).unwrap(), fetch);
        assert_eq!(parse_args(&["--check", "bee"]).unwrap(), Options { mode: Mode::Check, names: vec!["bee"], ..fetch });
        assert_eq!(parse_args(&["--fixtures"]).unwrap(), Options { fixtures_only: true, ..fetch });
        assert_eq!(parse_args(&["--manifest", "m.toml", "--help"]).unwrap().mode, Mode::Help);
        assert_eq!(parse_args(&["--hash", "a.jpg"]).unwrap().names, ["a.jpg"]);
        assert!(parse_args(&["--hash", "--inline", "dir"]).unwrap().inline);
        assert!(parse_args(&["--inline"]).is_err(), "--inline only with --hash");
        assert!(parse_args(&["--hash"]).is_err());
        assert!(parse_args(&["--hash", "--fixtures", "x"]).is_err());
        assert!(parse_args(&["--check", "--hash", "x"]).is_err());
        assert!(parse_args(&["--manifest"]).is_err());
        assert!(parse_args(&["--force"]).unwrap_err().contains("unknown option `--force`"));
    }

    #[test]
    fn hash_entries_come_as_tables_or_lines() {
        assert_eq!(hash_entry("a.png", "00ff", 3, true), "    { name = \"a.png\", sha256 = \"00ff\", size = 3 },");
        assert!(hash_entry("a.png", "00ff", 3, false).starts_with("[[stack.files]]\nname = \"a.png\"\n"));
    }
}
