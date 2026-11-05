//! `cargo xtask dist`: the release packages of one target, named from `branding.toml` (display name, binary names,
//! app ID) and the workspace version, written with `SHA256SUMS.txt` into one folder. `.github/workflows/release.yml`
//! runs it once per target on a version tag and publishes what the runs made; book/src/dev/packaging.md is the guide.
//!
//! The apps are built with the `dist` profile (the release profile, stripped), with camera control (`gphoto`) on
//! Linux and macOS only. Each package format is a function of the platform's module; they share the staging helpers
//! here. Third-party sources and tools come from `packaging/third-party.toml`, pinned by their SHA-256 (`pins`).

pub(crate) mod icon;
mod linux;
mod macos;
mod pins;
mod windows;

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::branding::{self, Branding};
use crate::sha256;

pub const USAGE: &str = "\
usage: cargo xtask dist [--target TARGET] [--formats LIST] [--out DIR] [--tag TAG]

Builds the release packages of one target from the identity in branding.toml and the workspace version, and
writes them with SHA256SUMS.txt into one folder (book/src/dev/packaging.md). Refuses an app ID that still contains the
OWNER placeholder.

  --target TARGET  x86_64-pc-windows-msvc, aarch64-pc-windows-msvc, universal-apple-darwin, aarch64-apple-darwin,
                   x86_64-apple-darwin, x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu (default: this
                   machine's)
  --formats LIST   only these formats, separated by commas (default: all of the target's):
                     Windows  msi, zip, cli, wheel
                     macOS    dmg, cli, wheel
                     Linux    tarball, deb, rpm, appimage, flatpak, cli, wheel
  --out DIR        the folder for the packages, which must be empty or missing (default:
                   target/dist/<target>/artifacts, emptied first)
  --tag TAG        fail unless TAG is v<version> or v<version>-<suffix>, as the release workflow passes it
  -h, --help       show this message

Each format needs its tools: cargo-wix for msi (xtask downloads the pinned WiX Toolset); Xcode's command-line tools
and pkg-config for dmg; dpkg-deb for deb, rpmbuild for rpm, flatpak, flatpak-builder and elfutils' eu-strip for
flatpak (xtask downloads the pinned appimagetool); Python 3.9+ for wheel (PYO3_PYTHON selects it). macOS signing and notarization
run when APPLE_SIGNING_IDENTITY, and APPLE_NOTARY_KEY_PATH, APPLE_NOTARY_KEY_ID and APPLE_NOTARY_ISSUER, are set.
";

/// The Cargo profile of the shipped binaries (`[profile.dist]` in the root Cargo.toml).
const PROFILE: &str = "dist";

/// The oldest macOS the apps and the bundled libraries are built for.
pub const MACOS_MINIMUM: &str = "11.0";

/// The files every package carries next to the binaries.
const LICENSE_FILES: [&str; 3] = ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"];

const SUMS: &str = "SHA256SUMS.txt";

/// What the app is, in one line: package summaries.
pub const SUMMARY: &str = "Focus stacking for macro photography";

/// What the app is, in a paragraph: package descriptions.
pub fn description(display_name: &str) -> String {
    format!(
        "{display_name} aligns photos focused at different distances and merges the sharp parts of each into one image with more depth of field \
         than any single exposure, for macro, microscopy, product and landscape photography. It stacks by weighted average, pyramid or depth \
         map, on the GPU when there is one, retouches the result from its frames, and exports TIFF, PNG, JPEG, DNG or OpenEXR. A \
         command-line tool comes with it, and it controls cameras and focus rails."
    )
}

/// libgphoto2's configure options, the same for the macOS app and the Flatpak: only the PTP driver, which speaks to
/// the cameras whose focus can be driven remotely, and none of the optional libraries (EXIF, JPEG, XML, curl, gd,
/// TIFF, translations).
pub const GPHOTO_CONFIGURE: [&str; 9] = [
    "--with-camlibs=ptp2",
    "--disable-nls",
    "--disable-static",
    "--without-jpeg",
    "--with-libexif=no",
    "--with-libxml-2.0=no",
    "--with-libcurl=no",
    "--with-gdlib=no",
    "--with-libtiff-4=no",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    /// Both macOS architectures in each file.
    Universal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
}

const TARGETS: [(&str, Os, Arch); 7] = [
    ("x86_64-pc-windows-msvc", Os::Windows, Arch::X86_64),
    ("aarch64-pc-windows-msvc", Os::Windows, Arch::Aarch64),
    ("universal-apple-darwin", Os::Macos, Arch::Universal),
    ("aarch64-apple-darwin", Os::Macos, Arch::Aarch64),
    ("x86_64-apple-darwin", Os::Macos, Arch::X86_64),
    ("x86_64-unknown-linux-gnu", Os::Linux, Arch::X86_64),
    ("aarch64-unknown-linux-gnu", Os::Linux, Arch::Aarch64),
];

impl Target {
    pub fn parse(triple: &str) -> Result<Self, String> {
        TARGETS.iter().find(|(name, ..)| *name == triple).map(|&(_, os, arch)| Target { os, arch }).ok_or_else(|| {
            let names: Vec<&str> = TARGETS.iter().map(|(name, ..)| *name).collect();
            format!("dist doesn't build for `{triple}`: it builds for {}", names.join(", "))
        })
    }

    pub fn triple(self) -> &'static str {
        TARGETS.iter().find(|(_, os, arch)| (*os, *arch) == (self.os, self.arch)).map_or("", |(name, ..)| name)
    }

    /// The Rust targets to compile: both macOS ones for a universal build.
    pub fn rust_triples(self) -> Vec<&'static str> {
        if self.arch == Arch::Universal { vec!["aarch64-apple-darwin", "x86_64-apple-darwin"] } else { vec![self.triple()] }
    }

    /// The platform part of the packages' file names, in each platform's own words: `windows-x64`, `macos-universal`,
    /// `linux-aarch64`.
    pub fn label(self) -> String {
        let arch = match (self.os, self.arch) {
            (Os::Windows, Arch::X86_64) => "x64",
            (Os::Windows | Os::Macos, Arch::Aarch64) => "arm64",
            (_, Arch::X86_64) => "x86_64",
            (_, Arch::Aarch64) => "aarch64",
            (_, Arch::Universal) => "universal",
        };
        let os = match self.os {
            Os::Windows => "windows",
            Os::Macos => "macos",
            Os::Linux => "linux",
        };
        format!("{os}-{arch}")
    }

    pub fn formats(self) -> &'static [Format] {
        match self.os {
            Os::Windows => &[Format::Msi, Format::Zip, Format::Cli, Format::Wheel],
            Os::Macos => &[Format::Dmg, Format::Cli, Format::Wheel],
            Os::Linux => &[Format::Tarball, Format::Deb, Format::Rpm, Format::AppImage, Format::Flatpak, Format::Cli, Format::Wheel],
        }
    }

    /// Camera control through libgphoto2: Linux and macOS only, never Windows.
    pub fn gphoto(self) -> bool {
        self.os != Os::Windows
    }

    pub fn exe(self, name: &str) -> String {
        if self.os == Os::Windows { format!("{name}.exe") } else { name.to_owned() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Msi,
    Zip,
    Dmg,
    Tarball,
    Deb,
    Rpm,
    AppImage,
    Flatpak,
    /// The command-line tool alone, as a .zip or .tar.gz.
    Cli,
    /// The Python bindings, as a wheel.
    Wheel,
}

const FORMATS: [(&str, Format); 10] = [
    ("msi", Format::Msi),
    ("zip", Format::Zip),
    ("dmg", Format::Dmg),
    ("tarball", Format::Tarball),
    ("deb", Format::Deb),
    ("rpm", Format::Rpm),
    ("appimage", Format::AppImage),
    ("flatpak", Format::Flatpak),
    ("cli", Format::Cli),
    ("wheel", Format::Wheel),
];

impl Format {
    fn name(self) -> &'static str {
        FORMATS.iter().find(|(_, format)| *format == self).map_or("", |(name, _)| name)
    }
}

/// The formats a `--formats` list names, in the target's order.
fn parse_formats(list: &str, target: Target) -> Result<Vec<Format>, String> {
    let mut chosen = Vec::new();
    for name in list.split(',').map(str::trim).filter(|name| !name.is_empty()) {
        let format = FORMATS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f).ok_or_else(|| format!("unknown format `{name}`"))?;
        if !target.formats().contains(&format) {
            let names: Vec<&str> = target.formats().iter().map(|f| f.name()).collect();
            return Err(format!("{} has no `{name}` packages: it has {}", target.triple(), names.join(", ")));
        }
        chosen.push(format);
    }
    if chosen.is_empty() {
        return Err("--formats names no format".to_owned());
    }
    Ok(target.formats().iter().copied().filter(|f| chosen.contains(f)).collect())
}

#[derive(Debug, Default, PartialEq)]
struct Options<'a> {
    help: bool,
    target: Option<&'a str>,
    formats: Option<&'a str>,
    out: Option<&'a str>,
    tag: Option<&'a str>,
}

fn parse_args<'a>(args: &[&'a str]) -> Result<Options<'a>, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        let mut value = |name: &str| args.next().copied().ok_or_else(|| format!("{name} needs a value"));
        match arg {
            "-h" | "--help" => options.help = true,
            "--target" => options.target = Some(value(arg)?),
            "--formats" => options.formats = Some(value(arg)?),
            "--out" => options.out = Some(value(arg)?),
            "--tag" => options.tag = Some(value(arg)?),
            _ => return Err(format!("unknown argument `{arg}`\n\n{USAGE}")),
        }
    }
    Ok(options)
}

/// Refuses an identity that can't ship: the OWNER placeholder, or names that some package format can't hold.
pub fn check_identity(branding: &Branding) -> Result<(), String> {
    let id = &branding.app_id;
    if id.contains("OWNER") {
        return Err(format!(
            "branding.toml's app_id `{id}` still contains the OWNER placeholder: set it to the GitHub user or organization that publishes the release"
        ));
    }
    // Reverse-DNS, as Flatpak and macOS bundle identifiers require: three or more segments of ASCII letters, digits
    // and underscores (hyphens too, except in the last), none starting with a digit.
    let segments: Vec<&str> = id.split('.').collect();
    let segment_ok = |s: &str, last: bool| {
        !s.is_empty()
            && !s.starts_with(|c: char| c.is_ascii_digit())
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || (c == '-' && !last))
    };
    if segments.len() < 3 || id.len() > 255 || segments.iter().enumerate().any(|(i, s)| !segment_ok(s, i + 1 == segments.len())) {
        return Err(format!("branding.toml's app_id `{id}` is not a reverse-DNS name such as io.github.owner.App"));
    }
    // The binary names are also the Debian and RPM package names.
    for name in branding.binary_names() {
        if !name.starts_with(|c: char| c.is_ascii_lowercase()) || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(format!("branding.toml's binary name `{name}` must be lower-case letters, digits and hyphens, starting with a letter"));
        }
    }
    let display = &branding.display_name;
    if display.trim().is_empty() || display.trim() != display || display.chars().any(|c| c.is_control() || "/\\:*?\"<>|".contains(c)) {
        return Err(format!("branding.toml's display_name `{display}` can't name files: no surrounding spaces, control characters or /\\:*?\"<>|"));
    }
    Ok(())
}

/// Checks a release tag against the version: `v0.1.0` releases 0.1.0, and `v0.1.0-rc.1` (any suffix) is a
/// pre-release of it, such as a test tag. Returns whether it is a pre-release.
pub fn check_tag(tag: &str, version: &str) -> Result<bool, String> {
    let rest = tag.strip_prefix('v').and_then(|rest| rest.strip_prefix(version));
    match rest {
        Some("") => Ok(false),
        Some(suffix)
            if suffix.len() > 1 && suffix.starts_with('-') && suffix[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') =>
        {
            Ok(true)
        }
        _ => Err(format!("the tag `{tag}` doesn't release version {version} (Cargo.toml's): tag v{version}, or v{version}-<suffix> for a test")),
    }
}

/// The version of the package that builds the binary `bin`, and that package's name.
fn bin_package<'m>(meta: &'m Value, bin: &str) -> Result<(&'m str, &'m Value), String> {
    let packages = meta["packages"].as_array().ok_or("cargo metadata lists no packages")?;
    for package in packages {
        let targets = package["targets"].as_array().map(Vec::as_slice).unwrap_or_default();
        if targets.iter().any(|t| t["name"] == bin && t["kind"].as_array().is_some_and(|kinds| kinds.iter().any(|k| k == "bin"))) {
            let name = package["name"].as_str().ok_or("a package has no name")?;
            return Ok((name, package));
        }
    }
    Err(format!("no workspace package builds the binary `{bin}` that branding.toml names"))
}

/// The host's Rust target, from `rustc -vV`.
fn host_triple() -> Result<String, String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let text = output(Command::new(rustc).arg("-vV"), "ask rustc for the host target")?;
    text.lines().find_map(|line| line.strip_prefix("host: ")).map(str::to_owned).ok_or_else(|| "rustc -vV names no host".to_owned())
}

/// Everything the formats share.
pub struct Dist {
    pub root: PathBuf,
    pub branding: Branding,
    pub version: String,
    pub repository: String,
    pub target: Target,
    /// The Cargo packages that build the two binaries.
    pub gui_package: String,
    pub cli_package: String,
    /// Cargo's target folder.
    pub cargo_target: PathBuf,
    /// Scratch space for this target, `target/dist/<target>/`.
    pub work: PathBuf,
    /// Downloads shared by every target, `target/dist/downloads/`.
    pub downloads: PathBuf,
    pub out: PathBuf,
}

impl Dist {
    /// The start of the app packages' file names: `FloraStack-0.1.0-windows-x64`.
    pub fn stem(&self) -> String {
        format!("{}-{}-{}", self.branding.display_name.replace(' ', "-"), self.version, self.target.label())
    }

    /// The start of the CLI archive's name: `florastack-cli-0.1.0-linux-x86_64`.
    pub fn cli_stem(&self) -> String {
        format!("{}-{}-{}", self.branding.binaries.cli, self.version, self.target.label())
    }

    /// A built binary: Cargo's output, or for a universal target the joined file.
    pub fn binary(&self, name: &str) -> PathBuf {
        if self.target.arch == Arch::Universal {
            self.work.join("universal").join(name)
        } else {
            self.cargo_target.join(self.target.triple()).join(PROFILE).join(self.target.exe(name))
        }
    }

    pub fn gui(&self) -> PathBuf {
        self.binary(&self.branding.binaries.gui)
    }

    pub fn cli(&self) -> PathBuf {
        self.binary(&self.branding.binaries.cli)
    }

    /// The license and notice files, with their names.
    pub fn licenses(&self) -> Vec<(&'static str, PathBuf)> {
        LICENSE_FILES.iter().map(|name| (*name, self.root.join(name))).collect()
    }

    /// `work/<name>`, emptied.
    pub fn fresh(&self, name: &str) -> Result<PathBuf, String> {
        let dir = self.work.join(name);
        fresh_dir(&dir)?;
        Ok(dir)
    }

    /// A template from `packaging/`, filled with the identity and `extra` (see `fill`).
    pub fn template(&self, path: &str, extra: &[(&str, &str)]) -> Result<String, String> {
        let text = read_template(&self.root.join("packaging").join(path))?;
        let b = &self.branding;
        let mut values = vec![
            ("DISPLAY_NAME", b.display_name.as_str()),
            ("APP_ID", b.app_id.as_str()),
            ("GUI_BINARY", b.binaries.gui.as_str()),
            ("CLI_BINARY", b.binaries.cli.as_str()),
            ("VERSION", self.version.as_str()),
            ("REPOSITORY", self.repository.as_str()),
        ];
        values.extend_from_slice(extra);
        fill(&text, &values).map_err(|e| format!("packaging/{path}: {e}"))
    }

    /// The command-line tool with the license files, in `<cli stem>/`, archived.
    fn cli_archive(&self) -> Result<PathBuf, String> {
        let stem = self.cli_stem();
        let dir = self.fresh("cli")?.join(&stem);
        copy(&self.cli(), &dir.join(self.target.exe(&self.branding.binaries.cli)))?;
        for (name, path) in self.licenses() {
            copy(&path, &dir.join(name))?;
        }
        if self.target.os == Os::Windows {
            windows::zip(&dir, &self.out.join(format!("{stem}.zip")))
        } else {
            tar_gz(&dir, &self.out.join(format!("{stem}.tar.gz")))
        }
    }

    fn build_apps(&self) -> Result<(), String> {
        for triple in self.target.rust_triples() {
            let mut cargo = crate::cargo();
            cargo.current_dir(&self.root).args(["build", "--locked", "--profile", PROFILE, "--target", triple]);
            cargo.args(["--package", &self.gui_package, "--package", &self.cli_package]);
            if self.target.gphoto() {
                cargo.args(["--features", &format!("{}/gphoto,{}/gphoto", self.gui_package, self.cli_package)]);
            }
            if self.target.os == Os::Macos {
                cargo.env("MACOSX_DEPLOYMENT_TARGET", MACOS_MINIMUM);
            }
            execute(&mut cargo, &format!("build the apps for {triple} (is the target installed? `rustup target add {triple}`)"))?;
        }
        if self.target.arch == Arch::Universal {
            macos::join_binaries(self)?;
        }
        Ok(())
    }

    /// Builds the Python bindings' wheel with maturin, in a virtual environment of its own. The bindings use the
    /// stable ABI (abi3), so one wheel serves every Python from 3.9.
    fn wheel(&self) -> Result<Vec<PathBuf>, String> {
        let python = crate::python::prepare_environment(&self.work.join("python"))?;
        crate::python::checked(
            Command::new(&python).args(["-m", "pip", "--disable-pip-version-check", "install", crate::python::MATURIN]),
            "install maturin",
        )?;
        let wheels = self.fresh("wheels")?;
        let target = if self.target.arch == Arch::Universal { "universal2-apple-darwin" } else { self.target.triple() };
        let mut maturin = Command::new(&python);
        maturin.args(["-m", "maturin", "build", "--locked", "--profile", PROFILE, "--target", target, "--out"]).arg(&wheels);
        maturin.arg("--manifest-path").arg(self.root.join("python/florastack/Cargo.toml"));
        if self.target.os == Os::Macos {
            maturin.env("MACOSX_DEPLOYMENT_TARGET", MACOS_MINIMUM);
        }
        crate::python::checked(&mut maturin, "build the Python wheel")?;
        let mut built = Vec::new();
        for path in list_files(&wheels)? {
            if path.extension() == Some(OsStr::new("whl")) {
                let to = self.out.join(path.file_name().ok_or("a wheel has no name")?);
                std::fs::rename(&path, &to).map_err(|e| format!("cannot move {}: {e}", path.display()))?;
                built.push(to);
            }
        }
        if built.is_empty() { Err("maturin wrote no wheel".to_owned()) } else { Ok(built) }
    }

    fn package(&self, format: Format, pins: &pins::Pins) -> Result<Vec<PathBuf>, String> {
        let one = |result: Result<PathBuf, String>| result.map(|path| vec![path]);
        match format {
            Format::Msi => one(windows::msi(self, pins)),
            Format::Zip => one(windows::portable(self)),
            Format::Dmg => macos::dmg(self, pins),
            Format::Tarball => one(linux::tarball(self)),
            Format::Deb => one(linux::deb(self)),
            Format::Rpm => one(linux::rpm(self)),
            Format::AppImage => one(linux::appimage(self, pins)),
            Format::Flatpak => linux::flatpak(self, pins),
            Format::Cli => one(self.cli_archive()),
            Format::Wheel => self.wheel(),
        }
    }
}

/// The programs a format needs, each with how to get it.
fn programs(format: Format) -> Vec<(&'static str, &'static str)> {
    match format {
        Format::Msi => vec![("cargo-wix", "cargo install --locked cargo-wix --version 0.3.9")],
        Format::Zip | Format::Tarball | Format::Cli => vec![("tar", "part of Windows 10 and later, macOS and Linux")],
        Format::Dmg => macos::programs(),
        Format::Deb => vec![("dpkg-deb", "sudo apt-get install dpkg")],
        Format::Rpm => vec![("rpmbuild", "sudo apt-get install rpm, or sudo dnf install rpm-build")],
        Format::Flatpak => vec![
            ("flatpak", "https://flatpak.org/setup/"),
            ("flatpak-builder", "sudo apt-get install flatpak-builder"),
            // flatpak-builder strips the modules it builds with it, and only recommends the package.
            ("eu-strip", "sudo apt-get install elfutils"),
        ],
        // appimagetool is a pinned download; the wheel's Python is checked when its environment is made.
        Format::AppImage | Format::Wheel => vec![],
    }
}

pub fn run(meta: &Value, args: &[&str]) -> Result<(), String> {
    let options = parse_args(args)?;
    if options.help {
        print!("{USAGE}");
        return Ok(());
    }
    let root = crate::workspace_root()?;
    let branding = branding::load(&root)?;
    check_identity(&branding)?;
    let (gui_package, gui) = bin_package(meta, &branding.binaries.gui)?;
    let (cli_package, _) = bin_package(meta, &branding.binaries.cli)?;
    let version = gui["version"].as_str().ok_or("the app package has no version")?.to_owned();
    let repository =
        gui["repository"].as_str().ok_or("the app package has no repository (workspace.package.repository)")?.trim_end_matches('/').to_owned();
    check_python_version(&root, &version)?;
    if let Some(tag) = options.tag {
        let prerelease = check_tag(tag, &version)?;
        println!("tag {tag}: {}", if prerelease { "a pre-release" } else { "a release" });
    }
    let triple = match options.target {
        Some(triple) => triple.to_owned(),
        None => host_triple()?,
    };
    let target = Target::parse(&triple)?;
    let formats = match options.formats {
        Some(list) => parse_formats(list, target)?,
        None => target.formats().to_vec(),
    };
    let mut missing = Vec::new();
    for format in &formats {
        for (program, hint) in programs(*format) {
            if find_program(program).is_none() && !missing.iter().any(|(p, _): &(&str, &str)| *p == program) {
                missing.push((program, hint));
            }
        }
    }
    if !missing.is_empty() {
        let list: Vec<String> = missing.iter().map(|(program, hint)| format!("{program} ({hint})")).collect();
        return Err(format!("missing tools for {}: {}; or choose other --formats", target.triple(), list.join(", ")));
    }
    let pins = pins::load(&root)?;

    let cargo_target = meta["target_directory"].as_str().map_or_else(|| root.join("target"), PathBuf::from);
    let work = cargo_target.join("dist").join(target.triple());
    let out = if let Some(dir) = options.out {
        let dir = PathBuf::from(dir);
        if dir.is_dir() && std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?.next().is_some() {
            return Err(format!("--out {} is not empty", dir.display()));
        }
        dir
    } else {
        let dir = work.join("artifacts");
        fresh_dir(&dir)?;
        dir
    };
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let dist = Dist {
        root,
        branding,
        version,
        repository,
        target,
        gui_package: gui_package.to_owned(),
        cli_package: cli_package.to_owned(),
        downloads: cargo_target.join("dist").join("downloads"),
        cargo_target,
        work,
        out,
    };

    if formats.iter().any(|f| *f != Format::Wheel) {
        dist.build_apps()?;
    }
    let mut artifacts = Vec::new();
    for format in formats {
        println!("packaging {} for {}", format.name(), target.triple());
        artifacts.extend(dist.package(format, &pins)?);
    }
    let sums = write_sums(&dist.out, &artifacts)?;
    println!("\n{} packages for {} in {}:", artifacts.len(), target.triple(), dist.out.display());
    for path in artifacts.iter().chain([&sums]) {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or_default();
        println!("  {:<64} {:>8.1} MB", path.file_name().map(OsStr::to_string_lossy).unwrap_or_default(), mb(size));
    }
    Ok(())
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1e6
}

/// pyproject.toml's version must be the workspace's, which tags and file names carry.
fn check_python_version(root: &Path, version: &str) -> Result<(), String> {
    let path = root.join("python/florastack/pyproject.toml");
    let python = python_version(&read_text(&path)?).map_err(|e| format!("{}: {e}", path.display()))?;
    if python == version { Ok(()) } else { Err(format!("{} has version {python}, the workspace {version}: keep them the same", path.display())) }
}

fn python_version(pyproject: &str) -> Result<String, String> {
    let value: toml::Value = toml::from_str(pyproject).map_err(|e| e.to_string())?;
    value
        .get("project")
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "no project.version".to_owned())
}

/// `SHA256SUMS.txt` in `dir`: one `<hash>  <name>` line per file, sorted by name, as `sha256sum` writes and checks it.
pub fn write_sums(dir: &Path, files: &[PathBuf]) -> Result<PathBuf, String> {
    let mut sums = Vec::new();
    for path in files {
        let name = path.file_name().map(OsStr::to_string_lossy).ok_or_else(|| format!("{} has no name", path.display()))?;
        let (hash, _) = sha256::file(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        sums.push((name, hash));
    }
    sums.sort();
    let text = sums.iter().fold(String::new(), |mut text, (name, hash)| {
        let _ = writeln!(text, "{hash}  {name}");
        text
    });
    let path = dir.join(SUMS);
    write(&path, text.as_bytes())?;
    Ok(path)
}

/// Replaces each `@NAME@` with its value, and fails on a token that has none, so that no placeholder ships.
pub fn fill(template: &str, values: &[(&str, &str)]) -> Result<String, String> {
    let mut text = template.to_owned();
    for (name, value) in values {
        text = text.replace(&format!("@{name}@"), value);
    }
    let mut rest = text.as_str();
    while let Some(start) = rest.find('@') {
        let after = &rest[start + 1..];
        let end = after.find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')).unwrap_or(after.len());
        if end > 0 && after[end..].starts_with('@') {
            return Err(format!("no value for @{}@", &after[..end]));
        }
        rest = after;
    }
    Ok(text)
}

/// Escapes text for XML content and attribute values.
pub fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// A program on PATH (with Windows' executable extensions).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_owned()).split(';').map(str::to_owned).collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).find_map(|dir| extensions.iter().map(|ext| dir.join(format!("{name}{ext}"))).find(|candidate| candidate.is_file()))
}

pub fn execute(command: &mut Command, what: &str) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status().map_err(|e| format!("cannot {what}: cannot run {program}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("cannot {what}: {program} failed ({status})")) }
}

pub fn output(command: &mut Command, what: &str) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command.output().map_err(|e| format!("cannot {what}: cannot run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!("cannot {what}: {program} failed ({}):\n{}", output.status, String::from_utf8_lossy(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("cannot {what}: {program} printed something that isn't UTF-8"))
}

pub fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// A template from `packaging/`, with Unix line endings whatever the checkout's: Git for Windows writes text files
/// with CRLF (`core.autocrlf`), and what the templates make goes into Linux and macOS packages.
pub fn read_template(path: &Path) -> Result<String, String> {
    Ok(read_text(path)?.replace("\r\n", "\n"))
}

pub fn write(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// Copies a folder's files and folders into `to`, which may exist already.
pub fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() { copy_tree(&path, &target)? } else { copy(&path, &target)? }
    }
    Ok(())
}

/// Every file below `dir`, sorted.
pub fn list_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))? {
            let path = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?.path();
            if path.is_dir() { pending.push(path) } else { files.push(path) }
        }
    }
    files.sort();
    Ok(files)
}

pub fn fresh_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("cannot empty {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

/// Sets a file's Unix permission bits (packages record them); nothing to do on Windows, which builds no Unix package.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))] // Fallible on Unix, where it does something.
pub fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| format!("cannot set the mode of {}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// `dir` as `<dir name>/...` in a gzipped tarball, owned by root: no local user or group names get in.
pub fn tar_gz(dir: &Path, archive: &Path) -> Result<PathBuf, String> {
    let (parent, name) = split(dir)?;
    let mut tar = Command::new("tar");
    if cfg!(target_os = "macos") {
        // bsdtar: no AppleDouble files for extended attributes.
        tar.env("COPYFILE_DISABLE", "1").args(["--uid", "0", "--gid", "0"]);
    } else {
        tar.args(["--owner=0", "--group=0", "--numeric-owner"]);
    }
    tar.arg("-czf").arg(archive).arg("-C").arg(parent).arg(name);
    execute(&mut tar, &format!("archive {}", dir.display()))?;
    Ok(archive.to_path_buf())
}

/// A path's folder and file name.
pub fn split(path: &Path) -> Result<(&Path, &OsStr), String> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok((parent, name)),
        _ => Err(format!("{} has no folder or name", path.display())),
    }
}

/// Extracts a library's license text from its source archive into `to`.
pub fn license_text(archive: &Path, library: &pins::Library, to: &Path) -> Result<(), String> {
    let (dir, file) = split(to)?;
    let staging = dir.join(format!(".extract-{}", file.to_string_lossy()));
    fresh_dir(&staging)?;
    let member = format!("{}/{}", library.folder(), library.license_file);
    let (folder, name) = split(archive)?;
    // Run where the archive is, with relative paths, so that GNU tar doesn't read a drive letter as a host.
    let mut tar = Command::new("tar");
    tar.current_dir(folder).arg("-xf").arg(name).arg("-C").arg(&staging).arg(&member);
    execute(&mut tar, &format!("extract {member}"))?;
    copy(&staging.join(&member), to)?;
    std::fs::remove_dir_all(&staging).map_err(|e| format!("cannot remove {}: {e}", staging.display()))
}

/// The license notice for the bundled camera libraries (macOS app and Flatpak), with each library's license text,
/// written into `dir`.
pub fn camera_library_notices(dist: &Dist, pins: &pins::Pins, archives: &[PathBuf; 3], dir: &Path, replace: &str) -> Result<(), String> {
    let libraries = &pins.library;
    let mut names = String::new();
    for ((name, library), archive) in libraries.all().into_iter().zip(archives) {
        let file = format!("COPYING.{name}");
        license_text(archive, library, &dir.join(&file))?;
        let _ = write!(names, "{file} ");
    }
    let text = dist.template(
        "gphoto/LIBGPHOTO2.txt.in",
        &[
            ("LIBGPHOTO2_VERSION", &libraries.libgphoto2.version),
            ("LIBUSB_VERSION", &libraries.libusb.version),
            ("LIBTOOL_VERSION", &libraries.libtool.version),
            ("LICENSE_FILES", names.trim_end()),
            ("REPLACE", replace),
        ],
    )?;
    write(&dir.join("LIBGPHOTO2.txt"), text.as_bytes())
}

/// The camera libraries' source archives, downloaded and checked, in build order.
pub fn camera_library_sources(dist: &Dist, pins: &pins::Pins) -> Result<[PathBuf; 3], String> {
    let l = &pins.library;
    Ok([pins::fetch(&l.libtool, &dist.downloads)?, pins::fetch(&l.libusb, &dist.downloads)?, pins::fetch(&l.libgphoto2, &dist.downloads)?])
}

/// Copies the source archives the camera libraries were built from next to the packages, for the release: the
/// LGPL asks a binary distribution to offer its libraries' source, and the workflow attaches these.
pub fn publish_sources(dist: &Dist, archives: &[PathBuf; 3]) -> Result<Vec<PathBuf>, String> {
    let mut published = Vec::new();
    for archive in archives {
        let to = dist.out.join(archive.file_name().ok_or("a source archive has no name")?);
        copy(archive, &to)?;
        published.push(to);
    }
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branding_with(app_id: &str) -> Branding {
        let mut branding = branding::repository();
        branding.app_id = app_id.to_owned();
        branding
    }

    #[test]
    fn the_repository_identity_ships() {
        check_identity(&branding::repository()).unwrap();
    }

    #[test]
    fn the_owner_placeholder_is_refused() {
        let error = check_identity(&branding_with("io.github.OWNER.FloraStack")).unwrap_err();
        assert!(error.contains("OWNER placeholder"), "{error}");
        for bad in ["FloraStack", "io.github", "io..x.App", "io.github.x.Flora-Stack", "io.github.9x.App", "io.github.x y.App"] {
            assert!(check_identity(&branding_with(bad)).is_err(), "{bad}");
        }
        check_identity(&branding_with("io.github.some-owner.FloraStack")).unwrap();
        let mut branding = branding::repository();
        branding.binaries.cli = "FloraStack_CLI".to_owned();
        assert!(check_identity(&branding).is_err(), "package names are lower case");
        let mut branding = branding::repository();
        branding.display_name = "Flora/Stack".to_owned();
        assert!(check_identity(&branding).is_err(), "display names name files");
    }

    #[test]
    fn tags_must_release_the_workspace_version() {
        assert_eq!(check_tag("v0.1.0", "0.1.0"), Ok(false));
        assert_eq!(check_tag("v0.1.0-test.1", "0.1.0"), Ok(true));
        assert_eq!(check_tag("v0.1.0-rc1", "0.1.0"), Ok(true));
        for bad in ["0.1.0", "v0.1.1", "v0.1.0.1", "v0.1.00", "v0.1.0-", "v0.1.0-a b", "v0.1"] {
            assert!(check_tag(bad, "0.1.0").is_err(), "{bad}");
        }
    }

    #[test]
    fn targets_and_formats() {
        for (triple, ..) in TARGETS {
            assert_eq!(Target::parse(triple).unwrap().triple(), triple);
        }
        assert!(Target::parse("i686-pc-windows-msvc").is_err());
        let windows = Target::parse("aarch64-pc-windows-msvc").unwrap();
        assert_eq!(windows.label(), "windows-arm64");
        assert!(!windows.gphoto(), "Windows builds never include camera control");
        let mac = Target::parse("universal-apple-darwin").unwrap();
        assert_eq!((mac.label(), mac.rust_triples().len(), mac.gphoto()), ("macos-universal".to_owned(), 2, true));
        assert_eq!(Target::parse("x86_64-unknown-linux-gnu").unwrap().label(), "linux-x86_64");
        let linux = Target::parse("aarch64-unknown-linux-gnu").unwrap();
        assert_eq!(parse_formats("cli, deb,flatpak", linux).unwrap(), [Format::Deb, Format::Flatpak, Format::Cli], "the target's order");
        assert!(parse_formats("msi", linux).unwrap_err().contains("has no `msi`"));
        assert!(parse_formats("snap", linux).is_err());
        assert!(parse_formats(",", linux).is_err());
    }

    #[test]
    fn the_release_workflow_packages_every_target() {
        let workflow = include_str!("../../../.github/workflows/release.yml");
        let mut released: Vec<&str> = workflow.lines().filter_map(|line| line.trim().strip_prefix("target: ")).collect();
        released.sort_unstable();
        // Every target but the single-architecture macOS builds, which are for local use: releases ship universal ones.
        let mut expected: Vec<&str> =
            TARGETS.iter().map(|(name, ..)| *name).filter(|name| !name.ends_with("-apple-darwin") || name.starts_with("universal")).collect();
        expected.sort_unstable();
        assert_eq!(released, expected);
        assert!(workflow.contains("run: cargo xtask dist --target ${{ matrix.target }} --out dist --tag \"$GITHUB_REF_NAME\""));
        for variable in ["APPLE_SIGNING_IDENTITY", "APPLE_NOTARY_KEY_PATH", "APPLE_NOTARY_KEY_ID", "APPLE_NOTARY_ISSUER"] {
            assert!(workflow.contains(variable) && USAGE.contains(variable), "{variable}");
        }
    }

    #[test]
    fn arguments() {
        let options = parse_args(&["--target", "x86_64-pc-windows-msvc", "--formats", "zip", "--out", "o", "--tag", "v1"]).unwrap();
        assert_eq!(options, Options { help: false, target: Some("x86_64-pc-windows-msvc"), formats: Some("zip"), out: Some("o"), tag: Some("v1") });
        assert!(parse_args(&["--target"]).is_err());
        assert!(parse_args(&["--sign"]).is_err());
        assert!(parse_args(&["-h"]).unwrap().help);
    }

    #[test]
    fn templates_leave_no_placeholder() {
        assert_eq!(fill("a @X@ b@X@ @Y_2@", &[("X", "1"), ("Y_2", "2")]).unwrap(), "a 1 b1 2");
        assert!(fill("@X@ and @MISSING@", &[("X", "1")]).unwrap_err().contains("@MISSING@"));
        // Lone and lower-case at signs aren't placeholders.
        assert_eq!(fill("mail@example.org, @ @lower@", &[]).unwrap(), "mail@example.org, @ @lower@");
        assert_eq!(xml("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
    }

    #[test]
    fn the_python_package_has_the_workspace_version() {
        let version = python_version(include_str!("../../../python/florastack/pyproject.toml")).unwrap();
        let workspace: toml::Value = toml::from_str(include_str!("../../../Cargo.toml")).unwrap();
        assert_eq!(Some(version.as_str()), workspace["workspace"]["package"]["version"].as_str());
    }

    #[test]
    fn sums_are_sha256sum_lines_sorted_by_name() {
        let dir = std::env::temp_dir().join(format!("florastack-dist-sums-{}", std::process::id()));
        fresh_dir(&dir).unwrap();
        write(&dir.join("b.zip"), b"b").unwrap();
        write(&dir.join("a.msi"), b"").unwrap();
        let sums = write_sums(&dir, &[dir.join("b.zip"), dir.join("a.msi")]).unwrap();
        let text = std::fs::read_to_string(sums).unwrap();
        assert_eq!(
            text,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  a.msi\n\
             3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d  b.zip\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
