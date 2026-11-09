//! `cargo xtask web`: the browser build (roadmap 7.3), a static site in `target/web-site/` (`--out` to choose).
//!
//! The desktop app's library is built for `wasm32-unknown-unknown` with threads: atomics and shared memory, which
//! take a nightly toolchain ([`NIGHTLY`], the one the fuzz targets use) to rebuild the standard library with them,
//! and up to 4 GiB of memory. wasm-bindgen's CLI, at the version `Cargo.lock` pins, makes the JavaScript glue, and the
//! page and the worker script come from `packaging/web/`, filled with the product identity. Then:
//!
//! - `--serve [PORT]` serves the site on `127.0.0.1` (8080 by default) with the headers it needs ([`HEADERS`]): the
//!   cross-origin isolation (COOP and COEP) that shared memory, and so threads, require.
//! - `--test` runs the headless browser test: the engine's `web_stack` example writes a 20-frame synthetic stack and
//!   stacks it natively; `apps/florastack/tests/web/stack.mjs` (Playwright, installed outside the repository at the
//!   version [`PLAYWRIGHT`]) gives the page the frames through its file input, aligns and stacks them by the pyramid
//!   and downloads the output (and on WebGPU, stacks them again on the CPU's threads); and each output must agree with
//!   the native one.
//!
//! The site server is std only, adapted from LightCraft's (`xtask/src/web.rs`, MIT OR Apache-2.0; ATTRIBUTION.md).

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::branding::Branding;

/// The toolchain that builds the site: a nightly, for `-Z build-std`. The fuzz targets use the same one
/// (`fuzz/rust-toolchain.toml`); move them together.
pub const NIGHTLY: &str = "nightly-2026-11-01";
const TARGET: &str = "wasm32-unknown-unknown";
/// Threads: atomics, and a memory that the module imports, shared, up to the most a page may have, 4 GiB; and the
/// thread-local storage exports that wasm-bindgen's glue starts each thread with. The compiler passes the linker
/// none of these by itself.
const RUSTFLAGS: [&str; 8] = [
    "-C target-feature=+atomics,+bulk-memory,+mutable-globals",
    "-C link-arg=--shared-memory",
    "-C link-arg=--import-memory",
    "-C link-arg=--max-memory=4294967296",
    "-C link-arg=--export=__wasm_init_tls",
    "-C link-arg=--export=__tls_size",
    "-C link-arg=--export=__tls_align",
    "-C link-arg=--export=__tls_base",
];
/// The profile of the optimized build (`[profile.web]`); `--dev` builds with `dev`.
const PROFILE: &str = "web";
/// The site's folder under the target directory. Not a profile's name: a cross build writes the host's artifacts of
/// the profile it builds to `<target>/<profile>/`.
const SITE: &str = "web-site";
/// Playwright's library, without its test runner or browsers: the test drives the Chrome or Edge installed.
pub const PLAYWRIGHT: &str = "1.64.1";

/// The headers every file of the site is served with. `Cross-Origin-Opener-Policy: same-origin` and
/// `Cross-Origin-Embedder-Policy: require-corp` make the page cross-origin isolated, which `SharedArrayBuffer`, and so
/// the app's threads, need; `Cross-Origin-Resource-Policy` lets such a page load the site's own files. The files' names
/// don't change from version to version, so browsers must check them again (`no-cache`), lest they keep an old module.
pub const HEADERS: [(&str, &str); 5] = [
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Embedder-Policy", "require-corp"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    ("X-Content-Type-Options", "nosniff"),
    ("Cache-Control", "no-cache"),
];

const HELP: &str = "\
usage: cargo xtask web [--clippy] [--dev] [--out DIR] [--no-build] [--serve [PORT] | --test]

Builds the browser version of the desktop app into target/web-site (or DIR): a static site, which any web server
can serve with the headers in packaging/web/README.md.

  --clippy     first run Clippy on the crates the browser build compiles, for wasm32 with threads
  --dev        build with the dev profile: quicker to build, slower to stack
  --out DIR    write the site to DIR
  --no-build   use the site already built
  --serve [P]  then serve it on http://127.0.0.1:P/ (8080) with the headers it needs, until stopped
  --test       then run the headless browser test: a 20-frame synthetic stack, stacked in the page and compared
               with the same stack stacked natively (Node.js and Chrome or Edge; FLORASTACK_BROWSER names another
               channel, or a browser's path)

Needs the nightly toolchain with its standard library's source and the wasm-bindgen CLI at Cargo.lock's version:
  rustup toolchain install nightly-2026-11-01 --profile minimal --component rust-src
  cargo install wasm-bindgen-cli --version <the version in Cargo.lock> --locked
";

#[derive(Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per option")]
struct Args {
    dev: bool,
    clippy: bool,
    out: Option<PathBuf>,
    build: bool,
    serve: Option<u16>,
    test: bool,
}

fn parse(args: &[&str]) -> Result<Option<Args>, String> {
    let mut parsed = Args { build: true, ..Args::default() };
    let mut i = 0;
    while let Some(&arg) = args.get(i) {
        match arg {
            "--dev" => parsed.dev = true,
            "--clippy" => parsed.clippy = true,
            "--no-build" => parsed.build = false,
            "--test" => parsed.test = true,
            "--out" => {
                i += 1;
                parsed.out = Some(PathBuf::from(args.get(i).ok_or("--out needs a folder")?));
            }
            "--serve" => {
                let port = args.get(i + 1).and_then(|p| p.parse::<u16>().ok());
                if port.is_some() {
                    i += 1;
                }
                parsed.serve = Some(port.unwrap_or(8080));
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument `{other}`\n\n{HELP}")),
        }
        i += 1;
    }
    if parsed.test && parsed.serve.is_some() {
        return Err("--serve and --test don't go together".to_owned());
    }
    Ok(Some(parsed))
}

pub fn run(meta: &serde_json::Value, args: &[&str]) -> Result<(), String> {
    let Some(args) = parse(args)? else {
        print!("{HELP}");
        return Ok(());
    };
    let root = crate::workspace_root()?;
    let target_dir = PathBuf::from(meta["target_directory"].as_str().ok_or("cargo metadata has no target_directory")?);
    let site = args.out.clone().unwrap_or_else(|| target_dir.join(SITE));
    if args.clippy {
        clippy(&root)?;
    }
    if args.build {
        build(meta, &root, &target_dir, &site, args.dev)?;
    } else if !site.join("index.html").is_file() {
        return Err(format!("{} has no site: build it first, without --no-build", site.display()));
    }
    if let Some(port) = args.serve {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
        println!("serving {} at http://127.0.0.1:{port}/ (Ctrl+C stops)", site.display());
        serve(&listener, &site);
        return Ok(());
    }
    if args.test {
        return test(&root, &target_dir, &site);
    }
    println!("serve it with `cargo xtask web --no-build --serve`, or any web server with the headers packaging/web/README.md lists");
    Ok(())
}

/// The version of `package` that `Cargo.lock` pins.
fn locked(root: &Path, package: &str) -> Result<String, String> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == format!("name = \"{package}\"")
            && let Some(version) = lines.next().and_then(|v| v.trim().strip_prefix("version = \""))
        {
            return Ok(version.trim_end_matches('"').to_owned());
        }
    }
    Err(format!("{package} isn't in Cargo.lock"))
}

/// Checks that the nightly toolchain, its standard library's source and the wasm-bindgen CLI are there.
fn check_tools(root: &Path) -> Result<(), String> {
    let install = format!("install it with:\n    rustup toolchain install {NIGHTLY} --profile minimal --component rust-src");
    let sysroot = Command::new("rustc")
        .arg(format!("+{NIGHTLY}"))
        .args(["--print", "sysroot"])
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .map_err(|e| format!("cannot run rustc ({e}): the browser build needs rustup's {NIGHTLY} toolchain; {install}"))?;
    if !sysroot.status.success() {
        return Err(format!("the browser build needs the {NIGHTLY} toolchain; {install}"));
    }
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    if !sysroot.join("lib/rustlib/src/rust/library/std").is_dir() {
        return Err(format!("the browser build needs {NIGHTLY}'s rust-src component; {install}"));
    }
    let want = locked(root, "wasm-bindgen")?;
    let hint = format!("install it with:\n    cargo install wasm-bindgen-cli --version {want} --locked");
    let out = Command::new("wasm-bindgen").arg("--version").output().map_err(|_| format!("the wasm-bindgen CLI isn't installed; {hint}"))?;
    let have = String::from_utf8_lossy(&out.stdout);
    let have = have.split_whitespace().nth(1).unwrap_or("?");
    if have != want {
        return Err(format!("the wasm-bindgen CLI is {have}, but Cargo.lock has {want}; {hint}"));
    }
    Ok(())
}

/// The crates the browser build compiles, which Clippy checks for its target.
const CRATES: [&str; 12] = [
    "florastack-core",
    "florastack-codecs",
    "florastack-raw",
    "florastack-align",
    "florastack-stack",
    "florastack-gpu",
    "florastack-project",
    "florastack-retouch",
    "florastack-edit",
    "florastack-capture",
    "florastack-engine",
    "florastack",
];

/// Cargo on the nightly, for the browser's target with threads: rustup's cargo, not the stable one that runs xtask.
fn nightly_cargo(root: &Path, subcommand: &str) -> Command {
    let mut cargo = Command::new("cargo");
    cargo
        .arg(format!("+{NIGHTLY}"))
        .args([subcommand, "--locked", "--target", TARGET, "-Z", "build-std=panic_abort,std"])
        .current_dir(root)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTC")
        .env_remove("CARGO")
        .env("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS", RUSTFLAGS.join(" "));
    cargo
}

/// Clippy, with warnings as errors, on [`CRATES`] for the browser's target: the code only it compiles.
fn clippy(root: &Path) -> Result<(), String> {
    check_tools(root)?;
    let mut clippy = nightly_cargo(root, "clippy");
    for name in CRATES {
        clippy.args(["-p", name]);
    }
    // Two lints the nightly's Clippy has and the workspace's pinned one doesn't yet, which `cargo clippy` checks natively.
    clippy.args(["--lib", "--bins", "--", "-D", "warnings", "-A", "clippy::map_unwrap_or", "-A", "clippy::unnecessary_lazy_evaluations"]);
    step(clippy, &format!("Clippy for {TARGET} ({NIGHTLY})"))
}

fn step(mut command: Command, what: &str) -> Result<(), String> {
    println!("==> {what}");
    let status = command.status().map_err(|e| format!("{what}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what} failed ({status})")) }
}

/// The app's package version, from cargo metadata.
fn version(meta: &serde_json::Value, branding: &Branding) -> Result<String, String> {
    let packages = meta["packages"].as_array().ok_or("cargo metadata has no packages")?;
    packages
        .iter()
        .find(|p| p["name"] == branding.binaries.gui.as_str())
        .and_then(|p| p["version"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("no package {}", branding.binaries.gui))
}

fn build(meta: &serde_json::Value, root: &Path, target_dir: &Path, site: &Path, dev: bool) -> Result<(), String> {
    check_tools(root)?;
    let branding = crate::branding::load(root)?;
    let profile = if dev { "dev" } else { PROFILE };
    let mut cargo = nightly_cargo(root, "rustc");
    cargo.args(["-p", &branding.binaries.gui, "--lib", "--crate-type", "cdylib", "--profile", profile]);
    step(cargo, &format!("build the app for {TARGET} ({profile} profile, {NIGHTLY})"))?;

    let wasm = target_dir.join(TARGET).join(if dev { "debug" } else { PROFILE }).join(format!("{}_app.wasm", branding.binaries.gui));
    if site.exists() {
        std::fs::remove_dir_all(site).map_err(|e| format!("cannot empty {}: {e}", site.display()))?;
    }
    std::fs::create_dir_all(site).map_err(|e| format!("{}: {e}", site.display()))?;
    let mut bindgen = Command::new("wasm-bindgen");
    bindgen.arg(&wasm).args(["--target", "web", "--no-typescript", "--out-name", &branding.binaries.gui, "--out-dir"]).arg(site);
    if dev {
        bindgen.arg("--debug");
    }
    step(bindgen, "wasm-bindgen")?;

    let version = version(meta, &branding)?;
    let values = [("DISPLAY_NAME", branding.display_name.as_str()), ("GUI_BINARY", branding.binaries.gui.as_str()), ("VERSION", version.as_str())];
    for name in ["index.html", "worker.js"] {
        let template = crate::dist::read_template(&root.join("packaging/web").join(format!("{name}.in")))?;
        let text = crate::dist::fill(&template, &values).map_err(|e| format!("packaging/web/{name}.in: {e}"))?;
        crate::dist::write(&site.join(name), text.as_bytes())?;
    }
    // Sample header rules for hosts that read them (Netlify and Cloudflare Pages; Apache); harmless elsewhere.
    for name in ["_headers", ".htaccess"] {
        crate::dist::copy(&root.join("packaging/web").join(name), &site.join(name))?;
    }
    crate::dist::write(&site.join("icon.png"), &crate::dist::icon::png(192)?)?;
    for name in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"] {
        crate::dist::copy(&root.join(name), &site.join(name))?;
    }
    let bytes = |name: &str| std::fs::metadata(site.join(name)).map_or(0, |m| m.len());
    println!(
        "the site is ready in {} ({:.1} MB of WebAssembly, {:.1} MB in all)",
        site.display(),
        bytes(&format!("{}_bg.wasm", branding.binaries.gui)) as f64 / 1e6,
        dir_size(site) as f64 / 1e6
    );
    Ok(())
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| e.metadata().ok()).filter(std::fs::Metadata::is_file).map(|m| m.len()).sum()
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("LICENSE") || n == "NOTICE") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file `target` (a request's path) names in `dir`; `None` for one outside it.
fn resolve(dir: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() || rel.ends_with('/') { format!("{rel}index.html") } else { rel.to_owned() };
    let safe = rel.split('/').all(|c| !c.is_empty() && c != ".." && c != "." && !c.contains(['\\', ':']));
    safe.then(|| dir.join(rel))
}

/// Serves `dir` on `listener` (GET and HEAD, no listings), a thread per connection, until the process ends.
pub fn serve(listener: &TcpListener, dir: &Path) {
    for stream in listener.incoming().flatten() {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = respond(stream, &dir) {
                eprintln!("web server: {e}");
            }
        });
    }
}

fn respond(mut stream: TcpStream, dir: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let file = resolve(dir, target);
    let body = file.as_ref().and_then(|f| std::fs::read(f).ok());
    let (status, body, kind) = match (method, body) {
        ("GET" | "HEAD", Some(body)) => ("200 OK", body, file.as_deref().map_or("application/octet-stream", mime)),
        ("GET" | "HEAD", None) => ("404 Not Found", b"not found".to_vec(), "text/plain; charset=utf-8"),
        _ => ("405 Method Not Allowed", Vec::new(), "text/plain; charset=utf-8"),
    };
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in HEADERS {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if method != "HEAD" {
        stream.write_all(&body)?;
    }
    stream.flush()
}

/// npm, which is a script on Windows.
fn npm() -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/c", "npm"]);
        c
    } else {
        Command::new("npm")
    }
}

/// Playwright's library, installed (once) outside the repository, as the glTF validator is.
fn playwright() -> Result<PathBuf, String> {
    let folder = std::env::temp_dir().join(format!("florastack-playwright-{PLAYWRIGHT}"));
    let package = folder.join("node_modules/playwright-core/package.json");
    let installed = std::fs::read(&package).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    if installed.is_some_and(|v| v["version"] == PLAYWRIGHT) {
        return Ok(folder);
    }
    let mut install = npm();
    install
        .args(["install", "--prefix"])
        .arg(&folder)
        .arg("--cache")
        .arg(folder.join("npm-cache"))
        .args(["--ignore-scripts", "--no-audit", "--no-fund", "--save-exact", "--user-agent=FloraStack-tests"])
        .arg(format!("playwright-core@{PLAYWRIGHT}"));
    step(install, &format!("install playwright-core {PLAYWRIGHT} (needs Node.js and npm)"))?;
    Ok(folder)
}

/// The engine's `web_stack` example, natively.
fn example(root: &Path, args: &[&std::ffi::OsStr], what: &str) -> Result<(), String> {
    let mut command = crate::cargo();
    command.args(["run", "--locked", "-q", "-p", "florastack-engine", "--example", "web_stack", "--"]).args(args).current_dir(root);
    step(command, what)
}

fn test(root: &Path, target_dir: &Path, site: &Path) -> Result<(), String> {
    let work = target_dir.join("web-test");
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| format!("cannot empty {}: {e}", work.display()))?;
    }
    let frames = work.join("frames");
    let downloads = work.join("downloads");
    let reference = work.join("reference.png");
    example(root, &["frames".as_ref(), frames.as_os_str()], "write the 20-frame synthetic stack")?;
    example(root, &["reference".as_ref(), frames.as_os_str(), reference.as_os_str()], "stack it natively, for reference")?;

    let modules = playwright()?;
    let script = modules.join("stack.mjs");
    crate::dist::copy(&root.join("apps/florastack/tests/web/stack.mjs"), &script)?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let served = site.to_path_buf();
    std::thread::spawn(move || serve(&listener, &served));
    let browser = std::env::var("FLORASTACK_BROWSER").unwrap_or_else(|_| if cfg!(windows) { "msedge" } else { "chrome" }.to_owned());
    let mut node = Command::new("node");
    node.arg(&script)
        .current_dir(&modules)
        .env("FLORASTACK_URL", format!("http://127.0.0.1:{port}/"))
        .env("FLORASTACK_FRAMES", &frames)
        .env("FLORASTACK_DOWNLOADS", &downloads)
        .env("FLORASTACK_BROWSER", &browser);
    step(node, &format!("stack it in the page, headless ({browser})"))?;
    if let Ok(timings) = std::fs::read_to_string(downloads.join("timings.json")) {
        println!("{}", timings.trim());
    }
    // The page's stack, and on WebGPU its stack on the CPU too.
    for name in ["web", "web-cpu"] {
        let web = downloads.join(format!("{name}.png"));
        if name == "web" || web.is_file() {
            example(
                root,
                &["compare".as_ref(), reference.as_os_str(), web.as_os_str()],
                &format!("compare the page's {name}.png with the native stack"),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]).unwrap(), Some(Args { build: true, ..Args::default() }));
        let a = parse(&["--dev", "--serve", "9000", "--out", "x"]).unwrap().unwrap();
        assert!(a.dev && a.serve == Some(9000) && a.out == Some(PathBuf::from("x")) && !a.clippy);
        assert!(parse(&["--clippy", "--test"]).unwrap().unwrap().clippy);
        assert_eq!(parse(&["--serve"]).unwrap().unwrap().serve, Some(8080));
        assert!(!parse(&["--no-build", "--test"]).unwrap().unwrap().build);
        assert!(parse(&["--serve", "--test"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert_eq!(parse(&["--help"]).unwrap(), None);
    }

    #[test]
    fn the_nightly_is_the_fuzz_targets() {
        let fuzz: toml::Value = toml::from_str(include_str!("../../fuzz/rust-toolchain.toml")).unwrap();
        assert_eq!(fuzz["toolchain"]["channel"].as_str(), Some(NIGHTLY), "move fuzz/rust-toolchain.toml and web::NIGHTLY together");
    }

    #[test]
    fn the_wasm_bindgen_cli_matches_the_library() {
        let root = crate::workspace_root().unwrap();
        let workspace: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
        let pinned = workspace["workspace"]["dependencies"]["wasm-bindgen"].as_str().unwrap();
        assert_eq!(pinned.strip_prefix('='), Some(locked(&root, "wasm-bindgen").unwrap().as_str()), "the CLI must match the library exactly");
    }

    #[test]
    fn requests_stay_in_the_site() {
        let dir = Path::new("site");
        assert_eq!(resolve(dir, "/"), Some(dir.join("index.html")));
        assert_eq!(resolve(dir, "/florastack_bg.wasm?v=1"), Some(dir.join("florastack_bg.wasm")));
        assert_eq!(resolve(dir, "/sub/"), Some(dir.join("sub/index.html")));
        for hostile in ["/../secret", "/a/../../b", "/C:/Windows", "/a\\..\\b", "/./x"] {
            assert_eq!(resolve(dir, hostile), None, "{hostile}");
        }
        assert_eq!(mime(Path::new("florastack_bg.wasm")), "application/wasm");
        assert_eq!(mime(Path::new("worker.js")), "text/javascript; charset=utf-8");
        assert_eq!(mime(Path::new("NOTICE")), "text/plain; charset=utf-8");
    }

    #[test]
    fn the_site_is_cross_origin_isolated_however_it_is_served() {
        let isolation = [("Cross-Origin-Opener-Policy", "same-origin"), ("Cross-Origin-Embedder-Policy", "require-corp")];
        let root = crate::workspace_root().unwrap();
        let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
        for (name, value) in isolation {
            assert!(HEADERS.contains(&(name, value)));
            for config in ["packaging/web/_headers", "packaging/web/.htaccess", "packaging/web/nginx.conf"] {
                let text = read(config);
                assert!(text.contains(name) && text.contains(value), "{config} must send {name}: {value}");
            }
            assert!(read("packaging/web/README.md").contains(&format!("{name}: {value}")), "the hosting notes must name {name}");
        }
    }

    #[test]
    fn the_templates_fill() {
        let root = crate::workspace_root().unwrap();
        let values = [("DISPLAY_NAME", "X"), ("GUI_BINARY", "x"), ("VERSION", "1")];
        for name in ["index.html.in", "worker.js.in"] {
            let template = crate::dist::read_template(&root.join("packaging/web").join(name)).unwrap();
            let text = crate::dist::fill(&template, &values).unwrap();
            assert!(text.contains("./x.js"), "{name} loads the module by the binary's name");
            assert!(!text.contains("src=\"/") && !text.contains("href=\"/"), "{name}: every URL is relative");
        }
    }

    #[test]
    fn the_dockerfile_builds_with_the_same_tools() {
        let root = crate::workspace_root().unwrap();
        let dockerfile = std::fs::read_to_string(root.join("Dockerfile")).unwrap();
        assert!(dockerfile.contains(&format!("rustup toolchain install {NIGHTLY} ")), "the Dockerfile's nightly is web::NIGHTLY");
        let bindgen = locked(&root, "wasm-bindgen").unwrap();
        assert!(dockerfile.contains(&format!("wasm-bindgen-cli --version {bindgen} ")), "the Dockerfile's wasm-bindgen CLI is Cargo.lock's");
        let toolchain: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("rust-toolchain.toml")).unwrap()).unwrap();
        let channel = toolchain["toolchain"]["channel"].as_str().unwrap();
        assert!(dockerfile.contains(&format!("FROM rust:{channel}-")), "the Dockerfile's Rust image is rust-toolchain.toml's {channel}");
        let workflow = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap();
        assert!(workflow.contains(&format!("rustup toolchain install {NIGHTLY} ")), "CI installs web::NIGHTLY");
        assert!(workflow.contains(&format!("download/{bindgen}/wasm-bindgen-{bindgen}-")), "CI installs Cargo.lock's wasm-bindgen CLI");
    }
}
