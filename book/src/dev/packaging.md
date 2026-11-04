# Packaging and releasing

`cargo xtask dist` builds the release packages of one platform, and `.github/workflows/release.yml` runs it for
every platform when a version tag is pushed, then drafts a GitHub release with everything it made. The packages
take the product's names from [`branding.toml`](https://github.com/gabrielsanchez/florastack/blob/main/branding.toml): the display name, the two binary names and the
app ID. Release packaging refuses an app ID that still contains the `OWNER` placeholder.

## What a release contains

| Platform | Packages | Camera control |
|---|---|---|
| Windows x64 and arm64 | `FloraStack-<version>-windows-<arch>.msi` (installs both apps into Program Files, a Start-menu shortcut, and optionally the folder on `PATH`), and the same files in a portable `.zip` | Never: watch-folder capture and serial rails |
| macOS (one universal file, Apple silicon and Intel) | `FloraStack-<version>-macos-universal.dmg` with `FloraStack.app`, which also holds the CLI (`Contents/MacOS/florastack-cli`) | libgphoto2 inside the app |
| Linux x86_64 and aarch64 | `.AppImage`, `.deb`, `.rpm`, `.flatpak` and a `.tar.gz` | The distribution's libgphoto2, except in the Flatpak, which carries its own |
| Every platform | `florastack-cli-<version>-<platform>.zip` or `.tar.gz`, the command-line tool alone | As above, from the system (Homebrew's on macOS) |
| Python | `florastack-<version>-cp39-abi3-<platform>.whl`, one wheel per platform for every Python from 3.9 (the bindings use the stable ABI) | Not used |

Every package but the wheel includes `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE` (the wheel names its license in
its metadata). The release also carries the source archives
of the libraries that the macOS app and the Flatpak include (see below), and `SHA256SUMS.txt`, which
`sha256sum --check SHA256SUMS.txt` verifies.

On Windows the desktop app is a GUI program, which opens no console, and the CLI a console program; `dist` checks
both executables' subsystem before packaging them. Started from a terminal, the desktop app returns at once and
prints nothing (`--help`, `--version`, the log), unless its output is redirected: `florastack.exe --version | more`.

The Linux binaries are built on Ubuntu 22.04, so they need glibc 2.35 or newer. The .deb depends on that `libc6`,
on `libgphoto2-6t64 | libgphoto2-6`, and recommends the graphics and windowing libraries that the app loads at run
time; the .rpm requires `libgphoto2.so.6()(64bit)`, which every RPM distribution provides under its own package name.
The Flatpak runs on the freedesktop.org 26.08 runtime from Flathub, with access to the home folder, the GPU and USB
devices, and no network.

## Building packages locally

```sh
cargo xtask dist                                     # everything for this machine's platform
cargo xtask dist --formats zip,cli                   # only some formats
cargo xtask dist --target universal-apple-darwin     # a universal macOS build (on a Mac)
```

The packages land in `target/dist/<target>/artifacts/`, with their `SHA256SUMS.txt` (`--out DIR` puts them in an
empty folder instead). `cargo xtask dist --help` lists the targets and formats. Each format needs its tools, and
`dist` names any that are missing before it builds anything:

- Windows: the Visual Studio C++ build tools, as for any build, and `cargo install --locked cargo-wix --version 0.3.9`
  for the .msi. The WiX Toolset 3.14.1 is downloaded and checked by xtask, so it needn't be installed. Building for
  arm64 on an x64 machine needs Visual Studio's "MSVC ARM64 build tools" and `rustup target add aarch64-pc-windows-msvc`.
- macOS: Xcode's command-line tools (`xcode-select --install`) and `brew install pkgconf`, and for a universal build
  `rustup target add aarch64-apple-darwin x86_64-apple-darwin`. The first .dmg also builds the camera libraries
  (a few minutes); later ones reuse them.
- Linux: `dpkg-deb` for the .deb, `rpmbuild` for the .rpm (`sudo apt-get install rpm` on Debian and Ubuntu), and
  `flatpak` and `flatpak-builder` for the Flatpak, which adds the Flathub remote for your user and installs the
  freedesktop.org SDK there on its first run. appimagetool and the AppImage runtime are downloaded and checked by
  xtask. AppImages and Flatpaks are built for the architecture of the machine that builds them.
- The wheel: Python 3.9 or newer (`PYO3_PYTHON` selects the interpreter); xtask installs maturin in a virtual
  environment of its own.

The apps are built with the `dist` profile (the release profile, with symbols stripped) and `--locked`.
Downloads go into `target/dist/downloads/`, checked against the SHA-256 in
[`packaging/third-party.toml`](https://github.com/gabrielsanchez/florastack/blob/main/packaging/third-party.toml), where every downloaded source and tool is pinned.

To check the packages: install the .msi (`msiexec /i <file>.msi`) or unpack the .zip on Windows; open the .dmg and
drag the app to Applications on macOS; on Linux, `sudo apt install ./<file>.deb`, `sudo dnf install ./<file>.rpm`,
`flatpak install --user <file>.flatpak`, or `chmod +x` and run the AppImage; and `pip install <file>.whl` in a
virtual environment, then `python -c "import florastack"`.

## Releasing

1. Set the new version in the root `Cargo.toml` (`workspace.package.version`) and in
   `python/florastack/pyproject.toml`; `dist` refuses to run when they differ. Commit, and merge to `main`.
2. Tag the commit `v<version>` and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.
3. The Release workflow builds every platform (about an hour), checks every package against its run's
   `SHA256SUMS.txt`, and creates a **draft** release with all of them and the merged `SHA256SUMS.txt`.
4. Review the draft (the generated notes, the files), then publish it.

A tag with a suffix, such as `v0.2.0-rc.1`, makes a pre-release of the same version; `dist` checks that the tag
starts with `v` and the workspace version. To test the workflow, push such a tag to a fork: without signing secrets,
the macOS app comes out signed ad hoc and not notarized, and everything else is the same. Delete the draft and the
tag afterwards.

## Signing and notarization

The macOS app is signed with a Developer ID and the hardened runtime, and the .dmg is notarized and stapled, when
the repository has these secrets (Settings > Secrets and variables > Actions):

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE_P12` | The "Developer ID Application" certificate and its private key, exported as a .p12 file, in base64 (`base64 -i cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | The .p12 file's password |
| `APPLE_SIGNING_IDENTITY` | The certificate's name, such as `Developer ID Application: Jane Doe (TEAMID1234)` |
| `APPLE_NOTARY_KEY` | An App Store Connect API key (the text of its .p8 file), with the Developer role |
| `APPLE_NOTARY_KEY_ID` | That key's ID |
| `APPLE_NOTARY_ISSUER` | The issuer ID of the account's API keys |

Without the certificate, the app is signed ad hoc, which Apple silicon requires of all code: users then open it the
first time with right-click > Open, or allow it in System Settings > Privacy & Security. Notarization needs both
the certificate and the key. Locally, `cargo xtask dist` signs when `APPLE_SIGNING_IDENTITY` names a certificate in
your keychain, and notarizes when `APPLE_NOTARY_KEY_PATH` (the .p8 file), `APPLE_NOTARY_KEY_ID` and
`APPLE_NOTARY_ISSUER` are set too.

The app's entitlements ([`packaging/macos/entitlements.plist`](https://github.com/gabrielsanchez/florastack/blob/main/packaging/macos/entitlements.plist)) turn off
library validation and nothing else, so that the signed app can load a libgphoto2 that its user built (below).

Windows packages and the Linux packages aren't signed yet.

## Camera control and libgphoto2

Built-in camera control uses [libgphoto2](https://gphoto.sourceforge.io/proj/libgphoto2/), which is licensed under the
LGPL-2.1. FloraStack never links it: florastack-capture loads it at run time, so any copy with the same interface
(ABI version 6) can take the place of the one a package uses. The Linux and macOS builds enable the `gphoto`
feature; Windows builds never do, and never include the library.

- **The macOS app** includes libgphoto2 2.5.34, with libusb and libltdl (GNU Libtool) that it needs, both also
  LGPL-2.1. [`packaging/macos/build-libgphoto2.sh`](https://github.com/gabrielsanchez/florastack/blob/main/packaging/macos/build-libgphoto2.sh) builds them from the
  pinned source archives for both architectures, with libgphoto2's PTP driver, which serves the cameras whose focus
  FloraStack drives. They are in `FloraStack.app/Contents/Frameworks/`, with the drivers in
  `Contents/PlugIns/libgphoto2/`; the app sets `CAMLIBS` and `IOLIBS` to those folders when it starts.
- **The Flatpak** builds the same three libraries from the same archives into `/app/lib`, with the same options.
- **The .deb and .rpm** depend on the distribution's libgphoto2 package, and the tarball and AppImage use whichever
  is installed (`libgphoto2.so.6`). Installing the distribution's package also installs the device rules that let
  users open cameras; the Flatpak needs them from the host as well.
- **The CLI archive for macOS** uses Homebrew's libgphoto2 (`brew install libgphoto2`), as a build from source does.

The LGPL asks three things of a package that includes these libraries, and the packages do them:

1. **The license and a notice.** The macOS app (`Contents/Resources/licenses/`) and the Flatpak
   (`/app/share/doc/florastack/`) carry each library's license text and `LIBGPHOTO2.txt`, which says what they are,
   how to replace them and where their source is.
2. **The source.** The release workflow attaches the exact source archives the libraries were built from
   (`libgphoto2-2.5.34.tar.xz`, `libusb-1.0.30.tar.bz2`, `libtool-2.6.2.tar.xz`) to every release, and the build
   steps are in this repository.
3. **Replaceability.** A user can swap in their own build:
   - Anywhere: set `FLORASTACK_GPHOTO_LIBRARY` to the absolute path of their libgphoto2 shared library before
     starting FloraStack, and `CAMLIBS` and `IOLIBS` to its driver folders if they aren't where it was built to find
     them. For example, on macOS with Homebrew's:
     `FLORASTACK_GPHOTO_LIBRARY=/opt/homebrew/lib/libgphoto2.6.dylib open -a FloraStack` (Homebrew's library knows
     its own drivers' folders).
   - In the macOS app, they can also replace the files in `Contents/Frameworks` and `Contents/PlugIns/libgphoto2`,
     and then sign the app again, for their own use ad hoc: `codesign --force --deep --sign - FloraStack.app`.
     Library validation is off, so the signed app accepts a library signed by someone else.
   - In the Flatpak: `flatpak run --filesystem=/path/to/build --env=FLORASTACK_GPHOTO_LIBRARY=/path/to/build/libgphoto2.so.6 io.github.<owner>.FloraStack`,
     or build the Flatpak from other sources (`cargo xtask dist --formats flatpak` writes its manifest into
     `target/dist/<target>/flatpak/`).

## The pieces

- [`xtask/src/dist/`](https://github.com/gabrielsanchez/florastack/tree/main/xtask/src/dist/): `mod.rs` (targets, formats, the build, the checks), `windows.rs`,
  `macos.rs`, `linux.rs`, `pins.rs` (the pinned downloads) and `icon.rs`.
- [`packaging/windows/main.wxs`](https://github.com/gabrielsanchez/florastack/blob/main/packaging/windows/main.wxs): the installer, whose names xtask passes as WiX
  variables. Its `UpgradeCode` must never change.
- [`packaging/linux/`](https://github.com/gabrielsanchez/florastack/tree/main/packaging/linux/): the desktop entry, the AppStream metadata and the tarball's README.
- [`packaging/macos/`](https://github.com/gabrielsanchez/florastack/tree/main/packaging/macos/): the camera libraries' build and the entitlements.
- [`packaging/gphoto/LIBGPHOTO2.txt.in`](https://github.com/gabrielsanchez/florastack/blob/main/packaging/gphoto/LIBGPHOTO2.txt.in): the camera libraries' notice.
- The icon is a placeholder drawn by `xtask/src/dist/icon.rs` (SVG for Linux, PNG, ICO and ICNS), until a designed
  one replaces it.
