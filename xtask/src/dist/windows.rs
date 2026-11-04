//! Windows packages: the portable .zip, and the .msi that cargo-wix builds with the WiX Toolset 3 from
//! `packaging/windows/main.wxs`. The template names nothing itself: xtask passes the identity, the version and the
//! input files as WiX preprocessor variables. The WiX Toolset is the pinned download, not an installed copy.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Dist, icon, pins};

/// Windows' own tar (bsdtar), which writes zip archives: the GNU tar that Git puts on PATH can't.
fn bsdtar() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join("tar.exe")
}

/// `dir` as `<dir name>\...` in a zip archive.
pub fn zip(dir: &Path, archive: &Path) -> Result<PathBuf, String> {
    let (parent, name) = super::split(dir)?;
    let mut tar = Command::new(bsdtar());
    tar.args(["-a", "-c", "-f"]).arg(archive).arg("-C").arg(parent).arg(name);
    super::execute(&mut tar, &format!("zip {}", dir.display()))?;
    Ok(archive.to_path_buf())
}

/// Both apps and the license files in `<stem>\`, zipped: runs from any folder, installs nothing.
pub fn portable(dist: &Dist) -> Result<PathBuf, String> {
    let stem = dist.stem();
    let dir = dist.fresh("zip")?.join(&stem);
    stage(dist, &dir)?;
    zip(&dir, &dist.out.join(format!("{stem}.zip")))
}

/// The Windows subsystem in a PE executable's header that opens no console: a GUI program.
const GUI: u16 = 2;
/// The subsystem that runs in a console, opening one when started from Explorer.
const CONSOLE: u16 = 3;

/// The subsystem that a PE executable's optional header names, from the file's first bytes.
fn subsystem(header: &[u8]) -> Option<u16> {
    let pe = usize::try_from(u32::from_le_bytes(header.get(0x3c..0x40)?.try_into().ok()?)).ok()?;
    if header.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    // The optional header follows the 4-byte signature and the 20-byte file header; its Subsystem field is at
    // offset 68 in both its PE32 and PE32+ forms.
    let at = pe.checked_add(24 + 68)?;
    Some(u16::from_le_bytes(header.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

/// The desktop app must be a GUI program, or Windows opens a console with it that ends the app when it's closed,
/// and the CLI a console program.
fn check_subsystems(dist: &Dist) -> Result<(), String> {
    use std::io::Read as _;
    for (path, wanted, what) in [(dist.gui(), GUI, "a GUI program"), (dist.cli(), CONSOLE, "a console program")] {
        let mut header = Vec::new();
        let file = std::fs::File::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        file.take(4096).read_to_end(&mut header).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        match subsystem(&header) {
            Some(found) if found == wanted => {}
            found => return Err(format!("{} must be {what} (Windows subsystem {wanted}), but is {found:?}", path.display())),
        }
    }
    Ok(())
}

fn stage(dist: &Dist, dir: &Path) -> Result<(), String> {
    check_subsystems(dist)?;
    let b = &dist.branding.binaries;
    super::copy(&dist.gui(), &dir.join(dist.target.exe(&b.gui)))?;
    super::copy(&dist.cli(), &dir.join(dist.target.exe(&b.cli)))?;
    for (name, path) in dist.licenses() {
        super::copy(&path, &dir.join(name))?;
    }
    Ok(())
}

/// The installer: both apps in Program Files, a Start-menu shortcut and the folder on PATH (an optional feature),
/// for every user (it asks for elevation), upgrading any earlier version in place.
pub fn msi(dist: &Dist, pins: &pins::Pins) -> Result<PathBuf, String> {
    let wix = wix_toolset(dist, &pins.tool.wix)?;
    let inputs = dist.fresh("msi")?;
    let bin = inputs.join("bin");
    stage(dist, &bin)?;
    let icon_file = inputs.join("icon.ico");
    super::write(&icon_file, &icon::ico()?)?;
    let mut licenses = Vec::new();
    for (name, path) in dist.licenses() {
        licenses.push((name, super::read_text(&path)?));
    }
    let license_file = inputs.join("License.rtf");
    super::write(&license_file, license_rtf(&dist.branding.display_name, &licenses).as_bytes())?;

    let msi = dist.out.join(format!("{}.msi", dist.stem()));
    let b = &dist.branding;
    let defines = [
        ("DisplayName", b.display_name.clone()),
        ("AppId", b.app_id.clone()),
        ("GuiBinary", b.binaries.gui.clone()),
        ("CliBinary", b.binaries.cli.clone()),
        ("ProductVersion", msi_version(&dist.version)?),
        ("Repository", dist.repository.clone()),
        ("Summary", super::SUMMARY.to_owned()),
        ("BinDir", bin.display().to_string()),
        ("IconFile", icon_file.display().to_string()),
        ("LicenseRtf", license_file.display().to_string()),
    ];
    let mut cargo = crate::cargo();
    cargo.current_dir(&dist.root).args(["wix", "--no-build", "--nocapture", "--package", &dist.gui_package, "--target", dist.target.triple()]);
    cargo.args(["--install-version", &msi_version(&dist.version)?]);
    cargo.arg("--target-bin-dir").arg(&bin).arg("--bin-path").arg(&wix);
    cargo.arg("--include").arg(dist.root.join("packaging/windows/main.wxs")).arg("--output").arg(&msi);
    for (name, value) in defines {
        cargo.arg("--compiler-arg").arg(format!("-d{name}={value}"));
    }
    // ICE61 flags the same-version upgrade that MajorUpgrade's AllowSameVersionUpgrades asks for (so that a rebuilt
    // installer of one version replaces the other); WiX's documentation says to ignore it there.
    cargo.args(["--linker-arg", "-sice:ICE61"]);
    super::execute(&mut cargo, "build the .msi (cargo wix)")?;
    Ok(msi)
}

/// An MSI `ProductVersion`: the numeric major.minor.patch, without any pre-release part, within MSI's limits.
fn msi_version(version: &str) -> Result<String, String> {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<u32> = core.split('.').map(str::parse).collect::<Result<_, _>>().map_err(|_| format!("version {version} isn't numeric"))?;
    match parts.as_slice() {
        [major, minor, patch] if *major <= 255 && *minor <= 255 && *patch <= 65535 => Ok(core.to_owned()),
        _ => Err(format!("version {version} doesn't fit an MSI version (major and minor up to 255, patch up to 65535)")),
    }
}

/// The license dialog's text: what the licensing is, then each file in full.
fn license_rtf(display_name: &str, files: &[(&str, String)]) -> String {
    let mut rtf = String::from("{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl{\\f0\\fswiss Segoe UI;}}\\f0\\fs18\n");
    let intro = format!(
        "{display_name} is free software: you can use, copy, modify and share it under the terms of either the MIT license or the Apache \
         License, Version 2.0, at your option. Both follow, and then the notices of the code it includes."
    );
    rtf.push_str(&rtf_text(&intro));
    rtf.push_str("\\par\\par\n");
    for (name, text) in files {
        let _ = writeln!(rtf, "\\b {}\\b0\\par", rtf_text(name));
        for line in text.lines() {
            rtf.push_str(&rtf_text(line));
            rtf.push_str("\\par\n");
        }
        rtf.push_str("\\par\n");
    }
    rtf.push('}');
    rtf
}

fn rtf_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '{' | '}' => {
                escaped.push('\\');
                escaped.push(c);
            }
            ' '..='~' => escaped.push(c),
            '\t' => escaped.push_str("\\tab "),
            _ => {
                // RTF writes other characters as signed 16-bit UTF-16 units, each with a fallback character.
                let mut units = [0; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(escaped, "\\u{}?", i16::from_ne_bytes(unit.to_ne_bytes()));
                }
            }
        }
    }
    escaped
}

/// The pinned WiX Toolset's folder of tools, unpacked once into the downloads folder.
fn wix_toolset(dist: &Dist, pin: &pins::Tool) -> Result<PathBuf, String> {
    let dir = dist.downloads.join(format!("wix-{}", pin.version));
    if dir.join("candle.exe").is_file() && dir.join("light.exe").is_file() {
        return Ok(dir);
    }
    let archive = pins::fetch(pin, &dist.downloads)?;
    let staging = dist.downloads.join(format!("wix-{}.part", pin.version));
    super::fresh_dir(&staging)?;
    let mut tar = Command::new(bsdtar());
    tar.arg("-x").arg("-f").arg(&archive).arg("-C").arg(&staging);
    super::execute(&mut tar, "unpack the WiX Toolset")?;
    if !staging.join("candle.exe").is_file() {
        return Err(format!("{} has no candle.exe", archive.display()));
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
    }
    std::fs::rename(&staging, &dir).map_err(|e| format!("cannot move the WiX Toolset into place: {e}"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subsystem_is_read_from_the_pe_header() {
        let mut header = vec![0_u8; 0x200];
        header[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
        header[0x80..0x84].copy_from_slice(b"PE\0\0");
        header[0x80 + 92..0x80 + 94].copy_from_slice(&GUI.to_le_bytes());
        assert_eq!(subsystem(&header), Some(GUI));
        assert_eq!(subsystem(&header[..0x90]), None, "cut short");
        header[0x80] = b'X';
        assert_eq!(subsystem(&header), None, "no PE signature");
        header[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(subsystem(&header), None, "a header offset past the end");
        assert_eq!(subsystem(b"MZ"), None);
    }

    /// This test's own executable is a console program.
    #[cfg(windows)]
    #[test]
    fn a_real_executable_has_its_subsystem() {
        let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(subsystem(&exe), Some(CONSOLE));
    }

    #[test]
    fn msi_versions_are_numeric() {
        assert_eq!(msi_version("0.1.0").unwrap(), "0.1.0");
        assert_eq!(msi_version("1.2.3-rc.1").unwrap(), "1.2.3");
        assert!(msi_version("256.0.0").is_err());
        assert!(msi_version("1.2").is_err());
        assert!(msi_version("1.x.0").is_err());
    }

    #[test]
    fn license_text_is_escaped_for_rtf() {
        assert_eq!(rtf_text(r"a\b {c} é 🌸"), r"a\\b \{c\} \u233? \u-10180?\u-8392?");
        let rtf = license_rtf("App", &[("LICENSE-MIT", "line one\nline {two}\n".to_owned())]);
        assert!(rtf.starts_with("{\\rtf1") && rtf.ends_with('}'));
        assert!(rtf.contains("\\b LICENSE-MIT\\b0\\par\nline one\\par\nline \\{two\\}\\par\n"));
        // Braces balance: every literal brace is escaped.
        let unescaped = rtf.replace("\\{", "").replace("\\}", "");
        assert_eq!(unescaped.matches('{').count(), unescaped.matches('}').count());
    }

    #[test]
    fn the_template_takes_every_variable_xtask_defines() {
        let wxs = include_str!("../../../packaging/windows/main.wxs");
        for name in ["DisplayName", "AppId", "GuiBinary", "CliBinary", "ProductVersion", "Repository", "Summary", "BinDir", "IconFile", "LicenseRtf"]
        {
            assert!(wxs.contains(&format!("$(var.{name})")), "main.wxs doesn't use {name}");
        }
        // Nothing of the identity is written into it.
        let branding = crate::branding::repository();
        for value in [&branding.display_name, &branding.app_id, &branding.binaries.gui] {
            assert!(!wxs.contains(value.as_str()), "main.wxs names {value}: pass it as a variable");
        }
    }
}
