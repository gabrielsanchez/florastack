//! `packaging/third-party.toml`: the source archives and tools that packaging downloads, each pinned by its SHA-256,
//! and fetching them the way `cargo xtask corpus` fetches its files (curl, then the hash, then a rename into place).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::sha256;

/// The largest file a pin may name (the WiX binaries are about 40 MB).
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub library: Libraries,
    pub tool: Tools,
}

/// The camera-control libraries, in build order: each one needs the ones before it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Libraries {
    pub libtool: Library,
    pub libusb: Library,
    pub libgphoto2: Library,
}

impl Libraries {
    pub fn all(&self) -> [(&'static str, &Library); 3] {
        [("libtool", &self.libtool), ("libusb", &self.libusb), ("libgphoto2", &self.libgphoto2)]
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    pub version: String,
    pub url: String,
    pub sha256: String,
    /// The license text's path inside the archive's top folder.
    pub license_file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub wix: Tool,
    #[serde(rename = "appimagetool-x86_64")]
    pub appimagetool_x86_64: Tool,
    #[serde(rename = "appimagetool-aarch64")]
    pub appimagetool_aarch64: Tool,
    #[serde(rename = "appimage-runtime-x86_64")]
    pub appimage_runtime_x86_64: Tool,
    #[serde(rename = "appimage-runtime-aarch64")]
    pub appimage_runtime_aarch64: Tool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// Something pinned: where it comes from and what it must hash to.
pub trait Pinned {
    fn url(&self) -> &str;
    fn sha256(&self) -> &str;

    /// The file name: the URL's last segment.
    fn file_name(&self) -> &str {
        self.url().rsplit('/').next().unwrap_or_default()
    }
}

impl Pinned for Library {
    fn url(&self) -> &str {
        &self.url
    }
    fn sha256(&self) -> &str {
        &self.sha256
    }
}

impl Pinned for Tool {
    fn url(&self) -> &str {
        &self.url
    }
    fn sha256(&self) -> &str {
        &self.sha256
    }
}

impl Library {
    /// The archive's top folder, which its file name names (`libusb-1.0.30.tar.bz2` holds `libusb-1.0.30/`).
    pub fn folder(&self) -> &str {
        let name = self.file_name();
        [".tar.xz", ".tar.bz2", ".tar.gz"].iter().find_map(|suffix| name.strip_suffix(suffix)).unwrap_or(name)
    }
}

pub fn load(root: &Path) -> Result<Pins, String> {
    let path = root.join("packaging/third-party.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let pins = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let problems = pins.problems();
    if problems.is_empty() { Ok(pins) } else { Err(format!("{}:\n  {}", path.display(), problems.join("\n  "))) }
}

pub fn parse(text: &str) -> Result<Pins, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

impl Pins {
    fn pinned(&self) -> Vec<(String, &dyn Pinned, &str)> {
        let mut all: Vec<(String, &dyn Pinned, &str)> = Vec::new();
        for (name, library) in self.library.all() {
            all.push((format!("library.{name}"), library, &library.version));
        }
        let tools = &self.tool;
        for (name, tool) in [
            ("wix", &tools.wix),
            ("appimagetool-x86_64", &tools.appimagetool_x86_64),
            ("appimagetool-aarch64", &tools.appimagetool_aarch64),
            ("appimage-runtime-x86_64", &tools.appimage_runtime_x86_64),
            ("appimage-runtime-aarch64", &tools.appimage_runtime_aarch64),
        ] {
            all.push((format!("tool.{name}"), tool, &tool.version));
        }
        all
    }

    /// Every entry needs an HTTPS URL that ends in a plain file name, a SHA-256 and a version; a library's license
    /// file stays inside its archive.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, pin, version) in self.pinned() {
            if !pin.url().starts_with("https://") {
                problems.push(format!("{name}: the url must be https"));
            }
            let file = pin.file_name();
            if file.is_empty() || !file.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) || file.starts_with('.') {
                problems.push(format!("{name}: the url must end in a plain file name"));
            }
            if pin.sha256().len() != 64 || !pin.sha256().chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                problems.push(format!("{name}: sha256 must be 64 lower-case hex digits"));
            }
            // As written, or without its dots (WiX names 3.14.1 `wix3141rtm`).
            if version.is_empty() || !(pin.url().contains(version) || pin.url().contains(&version.replace('.', ""))) {
                problems.push(format!("{name}: the url must contain the version ({version})"));
            }
        }
        for (name, library) in self.library.all() {
            let path = &library.license_file;
            if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
                problems.push(format!("library.{name}: license_file must be a relative path inside the archive"));
            }
            if library.folder() == library.file_name() {
                problems.push(format!("library.{name}: the url must name a .tar.xz, .tar.bz2 or .tar.gz archive"));
            }
        }
        problems
    }
}

/// The pinned file in `dir`, downloaded unless a file with the right hash is already there.
pub fn fetch(pin: &dyn Pinned, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(pin.file_name());
    if matches(&path, pin.sha256()) {
        return Ok(path);
    }
    let part = dir.join(format!("{}.part", pin.file_name()));
    crate::corpus::curl(pin.url(), &part, MAX_BYTES)?;
    let (hash, _) = sha256::file(&part).map_err(|e| format!("cannot read {}: {e}", part.display()))?;
    if hash != pin.sha256() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{} has SHA-256 {hash}, not the pinned {} (packaging/third-party.toml)", pin.url(), pin.sha256()));
    }
    std::fs::rename(&part, &path).map_err(|e| format!("cannot move {} into place: {e}", path.display()))?;
    println!("downloaded {}", pin.file_name());
    Ok(path)
}

fn matches(path: &Path, sha256: &str) -> bool {
    path.is_file() && sha256::file(path).is_ok_and(|(hash, _)| hash == sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> Pins {
        parse(include_str!("../../../packaging/third-party.toml")).unwrap()
    }

    #[test]
    fn the_repository_pins_are_valid() {
        let pins = repository();
        assert_eq!(pins.problems(), Vec::<String>::new());
        assert_eq!(pins.library.libgphoto2.folder(), format!("libgphoto2-{}", pins.library.libgphoto2.version));
        assert_eq!(pins.library.libusb.folder(), format!("libusb-{}", pins.library.libusb.version));
        assert_eq!(pins.tool.wix.file_name(), "wix314-binaries.zip");
    }

    #[test]
    fn bad_pins_are_reported() {
        let text = include_str!("../../../packaging/third-party.toml");
        let pins = parse(&text.replace("https://github.com/libusb", "http://github.com/libusb")).unwrap();
        assert!(pins.problems().iter().any(|p| p.contains("library.libusb") && p.contains("https")), "{:?}", pins.problems());
        let pins = parse(&text.replace("2ef1067c16c97db930fd740cc9bc3d3ba9a583804ae5ac42cc3e8719e49e191e", "2EF1")).unwrap();
        assert!(pins.problems().iter().any(|p| p.contains("library.libtool") && p.contains("sha256")));
        let pins = parse(&text.replace("license_file = \"COPYING\"", "license_file = \"../COPYING\"")).unwrap();
        assert!(pins.problems().iter().any(|p| p.contains("license_file")));
        assert!(parse(&format!("{text}\n[tool.extra]\nversion = \"1\"\nurl = \"https://x/1\"\nsha256 = \"0\"\n")).is_err(), "unknown tool");
    }
}
