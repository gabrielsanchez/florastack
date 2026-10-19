//! Test tooling shared by FloraStack's tests. It may depend on no other crate (std only, so it costs a test build
//! nothing), and other crates may use it only as a dev-dependency; `cargo xtask check-layers` enforces both.
//!
//! # Skipped tests
//!
//! libtest has no "skipped" status. A test that needs a GPU adapter or a corpus file and returns early without one
//! passes, so a green run can't tell you whether the GPU tests ran at all. Such a test records why with [`skip!`]
//! before it returns:
//!
//! ```no_run
//! # fn gpu_adapter() -> Option<u32> { None }
//! # fn compare_with_cpu_reference(_adapter: u32) {}
//! let Some(adapter) = gpu_adapter() else {
//!     florastack_testkit::skip!(Gpu, "no GPU adapter passed the self-test");
//!     return;
//! };
//! compare_with_cpu_reference(adapter);
//! ```
//!
//! [`skip!`] prints the reason to stderr and, when the [`SKIP_LOG_ENV`] environment variable names a file, appends
//! one JSON line to it, such as
//! `{"need":"gpu","reason":"no GPU adapter passed the self-test","test":"tests::warp_matches_cpu","binary":"florastack_gpu","location":"crates/gpu/src/warp.rs:120"}`.
//! `cargo xtask ci` sets the variable and lists every recorded skip in its summary. A test that runs only part of
//! its checks (the CPU half, say) records the part it couldn't run the same way.
//!
//! Adapted from LightCraft's `lightcraft-testkit` (MIT OR Apache-2.0; see ATTRIBUTION.md and NOTICE).

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

/// The environment variable naming the file that [`skip!`] appends its records to, one JSON object per line.
pub const SKIP_LOG_ENV: &str = "FLORASTACK_SKIP_LOG";

/// What a skipped test needed and didn't find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// A GPU adapter that passes the self-test, for the GPU kernels' agreement tests against their CPU references.
    Gpu,
    /// Files from the test corpus, which lives in the gitignored `corpus/` folder.
    Corpus,
}

impl Need {
    /// The name in the skip log and in the `cargo xtask ci` summary.
    pub fn as_str(self) -> &'static str {
        match self {
            Need::Gpu => "gpu",
            Need::Corpus => "corpus",
        }
    }
}

/// Records that this test, or a part of it, can't run here: `skip!(Corpus, "{} is missing", path.display())`.
///
/// The first argument names a [`Need`] variant and the rest is a `format!` message saying what is missing. The macro
/// doesn't return from the test: follow it with `return` (or `continue`).
#[macro_export]
macro_rules! skip {
    ($need:ident, $($arg:tt)+) => {
        $crate::record($crate::Need::$need, &::std::format!($($arg)+), ::std::file!(), ::std::line!())
    };
}

/// What [`skip!`] calls: prints the reason and appends a record to the skip log, if [`SKIP_LOG_ENV`] names one. It
/// never fails the test: a log that can't be written loses the record, not the run.
pub fn record(need: Need, reason: &str, file: &str, line: u32) {
    eprintln!("skipped ({}): {reason}", need.as_str());
    let Some(log) = std::env::var_os(SKIP_LOG_ENV).filter(|path| !path.is_empty()) else {
        return;
    };
    // libtest runs each test on a thread named after it.
    let thread = std::thread::current();
    let test = thread.name().unwrap_or_default();
    let exe = std::env::current_exe().ok();
    let binary = exe.as_deref().and_then(Path::file_stem).map(|stem| binary_name(&stem.to_string_lossy()).to_owned()).unwrap_or_default();
    append(Path::new(&log), &entry(need, reason, test, &binary, file, line));
}

/// Appends `line` with a single write, so that the lines of tests running in parallel don't interleave.
fn append(path: &Path, line: &str) {
    let written = OpenOptions::new().create(true).append(true).open(path).and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(e) = written {
        eprintln!("{SKIP_LOG_ENV}: cannot append to {}: {e}", path.display());
    }
}

/// A test binary's name without the hash that cargo appends (`florastack_gpu-0123456789abcdef` -> `florastack_gpu`).
fn binary_name(stem: &str) -> &str {
    match stem.rsplit_once('-') {
        Some((name, hash)) if !name.is_empty() && hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => name,
        _ => stem,
    }
}

/// One skip-log line: a JSON object and a newline.
fn entry(need: Need, reason: &str, test: &str, binary: &str, file: &str, line: u32) -> String {
    let location = format!("{}:{line}", file.replace('\\', "/"));
    let fields = [("need", need.as_str()), ("reason", reason), ("test", test), ("binary", binary), ("location", &location)];
    let fields: Vec<String> = fields.iter().map(|(key, value)| format!("\"{key}\":\"{}\"", escape(value))).collect();
    format!("{{{}}}\n", fields.join(","))
}

/// `s` as the contents of a JSON string.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_one_json_line_with_its_text_escaped() {
        let line = entry(Need::Corpus, "C:\\corpus\\bee \"missing\"\n\u{1}", "tests::bee", "florastack_align", "crates\\align\\src\\lib.rs", 42);
        assert_eq!(
            line,
            "{\"need\":\"corpus\",\"reason\":\"C:\\\\corpus\\\\bee \\\"missing\\\"\\n\\u0001\",\"test\":\"tests::bee\",\
             \"binary\":\"florastack_align\",\"location\":\"crates/align/src/lib.rs:42\"}\n"
        );
        assert_eq!(line.matches('\n').count(), 1, "{line}");
    }

    #[test]
    fn need_names_are_stable() {
        // `cargo xtask ci` groups skips by these names and has a hint for each (xtask/src/ci.rs).
        assert_eq!([Need::Gpu.as_str(), Need::Corpus.as_str()], ["gpu", "corpus"]);
    }

    #[test]
    fn binary_names_lose_only_cargos_hash() {
        assert_eq!(binary_name("florastack_gpu-0f3a9c2b1d4e5f60"), "florastack_gpu");
        assert_eq!(binary_name("agreement-0f3a9c2b1d4e5f60"), "agreement");
        assert_eq!(binary_name("real_stacks"), "real_stacks");
        assert_eq!(binary_name("real-stacks"), "real-stacks");
        assert_eq!(binary_name("-0f3a9c2b1d4e5f60"), "-0f3a9c2b1d4e5f60");
        assert_eq!(binary_name("tool-0f3a9c2b1d4e5f6g"), "tool-0f3a9c2b1d4e5f6g");
    }

    #[test]
    fn lines_appended_from_parallel_tests_stay_whole() {
        let dir = std::env::temp_dir().join(format!("florastack-testkit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("skips.jsonl");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let log = log.clone();
                std::thread::spawn(move || append(&log, &entry(Need::Gpu, &format!("no adapter {i}"), "t", "b", "f.rs", i)))
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let text = std::fs::read_to_string(&log).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(text.lines().count(), 8, "{text}");
        assert!(text.lines().all(|l| l.starts_with("{\"need\":\"gpu\",") && l.ends_with('}')), "{text}");
    }

    #[test]
    fn an_unwritable_log_does_not_fail_the_test() {
        append(Path::new("/no-such-florastack-directory/skips.jsonl"), "{}\n");
    }
}
