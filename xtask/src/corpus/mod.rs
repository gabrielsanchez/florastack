//! `cargo xtask corpus`: the corpus of real focus stacks, in the gitignored `corpus/` folder.
//!
//! - `corpus/manifest.toml` (tracked) pins stacks to download: each file with its URL, SHA-256 and size.
//!   `cargo xtask corpus` downloads them into `corpus/stacks/<name>/`, verifying every file before it is moved into
//!   place, and leaves files that already verify alone, so a rerun is cheap.
//! - `corpus/local/<name>/` holds stacks that can't or needn't be downloaded (the user's own shots), each described
//!   by a `stack.toml` (subject, frame order, optional ground-truth notes). xtask checks and lists them.
//! - `corpus/README.md` (tracked) explains both.
//!
//! Downloads run `curl` with a generic User-Agent, as PhotoCraft's xtask does, rather than an HTTP and TLS stack
//! compiled into xtask; the SHA-256 check makes the transport's integrity irrelevant. The download-verify-install
//! flow follows PhotoCraft's `xtask/src/corpus.rs` and `pinned.rs` (MIT OR Apache-2.0; see ATTRIBUTION.md).

mod local;
mod manifest;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use manifest::{MAX_FILE_BYTES, Manifest, RemoteFile, RemoteStack};

use crate::sha256;

pub const USAGE: &str = "\
usage: cargo xtask corpus [--check] [--manifest FILE] [NAME...]
       cargo xtask corpus --hash PATH...

Downloads the stacks pinned in corpus/manifest.toml into corpus/stacks/<name>/, checking every file's sha256 before
it is moved into place and keeping files that already match, then lists those stacks and the local stacks in
corpus/local/<name>/ (each described by a stack.toml). corpus/README.md says how to add a stack. Everything but the
manifest and the README is gitignored.

  NAME...          only these manifest stacks (default: every stack that is not a placeholder)
  --check          download nothing: verify the files that are there, and fail if any is missing or wrong
  --manifest FILE  read FILE instead of corpus/manifest.toml; its stacks go into stacks/ next to it
  --hash PATH...   print a [[stack.files]] entry (name, sha256, size) for each file, or each file in a folder
  -h, --help       show this message

Downloads use curl (part of Windows 10 and later, macOS and most Linux systems) with a generic User-Agent.
";

/// The User-Agent of every download: generic, with no personal details.
const USER_AGENT: &str = "florastack-xtask";

/// Folder names inside the corpus folder.
const STACKS: &str = "stacks";
const LOCAL: &str = "local";
const STAGING: &str = ".staging";

/// Downloads `url` into the file `to`, failing past `max_bytes`.
type Download<'a> = dyn FnMut(&str, &Path, u64) -> Result<(), String> + 'a;

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
    /// Stack names, or the paths to hash.
    names: Vec<&'a str>,
}

fn parse_args<'a>(args: &[&'a str]) -> Result<Options<'a>, String> {
    let mut options = Options { mode: Mode::Fetch, manifest: None, names: Vec::new() };
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
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            name => options.names.push(name),
        }
    }
    if options.mode == Mode::Hash && (options.names.is_empty() || options.manifest.is_some()) {
        return Err("--hash takes one or more files or folders, and no --manifest".to_owned());
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
        Mode::Hash => return hash(&options.names),
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
    let selected = select(&manifest, &options.names)?;
    let check = options.mode == Mode::Check;
    let mut download = curl;
    let failures = report(&root, &manifest, &selected, check, &mut download)?;
    if failures.is_empty() {
        Ok(())
    } else if check {
        Err(format!("{} stack(s) are missing or fail their check: {} (`cargo xtask corpus` downloads them)", failures.len(), failures.join(", ")))
    } else {
        Err(format!("{} stack(s) failed: {}", failures.len(), failures.join(", ")))
    }
}

/// The manifest stacks to fetch or check: those named, or every one that isn't a placeholder.
fn select<'m>(manifest: &'m Manifest, names: &[&str]) -> Result<Vec<&'m str>, String> {
    if names.is_empty() {
        return Ok(manifest.stacks.iter().filter(|s| !s.placeholder).map(|s| s.name.as_str()).collect());
    }
    names
        .iter()
        .map(|&name| match manifest.stacks.iter().find(|s| s.name == name) {
            Some(stack) if stack.placeholder => Err(format!("`{name}` is a placeholder: give it files in the manifest first")),
            Some(stack) => Ok(stack.name.as_str()),
            None => Err(format!("the manifest has no stack `{name}`")),
        })
        .collect()
}

/// Fetches (or with `check`, verifies) the selected stacks and prints the state of every stack, local ones
/// included. Returns the names of the stacks that failed.
fn report(root: &Path, manifest: &Manifest, selected: &[&str], check: bool, download: &mut Download<'_>) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();
    println!("corpus: {}", root.display());
    println!("manifest stacks, in {STACKS}/<name>/:");
    let width = manifest.stacks.iter().map(|s| s.name.len()).max().unwrap_or(0).max(12);
    for stack in &manifest.stacks {
        let what = format!("{}, {}", stack.subject.as_str(), stack.frame_order.as_str());
        if stack.placeholder {
            println!("  {:<width$}  placeholder  {what}. {}", stack.name, stack.description);
            continue;
        }
        if !selected.contains(&stack.name.as_str()) {
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

/// Why `path` doesn't hold exactly the file `expected` pins, if it doesn't.
fn verify_file(path: &Path, expected: &RemoteFile) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "missing".to_owned() } else { e.to_string() })?;
    if !meta.is_file() {
        return Err("not a regular file".to_owned());
    }
    if let Some(size) = expected.size
        && meta.len() != size
    {
        return Err(format!("{} bytes instead of {size}", meta.len()));
    }
    let (hex, _) = sha256::file(path).map_err(|e| format!("cannot read it: {e}"))?;
    if hex != expected.sha256 {
        return Err(format!("sha256 mismatch: expected {}, got {hex}", expected.sha256));
    }
    Ok(())
}

/// Verifies a downloaded stack without downloading anything.
fn verify_stack(root: &Path, stack: &RemoteStack) -> Result<(), String> {
    let dir = root.join(STACKS).join(&stack.name);
    let bad: Vec<String> =
        stack.files.iter().filter_map(|file| verify_file(&dir.join(&file.name), file).err().map(|e| format!("{}: {e}", file.name))).collect();
    if bad.is_empty() { Ok(()) } else { Err(bad.join("; ")) }
}

/// Downloads every file of `stack` that doesn't verify yet into a staging folder, checks it, and moves it into
/// `stacks/<name>/`. A file that fails its check is deleted, never installed. Returns what it did.
fn fetch_stack(root: &Path, stack: &RemoteStack, download: &mut Download<'_>) -> Result<String, String> {
    let dir = root.join(STACKS).join(&stack.name);
    let staging = root.join(STAGING).join(&stack.name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let (mut downloaded, mut kept) = (0, 0);
    let result = (|| {
        for file in &stack.files {
            let target = dir.join(&file.name);
            if verify_file(&target, file).is_ok() {
                kept += 1;
                continue;
            }
            std::fs::create_dir_all(&staging).map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
            let part = staging.join(format!("{}.part", file.name));
            remove_if_present(&part)?;
            let fetched = download(&file.url, &part, file.size.unwrap_or(MAX_FILE_BYTES)).and_then(|()| verify_file(&part, file));
            if let Err(e) = fetched {
                remove_if_present(&part)?;
                return Err(format!("{}: {e}; nothing was installed", file.name));
            }
            std::fs::rename(&part, &target).map_err(|e| format!("cannot move {} into place: {e}", file.name))?;
            downloaded += 1;
        }
        Ok(())
    })();
    // The staging folders are empty now, or hold nothing worth keeping.
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir(root.join(STAGING));
    result.map(|()| format!("{downloaded} downloaded, {kept} already verified"))
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

/// `--hash`: prints a manifest entry for every file named, or in a folder named, in name order.
fn hash(paths: &[&str]) -> Result<(), String> {
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
            println!("[[stack.files]]\nname = {name:?}\nurl = \"https://\"\nsha256 = \"{hex}\"\nsize = {size}\n");
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
    fn selection_and_reports_cover_placeholders_and_local_stacks() {
        let tmp = TempDir::new();
        let manifest = two_frame_manifest();
        assert_eq!(select(&manifest, &[]).unwrap(), ["bee"]);
        assert!(select(&manifest, &["later"]).unwrap_err().contains("placeholder"));
        assert!(select(&manifest, &["wasp"]).unwrap_err().contains("no stack `wasp`"));
        let mut calls = 0;
        let mut download = server(served(&[("f1.jpg", b"frame one"), ("f2.jpg", b"frame two")]), &mut calls);
        // Checking before anything is downloaded fails; fetching repairs it.
        assert_eq!(report(&tmp.0, &manifest, &["bee"], true, &mut download).unwrap(), ["bee"]);
        assert_eq!(report(&tmp.0, &manifest, &["bee"], false, &mut download).unwrap(), Vec::<String>::new());
        assert_eq!(report(&tmp.0, &manifest, &["bee"], true, &mut download).unwrap(), Vec::<String>::new());
        // A local stack without its stack.toml is reported.
        std::fs::create_dir_all(tmp.0.join(LOCAL).join("coin")).unwrap();
        assert_eq!(report(&tmp.0, &manifest, &[], true, &mut download).unwrap(), ["local/coin"]);
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
        assert_eq!(parse_args(&[]).unwrap(), Options { mode: Mode::Fetch, manifest: None, names: vec![] });
        assert_eq!(parse_args(&["--check", "bee"]).unwrap(), Options { mode: Mode::Check, manifest: None, names: vec!["bee"] });
        assert_eq!(parse_args(&["--manifest", "m.toml", "--help"]).unwrap().mode, Mode::Help);
        assert_eq!(parse_args(&["--hash", "a.jpg"]).unwrap().names, ["a.jpg"]);
        assert!(parse_args(&["--hash"]).is_err());
        assert!(parse_args(&["--check", "--hash", "x"]).is_err());
        assert!(parse_args(&["--manifest"]).is_err());
        assert!(parse_args(&["--force"]).unwrap_err().contains("unknown option `--force`"));
    }
}
