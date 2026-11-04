//! macOS packages: the app bundle in a .dmg, universal (`arm64` and `x86_64`) on the universal target.
//!
//! The bundle carries libgphoto2, libusb and libltdl as separate dylibs (`Contents/Frameworks`) with libgphoto2's
//! drivers (`Contents/PlugIns/libgphoto2`), built from the pinned sources by `packaging/macos/build-libgphoto2.sh`;
//! florastack-capture finds them next to the executable. The bundle is signed with the Developer ID that
//! `APPLE_SIGNING_IDENTITY` names, with the hardened runtime, or ad hoc without one (arm64 runs no unsigned code); the
//! .dmg is then notarized and stapled when the `APPLE_NOTARY_*` variables give an App Store Connect API key.
//!
//! The entitlements (`packaging/macos/entitlements.plist`) turn off library validation, so that a user can load their
//! own libgphoto2 (`FLORASTACK_GPHOTO_LIBRARY`) in a signed app, as the LGPL requires.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Dist, MACOS_MINIMUM, icon, pins, xml};

pub fn programs() -> Vec<(&'static str, &'static str)> {
    let xcode = "Xcode's command-line tools: xcode-select --install";
    vec![
        ("lipo", xcode),
        ("codesign", xcode),
        ("install_name_tool", xcode),
        ("otool", xcode),
        ("clang", xcode),
        ("make", xcode),
        ("hdiutil", "part of macOS"),
        ("pkg-config", "brew install pkgconf"),
    ]
}

/// Joins each binary's two builds into one universal file.
pub fn join_binaries(dist: &Dist) -> Result<(), String> {
    let dir = dist.work.join("universal");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for name in dist.branding.binary_names() {
        let mut lipo = Command::new("lipo");
        lipo.arg("-create").arg("-output").arg(dir.join(name));
        for triple in dist.target.rust_triples() {
            lipo.arg(dist.cargo_target.join(triple).join(super::PROFILE).join(name));
        }
        super::execute(&mut lipo, &format!("join the two builds of {name}"))?;
    }
    Ok(())
}

/// The variables that sign and notarize, when they are set.
struct Signing {
    identity: Option<String>,
    notary: Option<Notary>,
}

struct Notary {
    key: PathBuf,
    key_id: String,
    issuer: String,
}

fn signing() -> Result<Signing, String> {
    signing_from(|name| std::env::var(name).ok())
}

/// The signing settings from the variables `get` reads, where an empty one counts as unset.
fn signing_from(get: impl Fn(&str) -> Option<String>) -> Result<Signing, String> {
    let var = |name: &str| get(name).filter(|value| !value.trim().is_empty());
    let identity = var("APPLE_SIGNING_IDENTITY");
    let notary = match (var("APPLE_NOTARY_KEY_PATH"), var("APPLE_NOTARY_KEY_ID"), var("APPLE_NOTARY_ISSUER")) {
        (Some(key), Some(key_id), Some(issuer)) => Some(Notary { key: PathBuf::from(key), key_id, issuer }),
        (None, None, None) => None,
        _ => return Err("set all of APPLE_NOTARY_KEY_PATH, APPLE_NOTARY_KEY_ID and APPLE_NOTARY_ISSUER to notarize, or none".to_owned()),
    };
    if notary.is_some() && identity.is_none() {
        return Err("notarizing needs a signed app: set APPLE_SIGNING_IDENTITY too".to_owned());
    }
    Ok(Signing { identity, notary })
}

pub fn dmg(dist: &Dist, pins: &pins::Pins) -> Result<Vec<PathBuf>, String> {
    let signing = signing()?;
    let sources = super::camera_library_sources(dist, pins)?;
    let libraries = camera_libraries(dist, pins, &sources)?;

    let name = &dist.branding.display_name;
    let staging = dist.fresh("dmg")?;
    let app = staging.join(format!("{name}.app"));
    let contents = app.join("Contents");
    let b = &dist.branding.binaries;
    let executable = contents.join("MacOS").join(&b.gui);
    super::copy(&dist.gui(), &executable)?;
    super::copy(&dist.cli(), &contents.join("MacOS").join(&b.cli))?;
    super::write(&contents.join("Info.plist"), info_plist(dist).as_bytes())?;
    super::write(&contents.join("PkgInfo"), b"APPL????")?;
    super::write(&contents.join("Resources").join(format!("{name}.icns")), &icon::icns()?)?;
    for (file, path) in dist.licenses() {
        super::copy(&path, &contents.join("Resources/licenses").join(file))?;
    }
    super::copy_tree(&libraries.join("Frameworks"), &contents.join("Frameworks"))?;
    super::copy_tree(&libraries.join("PlugIns"), &contents.join("PlugIns"))?;
    let replace = format!(
        "replace the files in {name}.app/Contents/Frameworks and Contents/PlugIns/libgphoto2 with your own build, then sign the app again \
         (for your own use, ad hoc: codesign --force --deep --sign - {name}.app)"
    );
    super::camera_library_notices(dist, pins, &sources, &contents.join("Resources/licenses"), &replace)?;

    // Inside out: every library and driver, the CLI, then the bundle with its main executable.
    let entitlements = dist.root.join("packaging/macos/entitlements.plist");
    let mut nested: Vec<PathBuf> = super::list_files(&contents.join("PlugIns"))?;
    nested.extend(super::list_files(&contents.join("Frameworks"))?);
    for file in &nested {
        sign(file, &signing, None)?;
    }
    sign(&contents.join("MacOS").join(&b.cli), &signing, Some(&entitlements))?;
    sign(&app, &signing, Some(&entitlements))?;
    super::execute(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&app), "verify the app's signature")?;

    // The disk image: the app and a link to /Applications to drag it onto.
    super::execute(Command::new("ln").arg("-s").arg("/Applications").arg(staging.join("Applications")), "link /Applications")?;
    let dmg = dist.out.join(format!("{}.dmg", dist.stem()));
    let mut hdiutil = Command::new("hdiutil");
    hdiutil.args(["create", "-volname", name, "-fs", "HFS+", "-format", "UDZO", "-ov", "-srcfolder"]).arg(&staging).arg(&dmg);
    super::execute(&mut hdiutil, "create the disk image")?;
    if let Some(identity) = &signing.identity {
        super::execute(Command::new("codesign").args(["--force", "--timestamp", "--sign", identity]).arg(&dmg), "sign the disk image")?;
    }
    if let Some(notary) = &signing.notary {
        notarize(&dmg, notary)?;
    } else {
        println!("not notarized: APPLE_NOTARY_KEY_PATH, APPLE_NOTARY_KEY_ID and APPLE_NOTARY_ISSUER aren't set");
    }
    let mut artifacts = vec![dmg];
    artifacts.extend(super::publish_sources(dist, &sources)?);
    Ok(artifacts)
}

/// Signs with the Developer ID and the hardened runtime, or ad hoc.
fn sign(path: &Path, signing: &Signing, entitlements: Option<&Path>) -> Result<(), String> {
    let mut codesign = Command::new("codesign");
    codesign.arg("--force");
    match &signing.identity {
        Some(identity) => codesign.args(["--timestamp", "--options", "runtime", "--sign", identity]),
        None => codesign.args(["--sign", "-"]),
    };
    if let Some(entitlements) = entitlements {
        codesign.arg("--entitlements").arg(entitlements);
    }
    codesign.arg(path);
    super::execute(&mut codesign, &format!("sign {}", path.display()))
}

/// Submits the disk image to Apple's notary service, waits for the verdict, and staples the ticket to it.
fn notarize(dmg: &Path, notary: &Notary) -> Result<(), String> {
    let mut submit = Command::new("xcrun");
    submit.args(["notarytool", "submit", "--wait", "--output-format", "json", "--key"]).arg(&notary.key);
    submit.args(["--key-id", &notary.key_id, "--issuer", &notary.issuer]).arg(dmg);
    let text = super::output(&mut submit, "notarize the disk image")?;
    let result: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("cannot read notarytool's answer ({e}): {text}"))?;
    if result["status"] != "Accepted" {
        let id = result["id"].as_str().unwrap_or("?");
        return Err(format!(
            "notarization ended {}: `xcrun notarytool log {id} --key ... --key-id ... --issuer ...` says why",
            result["status"].as_str().unwrap_or("without a status")
        ));
    }
    super::execute(Command::new("xcrun").args(["stapler", "staple"]).arg(dmg), "staple the notarization ticket")
}

/// The camera libraries, built once per set of pins into the downloads folder (they don't depend on FloraStack).
fn camera_libraries(dist: &Dist, pins: &pins::Pins, sources: &[PathBuf; 3]) -> Result<PathBuf, String> {
    let l = &pins.library;
    let script = super::read_text(&dist.root.join("packaging/macos/build-libgphoto2.sh"))?;
    let key = format!(
        "{} {} {} {MACOS_MINIMUM} {} {}",
        l.libtool.sha256,
        l.libusb.sha256,
        l.libgphoto2.sha256,
        super::GPHOTO_CONFIGURE.join(" "),
        crate::sha256::hex_of(script.as_bytes())
    );
    let dir = dist.downloads.join(format!("macos-libgphoto2-{}", l.libgphoto2.version));
    let stamp = dir.join(".built-from");
    if std::fs::read_to_string(&stamp).is_ok_and(|built| built == key) {
        return Ok(dir);
    }
    let mut build = Command::new("bash");
    build.arg(dist.root.join("packaging/macos/build-libgphoto2.sh")).args(sources).arg(&dir);
    build.env("MACOSX_DEPLOYMENT_TARGET", MACOS_MINIMUM).env("GPHOTO_CONFIGURE", super::GPHOTO_CONFIGURE.join(" "));
    super::execute(&mut build, "build the camera libraries (packaging/macos/build-libgphoto2.sh)")?;
    super::write(&stamp, key.as_bytes())?;
    Ok(dir)
}

fn info_plist(dist: &Dist) -> String {
    let b = &dist.branding;
    let entries = [
        ("CFBundleDevelopmentRegion", "en".to_owned()),
        ("CFBundleDisplayName", b.display_name.clone()),
        ("CFBundleExecutable", b.binaries.gui.clone()),
        ("CFBundleIconFile", format!("{}.icns", b.display_name)),
        ("CFBundleIdentifier", b.app_id.clone()),
        ("CFBundleInfoDictionaryVersion", "6.0".to_owned()),
        ("CFBundleName", b.display_name.clone()),
        ("CFBundlePackageType", "APPL".to_owned()),
        ("CFBundleShortVersionString", dist.version.clone()),
        ("CFBundleVersion", dist.version.clone()),
        ("LSApplicationCategoryType", "public.app-category.photography".to_owned()),
        ("LSMinimumSystemVersion", MACOS_MINIMUM.to_owned()),
        ("NSHumanReadableCopyright", format!("{} contributors. MIT OR Apache-2.0.", b.display_name)),
    ];
    let mut plist = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in entries {
        let _ = writeln!(plist, "  <key>{key}</key>\n  <string>{}</string>", xml(&value));
    }
    plist.push_str("  <key>NSHighResolutionCapable</key>\n  <true/>\n</dict>\n</plist>\n");
    plist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_named_from_branding() {
        let root = crate::workspace_root().unwrap();
        let dist = Dist {
            branding: crate::branding::repository(),
            version: "1.2.3".to_owned(),
            repository: "https://example.org/r".to_owned(),
            target: super::super::Target::parse("universal-apple-darwin").unwrap(),
            gui_package: String::new(),
            cli_package: String::new(),
            cargo_target: root.join("target"),
            work: root.join("target/dist/universal-apple-darwin"),
            downloads: root.join("target/dist/downloads"),
            out: root.join("target/dist/out"),
            root,
        };
        let plist = info_plist(&dist);
        let b = &dist.branding;
        assert!(plist.contains(&format!("<key>CFBundleIdentifier</key>\n  <string>{}</string>", b.app_id)));
        assert!(plist.contains(&format!("<key>CFBundleExecutable</key>\n  <string>{}</string>", b.binaries.gui)));
        assert!(plist.contains("<key>CFBundleShortVersionString</key>\n  <string>1.2.3</string>"));
        assert_eq!(dist.stem(), format!("{}-1.2.3-macos-universal", b.display_name));
        assert_eq!(dist.gui(), dist.work.join("universal").join(&b.binaries.gui));
    }

    #[test]
    fn signing_variables_come_complete() {
        let from = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
            signing_from(move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
        };
        let unsigned = from(&[]).unwrap();
        assert!(unsigned.identity.is_none() && unsigned.notary.is_none(), "no secrets: unsigned");
        let signed = from(&[("APPLE_SIGNING_IDENTITY", "Developer ID Application: X (T)"), ("APPLE_NOTARY_KEY_ID", "")]).unwrap();
        assert!(signed.identity.is_some() && signed.notary.is_none(), "empty variables count as unset, as unset secrets give them");
        let notary = [("APPLE_NOTARY_KEY_PATH", "/k.p8"), ("APPLE_NOTARY_KEY_ID", "K"), ("APPLE_NOTARY_ISSUER", "I")];
        assert!(from(&notary).is_err(), "notarizing needs signing");
        assert!(from(&[("APPLE_SIGNING_IDENTITY", "D"), ("APPLE_NOTARY_KEY_ID", "K")]).is_err(), "a partial notary key");
        let full = from(&[("APPLE_SIGNING_IDENTITY", "D"), notary[0], notary[1], notary[2]]).unwrap();
        assert_eq!(full.notary.map(|n| (n.key, n.key_id, n.issuer)), Some((PathBuf::from("/k.p8"), "K".to_owned(), "I".to_owned())));
    }
}
