//! Linux packages: a tarball, .deb, .rpm, AppImage and Flatpak, all from one staged tree (`stage`: `bin/` and
//! `share/` under a prefix: the desktop entry, icons, AppStream metadata and license files).
//!
//! Camera control loads the system's libgphoto2 at run time: the .deb and .rpm depend on the distribution's package,
//! and the tarball and AppImage use whichever is installed. The Flatpak, whose sandbox can't see the system's, builds
//! libgphoto2, libusb and libltdl from the pinned sources into the app (`/app/lib`), as separate shared libraries.
//!
//! The tools must be native: AppImage and Flatpak packages are built for the architecture of the machine that
//! builds them, as are the binaries (Linux builds don't cross-compile).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::{Arch, Dist, GPHOTO_CONFIGURE, description, icon, pins, xml};

/// The Flatpak runtime (org.freedesktop.Platform), supported until 2028.
const FLATPAK_RUNTIME: &str = "26.08";

const FLATHUB: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// The PNG icon sizes, next to the SVG (Flatpak accepts up to 512).
const ICON_SIZES: [u32; 5] = [48, 64, 128, 256, 512];

/// Libraries that the binaries open at run time rather than link (graphics, windowing, input), so that no
/// dependency scanner finds them: Debian packages, then RPM's distribution-independent capabilities.
const DEB_RECOMMENDS: &str =
    "libvulkan1 | libegl1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0 | libx11-6, libxcursor1, libxi6, libxrandr2";
const RPM_RECOMMENDS: [&str; 4] =
    ["libvulkan.so.1()(64bit)", "libxkbcommon.so.0()(64bit)", "libxkbcommon-x11.so.0()(64bit)", "libwayland-client.so.0()(64bit)"];

/// libgphoto2's packages: Debian 13 and Ubuntu 24.04 renamed it for 64-bit time; RPM distributions provide it by soname.
const DEB_GPHOTO: &str = "libgphoto2-6t64 | libgphoto2-6";
const RPM_GPHOTO: &str = "libgphoto2.so.6()(64bit)";

fn machine(arch: Arch) -> Result<&'static str, String> {
    match arch {
        Arch::X86_64 => Ok("x86_64"),
        Arch::Aarch64 => Ok("aarch64"),
        Arch::Universal => Err("there are no universal Linux builds".to_owned()),
    }
}

/// AppImage and Flatpak tools run the target's code: they must run on a machine of its architecture.
fn native(dist: &Dist, what: &str) -> Result<&'static str, String> {
    let arch = machine(dist.target.arch)?;
    if std::env::consts::ARCH == arch { Ok(arch) } else { Err(format!("build the {what} for {arch} on an {arch} machine")) }
}

/// Lays out the apps under `prefix`: `bin/`, and in `share/` the desktop entry, the icons, the AppStream metadata
/// and the license files.
fn stage(dist: &Dist, prefix: &Path) -> Result<(), String> {
    let b = &dist.branding;
    let id = &b.app_id;
    super::copy(&dist.gui(), &prefix.join("bin").join(&b.binaries.gui))?;
    super::copy(&dist.cli(), &prefix.join("bin").join(&b.binaries.cli))?;
    let share = prefix.join("share");
    super::write(&share.join("applications").join(format!("{id}.desktop")), desktop_entry(dist)?.as_bytes())?;
    super::write(&share.join("icons/hicolor/scalable/apps").join(format!("{id}.svg")), icon::svg().as_bytes())?;
    for size in ICON_SIZES {
        super::write(&share.join(format!("icons/hicolor/{size}x{size}/apps/{id}.png")), &icon::png(size)?)?;
    }
    super::write(&share.join("metainfo").join(format!("{id}.metainfo.xml")), metainfo(dist)?.as_bytes())?;
    for (name, path) in dist.licenses() {
        super::copy(&path, &share.join("doc").join(&b.binaries.gui).join(name))?;
    }
    Ok(())
}

fn desktop_entry(dist: &Dist) -> Result<String, String> {
    dist.template("linux/app.desktop.in", &[("SUMMARY", super::SUMMARY)])
}

/// AppStream metadata, its values escaped for XML.
fn metainfo(dist: &Dist) -> Result<String, String> {
    let b = &dist.branding;
    let developer = b.app_id.rsplit_once('.').map_or(b.app_id.as_str(), |(owner, _)| owner);
    let values = [
        ("DISPLAY_NAME", xml(&b.display_name)),
        ("APP_ID", xml(&b.app_id)),
        ("DEVELOPER_ID", xml(developer)),
        ("GUI_BINARY", xml(&b.binaries.gui)),
        ("CLI_BINARY", xml(&b.binaries.cli)),
        ("REPOSITORY", xml(&dist.repository)),
        ("SUMMARY", xml(super::SUMMARY)),
        ("DESCRIPTION", xml(&description(&b.display_name))),
    ];
    let values: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let path = dist.root.join("packaging/linux/metainfo.xml.in");
    super::fill(&super::read_template(&path)?, &values).map_err(|e| format!("{}: {e}", path.display()))
}

/// The permissions a package records: folders and programs 0755, other files 0644.
fn normalize_modes(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        super::set_mode(&dir, 0o755)?;
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))? {
            let path = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let program = dir.file_name().is_some_and(|name| name == "bin") || path.file_name().is_some_and(|name| name == "AppRun");
                super::set_mode(&path, if program { 0o755 } else { 0o644 })?;
            }
        }
    }
    Ok(())
}

/// The newest glibc symbol version the binaries use, which the .deb then requires (`libc6 (>= ...)`): the
/// `GLIBC_2.x` names that the dynamic symbol tables carry.
fn glibc_floor(binaries: &[&[u8]]) -> Option<String> {
    let mut newest: Option<Vec<u32>> = None;
    for bytes in binaries {
        let mut rest = *bytes;
        while let Some(at) = find(rest, b"GLIBC_2.") {
            let tail = &rest[at + 6..];
            let length = tail.iter().take_while(|b| b.is_ascii_digit() || **b == b'.').count();
            let text = String::from_utf8_lossy(&tail[..length]);
            let version: Option<Vec<u32>> = text.trim_end_matches('.').split('.').map(|p| p.parse().ok()).collect();
            if let Some(version) = version
                && newest.as_ref().is_none_or(|n| version > *n)
            {
                newest = Some(version);
            }
            rest = &tail[length..];
        }
    }
    newest.map(|v| v.iter().map(u32::to_string).collect::<Vec<_>>().join("."))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Folds a paragraph into lines of at most `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Debian and RPM order `~` before anything, so that a pre-release sorts before its release.
fn package_version(version: &str) -> String {
    version.replace('-', "~")
}

pub fn tarball(dist: &Dist) -> Result<PathBuf, String> {
    let stem = dist.stem();
    let root = dist.fresh("tarball")?.join(&stem);
    stage(dist, &root)?;
    super::write(&root.join("README.txt"), dist.template("linux/tarball-README.txt.in", &[])?.as_bytes())?;
    normalize_modes(&root)?;
    super::tar_gz(&root, &dist.out.join(format!("{stem}.tar.gz")))
}

pub fn deb(dist: &Dist) -> Result<PathBuf, String> {
    let arch = match dist.target.arch {
        Arch::X86_64 => "amd64",
        Arch::Aarch64 => "arm64",
        Arch::Universal => return Err("there are no universal Linux builds".to_owned()),
    };
    let package = &dist.branding.binaries.gui;
    let version = package_version(&dist.version);
    let name = format!("{package}_{version}_{arch}");
    let root = dist.fresh("deb")?.join(&name);
    stage(dist, &root.join("usr"))?;
    let copyright = format!(
        "{} is licensed under the MIT license or the Apache License, Version 2.0, at your option.\n\
         Their texts and the notices of the code it includes are in /usr/share/doc/{package}/: LICENSE-MIT, LICENSE-APACHE and NOTICE.\n",
        dist.branding.display_name
    );
    super::write(&root.join("usr/share/doc").join(package).join("copyright"), copyright.as_bytes())?;
    let installed: u64 = super::list_files(&root)?.iter().filter_map(|path| std::fs::metadata(path).ok()).map(|m| m.len()).sum();
    let glibc = glibc_floor(&[&read(&dist.gui())?, &read(&dist.cli())?]).ok_or("the binaries name no glibc version")?;
    super::write(&root.join("DEBIAN/control"), deb_control(dist, arch, &version, installed.div_ceil(1024), &glibc).as_bytes())?;
    normalize_modes(&root)?;
    let deb = dist.out.join(format!("{name}.deb"));
    super::execute(Command::new("dpkg-deb").args(["--build", "--root-owner-group"]).arg(&root).arg(&deb), "build the .deb")?;
    Ok(deb)
}

fn deb_control(dist: &Dist, arch: &str, version: &str, installed_kib: u64, glibc: &str) -> String {
    let b = &dist.branding;
    let mut control = format!(
        "Package: {}\nVersion: {version}\nArchitecture: {arch}\nMaintainer: {} contributors <{}/issues>\nInstalled-Size: {installed_kib}\n\
         Depends: libc6 (>= {glibc}), libgcc-s1, {DEB_GPHOTO}\nRecommends: {DEB_RECOMMENDS}\nSection: graphics\nPriority: optional\n\
         Homepage: {}\nDescription: {}\n",
        b.binaries.gui,
        b.display_name,
        dist.repository,
        dist.repository,
        super::SUMMARY,
    );
    for line in wrap(&description(&b.display_name), 78) {
        control.push(' ');
        control.push_str(&line);
        control.push('\n');
    }
    control
}

pub fn rpm(dist: &Dist) -> Result<PathBuf, String> {
    let arch = machine(dist.target.arch)?;
    let top = dist.fresh("rpm")?;
    let root = top.join("root");
    stage(dist, &root.join("usr"))?;
    normalize_modes(&root)?;
    let mut files = Vec::new();
    for path in super::list_files(&root)? {
        let relative = path.strip_prefix(&root).map_err(|_| "a staged file is outside the root")?;
        files.push(format!("/{}", relative.to_string_lossy().replace('\\', "/")));
    }
    let spec = top.join("SPECS").join(format!("{}.spec", dist.branding.binaries.gui));
    super::write(&spec, rpm_spec(dist, &root, &files)?.as_bytes())?;
    let mut rpmbuild = Command::new("rpmbuild");
    rpmbuild.args(["-bb", "--target", arch, "--define"]).arg(format!("_topdir {}", top.display())).arg(&spec);
    super::execute(&mut rpmbuild, "build the .rpm")?;
    let built = super::list_files(&top.join("RPMS"))?;
    let [rpm] = built.as_slice() else { return Err(format!("rpmbuild wrote {} packages, not one", built.len())) };
    let to = dist.out.join(rpm.file_name().ok_or("the .rpm has no name")?);
    std::fs::rename(rpm, &to).map_err(|e| format!("cannot move {}: {e}", rpm.display()))?;
    Ok(to)
}

fn rpm_spec(dist: &Dist, root: &Path, files: &[String]) -> Result<String, String> {
    let b = &dist.branding;
    let root = root.to_string_lossy();
    if root.contains(['\'', '%', '\n']) {
        return Err(format!("rpmbuild can't copy from {root}: build in a folder whose path has no ' or %"));
    }
    let recommends: String = RPM_RECOMMENDS.iter().map(|r| ["Recommends: ", r, "\n"].concat()).collect();
    let files: String = files.iter().map(|f| [f.as_str(), "\n"].concat()).collect();
    Ok(format!(
        "# Written by cargo xtask dist: the files are built already, and staged in the folder %install copies.\n\
         %global debug_package %{{nil}}\n%global __os_install_post %{{nil}}\n%global _build_id_links none\n\n\
         Name: {gui}\nVersion: {version}\nRelease: 1\nSummary: {summary}\nLicense: MIT OR Apache-2.0\nURL: {url}\n\
         Requires: {RPM_GPHOTO}\n{recommends}\n%description\n{description}\n\n\
         %install\nmkdir -p %{{buildroot}}\ncp -a '{root}/.' %{{buildroot}}/\n\n\
         %files\n%dir /usr/share/doc/{gui}\n{files}",
        gui = b.binaries.gui,
        version = package_version(&dist.version),
        summary = super::SUMMARY,
        url = dist.repository,
        description = wrap(&description(&b.display_name), 78).join("\n"),
    ))
}

pub fn appimage(dist: &Dist, pins: &pins::Pins) -> Result<PathBuf, String> {
    let arch = native(dist, "AppImage")?;
    let t = &pins.tool;
    let (tool, runtime) =
        if arch == "x86_64" { (&t.appimagetool_x86_64, &t.appimage_runtime_x86_64) } else { (&t.appimagetool_aarch64, &t.appimage_runtime_aarch64) };
    let tool = pins::fetch(tool, &dist.downloads)?;
    super::set_mode(&tool, 0o755)?;
    let runtime = pins::fetch(runtime, &dist.downloads)?;

    let b = &dist.branding;
    let id = &b.app_id;
    let appdir = dist.fresh("appimage")?.join(format!("{}.AppDir", b.display_name));
    stage(dist, &appdir.join("usr"))?;
    // At the top: the desktop entry and icon that appimagetool and desktop integration read, and the entry point.
    super::write(&appdir.join(format!("{id}.desktop")), desktop_entry(dist)?.as_bytes())?;
    super::write(&appdir.join(format!("{id}.svg")), icon::svg().as_bytes())?;
    super::write(&appdir.join(".DirIcon"), &icon::png(256)?)?;
    let app_run = format!("#!/bin/sh\nhere=\"$(dirname \"$(readlink -f \"$0\")\")\"\nexec \"$here/usr/bin/{}\" \"$@\"\n", b.binaries.gui);
    super::write(&appdir.join("AppRun"), app_run.as_bytes())?;
    normalize_modes(&appdir)?;

    let appimage = dist.out.join(format!("{}.AppImage", dist.stem()));
    let mut appimagetool = Command::new(&tool);
    // Unpacked and run, as appimagetool is itself an AppImage and build machines may lack FUSE.
    appimagetool.env("APPIMAGE_EXTRACT_AND_RUN", "1").env("ARCH", arch).env("VERSION", &dist.version);
    appimagetool.arg("--runtime-file").arg(&runtime).arg(&appdir).arg(&appimage);
    super::execute(&mut appimagetool, "build the AppImage")?;
    super::set_mode(&appimage, 0o755)?;
    Ok(appimage)
}

pub fn flatpak(dist: &Dist, pins: &pins::Pins) -> Result<Vec<PathBuf>, String> {
    native(dist, "Flatpak")?;
    let sources = super::camera_library_sources(dist, pins)?;
    let b = &dist.branding;
    let dir = dist.fresh("flatpak")?;
    let app = dir.join("app");
    stage(dist, &app)?;
    let replace = format!(
        "run it with your own build of libgphoto2, which the sandbox must be able to read: flatpak run \
         --filesystem=/path/to/build --env=FLORASTACK_GPHOTO_LIBRARY=/path/to/build/libgphoto2.so.6 --env=CAMLIBS=... --env=IOLIBS=... {}; \
         or build this Flatpak with your sources in place of the pinned ones (cargo xtask dist writes its manifest)",
        b.app_id
    );
    super::camera_library_notices(dist, pins, &sources, &app.join("share/doc").join(&b.binaries.gui), &replace)?;
    normalize_modes(&app)?;
    for archive in &sources {
        super::copy(archive, &dir.join("sources").join(archive.file_name().ok_or("a source archive has no name")?))?;
    }
    let manifest = dir.join(format!("{}.json", b.app_id));
    let text = serde_json::to_string_pretty(&flatpak_manifest(dist, pins)).map_err(|e| format!("cannot write the Flatpak manifest: {e}"))?;
    super::write(&manifest, text.as_bytes())?;

    super::execute(Command::new("flatpak").args(["remote-add", "--user", "--if-not-exists", "flathub", FLATHUB]), "add the Flathub remote")?;
    let repo = dir.join("repo");
    let mut builder = Command::new("flatpak-builder");
    builder.args(["--user", "--install-deps-from=flathub", "--force-clean", "--disable-rofiles-fuse"]);
    // The state folder outlives the run, so that the libraries are built once per set of pins.
    builder.arg(format!("--state-dir={}", dist.work.join("flatpak-state").display()));
    builder.arg(format!("--repo={}", repo.display())).arg(dir.join("build")).arg(&manifest);
    super::execute(&mut builder, "build the Flatpak")?;
    let bundle = dist.out.join(format!("{}.flatpak", dist.stem()));
    let mut bundler = Command::new("flatpak");
    bundler.args(["build-bundle", &format!("--runtime-repo={FLATHUB}")]).arg(&repo).arg(&bundle).arg(&b.app_id);
    super::execute(&mut bundler, "bundle the Flatpak")?;
    let mut artifacts = vec![bundle];
    artifacts.extend(super::publish_sources(dist, &sources)?);
    Ok(artifacts)
}

/// The manifest: the three libraries from their pinned archives, then the staged app files.
fn flatpak_manifest(dist: &Dist, pins: &pins::Pins) -> serde_json::Value {
    let b = &dist.branding;
    let l = &pins.library;
    let archive = |library: &pins::Library| {
        use pins::Pinned as _;
        json!([{ "type": "archive", "path": format!("sources/{}", library.file_name()), "sha256": library.sha256 }])
    };
    let mut gphoto: Vec<String> = GPHOTO_CONFIGURE.iter().map(|o| (*o).to_owned()).collect();
    gphoto.push("udevscriptdir=/app/lib/udev".to_owned());
    json!({
        "app-id": b.app_id,
        "runtime": "org.freedesktop.Platform",
        "runtime-version": FLATPAK_RUNTIME,
        "sdk": "org.freedesktop.Sdk",
        "command": b.binaries.gui,
        // Windows on Wayland or X11, the GPU, the photos in the home folder, and USB cameras and serial rails.
        "finish-args": ["--share=ipc", "--socket=wayland", "--socket=fallback-x11", "--device=all", "--filesystem=home"],
        "cleanup": [
            "/include", "/lib/pkgconfig", "/lib/*.la", "/lib/*.a", "/lib/libgphoto2/*/*.la", "/lib/libgphoto2_port/*/*.la",
            "/lib/udev", "/bin/libtool", "/bin/libtoolize", "/bin/gphoto2-config", "/bin/gphoto2-port-config",
            "/share/aclocal", "/share/info", "/share/libtool", "/share/man", "/share/doc/libgphoto2", "/share/doc/libgphoto2_port"
        ],
        "modules": [
            { "name": "libltdl", "config-opts": ["--enable-ltdl-install", "--disable-static"], "sources": archive(&l.libtool) },
            // No udev in the sandbox: libusb finds devices through netlink.
            { "name": "libusb", "config-opts": ["--disable-udev", "--disable-static"], "sources": archive(&l.libusb) },
            { "name": "libgphoto2", "config-opts": gphoto, "sources": archive(&l.libgphoto2) },
            {
                "name": b.binaries.gui,
                "buildsystem": "simple",
                "build-commands": ["cp -R bin share /app/"],
                "sources": [{ "type": "dir", "path": "app" }]
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(version: &str) -> Dist {
        let root = crate::workspace_root().unwrap();
        Dist {
            branding: crate::branding::repository(),
            version: version.to_owned(),
            repository: "https://example.org/r".to_owned(),
            target: super::super::Target::parse("x86_64-unknown-linux-gnu").unwrap(),
            gui_package: String::new(),
            cli_package: String::new(),
            cargo_target: root.join("target"),
            work: root.join("target/dist/x86_64-unknown-linux-gnu"),
            downloads: root.join("target/dist/downloads"),
            out: root.join("target/dist/out"),
            root,
        }
    }

    #[test]
    fn glibc_floor_is_the_newest_version_named() {
        let a: &[u8] = b"\0GLIBC_2.2.5\0GLIBC_2.17\0GLIBC_PRIVATE\0";
        let b: &[u8] = b"xGLIBC_2.34\0GLIBC_2.3.4\0";
        assert_eq!(glibc_floor(&[a, b]).as_deref(), Some("2.34"));
        assert_eq!(glibc_floor(&[b"no versions"]), None);
    }

    #[test]
    fn deb_control_is_well_formed() {
        let dist = dist("1.0.0-rc.1");
        let control = deb_control(&dist, "amd64", &package_version(&dist.version), 12345, "2.35");
        let b = &dist.branding;
        assert!(control.starts_with(&format!("Package: {}\nVersion: 1.0.0~rc.1\nArchitecture: amd64\n", b.binaries.gui)));
        assert!(control.contains("Depends: libc6 (>= 2.35), libgcc-s1, libgphoto2-6t64 | libgphoto2-6\n"));
        assert!(control.contains("Installed-Size: 12345\n"));
        // Every line is a field or a continuation; the long description's lines start with a space.
        let description = control.split_once(&format!("Description: {}\n", super::super::SUMMARY)).unwrap().1;
        assert!(!description.is_empty() && description.lines().all(|l| l.starts_with(' ') && l.len() <= 79));
    }

    #[test]
    fn rpm_spec_lists_the_staged_files() {
        let dist = dist("1.0.0");
        let spec = rpm_spec(&dist, Path::new("/tmp/stage"), &["/usr/bin/a".to_owned(), "/usr/bin/b".to_owned()]).unwrap();
        assert!(spec.contains(&format!("Name: {}\nVersion: 1.0.0\n", dist.branding.binaries.gui)));
        assert!(spec.contains("Requires: libgphoto2.so.6()(64bit)\n"));
        assert!(spec.contains("cp -a '/tmp/stage/.' %{buildroot}/\n"));
        assert!(spec.trim_end().ends_with("/usr/bin/a\n/usr/bin/b"));
        assert!(rpm_spec(&dist, Path::new("/tmp/it's"), &[]).is_err());
    }

    #[test]
    fn desktop_entry_and_metainfo_name_the_app() {
        let dist = dist("1.0.0");
        let b = &dist.branding;
        let entry = desktop_entry(&dist).unwrap();
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains(&format!("\nExec={} %F\n", b.binaries.gui)));
        assert!(entry.contains(&format!("\nIcon={}\n", b.app_id)));
        let metainfo = metainfo(&dist).unwrap();
        assert!(metainfo.contains(&format!("<id>{}</id>", b.app_id)));
        assert!(metainfo.contains(&format!("<launchable type=\"desktop-id\">{}.desktop</launchable>", b.app_id)));
        assert!(!metainfo.contains('@'), "every placeholder filled");
    }

    #[test]
    fn the_flatpak_builds_the_libraries_then_the_app() {
        let dist = dist("1.0.0");
        let pins = pins::parse(include_str!("../../../packaging/third-party.toml")).unwrap();
        let manifest = flatpak_manifest(&dist, &pins);
        assert_eq!(manifest["app-id"], dist.branding.app_id.as_str());
        let modules: Vec<&str> = manifest["modules"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(modules, ["libltdl", "libusb", "libgphoto2", dist.branding.binaries.gui.as_str()]);
        let gphoto = &manifest["modules"][2];
        assert_eq!(gphoto["sources"][0]["sha256"], pins.library.libgphoto2.sha256.as_str());
        assert!(gphoto["config-opts"].as_array().unwrap().iter().any(|o| o == "--with-camlibs=ptp2"));
        assert!(!manifest["finish-args"].as_array().unwrap().iter().any(|a| a == "--share=network"), "no network");
    }

    const NOTICE_VALUES: [(&str, &str); 5] =
        [("LIBGPHOTO2_VERSION", "2"), ("LIBUSB_VERSION", "1"), ("LIBTOOL_VERSION", "3"), ("LICENSE_FILES", "COPYING.x"), ("REPLACE", "replace them")];

    #[test]
    fn every_template_is_filled() {
        let dist = dist("1.0.0");
        let readme = dist.template("linux/tarball-README.txt.in", &[]).unwrap();
        assert!(readme.contains(&format!("bin/{}", dist.branding.binaries.gui)));
        let notice = dist.template("gphoto/LIBGPHOTO2.txt.in", &NOTICE_VALUES).unwrap();
        assert!(notice.contains("FLORASTACK_GPHOTO_LIBRARY") && notice.contains("https://example.org/r/releases"));
        assert!(dist.template("gphoto/LIBGPHOTO2.txt.in", &NOTICE_VALUES[..4]).unwrap_err().contains("@REPLACE@"));
    }

    /// Git for Windows checks the templates out with CRLF line endings (`core.autocrlf`, as on CI's Windows runner);
    /// the files made from them have Unix line endings all the same.
    #[test]
    fn templates_checked_out_with_crlf_make_the_same_files() {
        fn made(dist: &Dist) -> Vec<String> {
            vec![
                desktop_entry(dist).unwrap(),
                metainfo(dist).unwrap(),
                dist.template("linux/tarball-README.txt.in", &[]).unwrap(),
                dist.template("gphoto/LIBGPHOTO2.txt.in", &NOTICE_VALUES).unwrap(),
            ]
        }
        let mut dist = dist("1.0.0");
        let expected = made(&dist);
        let root = std::env::temp_dir().join(format!("florastack-dist-crlf-{}", std::process::id()));
        for name in ["linux/app.desktop.in", "linux/metainfo.xml.in", "linux/tarball-README.txt.in", "gphoto/LIBGPHOTO2.txt.in"] {
            let text = std::fs::read_to_string(dist.root.join("packaging").join(name)).unwrap();
            let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
            super::super::write(&root.join("packaging").join(name), crlf.as_bytes()).unwrap();
        }
        dist.root.clone_from(&root);
        let crlf = made(&dist);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(expected[0].starts_with("[Desktop Entry]\n"));
        for (made, expected) in crlf.iter().zip(&expected) {
            assert!(!made.contains('\r') && !expected.contains('\r'));
            assert_eq!(made, expected);
        }
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("", 5), Vec::<String>::new());
        assert_eq!(package_version("1.0.0-rc.1"), "1.0.0~rc.1");
    }
}
