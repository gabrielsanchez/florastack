# Installing FloraStack

Every release on the [Releases page](https://github.com/gabrielsanchez/florastack/releases) has packages for
Windows, macOS and Linux, the command-line tool on its own, and a Python package. Each is listed in the release's
`SHA256SUMS.txt`: `sha256sum --check SHA256SUMS.txt --ignore-missing` in the download folder checks the ones you took
(`Get-FileHash <file>` in PowerShell shows a file's hash to compare by eye).

You need no GPU: FloraStack stacks on the graphics card when it has one that passes a self-test, and on the CPU
otherwise. Stacks of hundreds of 24-megapixel frames work on a machine with 8 GB of memory, since stacking keeps one
frame in memory at a time and moves its work to disk beyond a [memory budget](performance.md).

## Windows

Windows 10 and 11, on x64 or Arm (`windows-x64` or `windows-arm64` in the file names).

- **The installer**, `FloraStack-<version>-windows-x64.msi`, puts the desktop app and the command-line tool in
  `C:\Program Files\FloraStack`, adds FloraStack to the Start menu and, if you leave that option on, the folder to
  `PATH`, so that `florastack-cli` works in any terminal. Installing a newer version replaces the older one;
  **Settings > Apps** uninstalls it.
- **The portable .zip** holds the same files. Unpack it anywhere and run `florastack.exe`; nothing is installed.

The packages aren't signed yet, so Windows SmartScreen may warn that the publisher is unknown: choose
**More info > Run anyway**.

The desktop app is a windowed program. Started from a terminal it returns at once and prints nothing, unless its
output is redirected (`florastack.exe --version | more`); the command-line tool, `florastack-cli.exe`, is the one for
terminals and scripts.

## macOS

macOS 11 or later, on Apple silicon and Intel alike: `FloraStack-<version>-macos-universal.dmg` holds one app for
both. Open the .dmg and drag FloraStack to Applications. The command-line tool is inside the app:
`/Applications/FloraStack.app/Contents/MacOS/florastack-cli`, which you can link onto your `PATH`:

```sh
sudo ln -s /Applications/FloraStack.app/Contents/MacOS/florastack-cli /usr/local/bin/florastack-cli
```

A release whose app was signed with a Developer ID and notarized opens like any other app. A build without that
signature opens the first time with right-click (or Control-click) **> Open**, or **System Settings > Privacy &
Security > Open Anyway**.

The app includes libgphoto2 for [camera control](capture.md).

## Linux

x86_64 and aarch64, on distributions with glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36 and later). Pick
the format your system prefers:

| Package | Install | Notes |
|---|---|---|
| `.deb` | `sudo apt install ./florastack_<version>_amd64.deb` | Debian, Ubuntu and their derivatives; brings libgphoto2 for camera control |
| `.rpm` | `sudo dnf install ./florastack-<version>-1.x86_64.rpm` | Fedora, openSUSE and others; brings libgphoto2 |
| `.flatpak` | `flatpak install --user FloraStack-<version>-linux-x86_64.flatpak` | Any distribution with Flatpak; carries its own libgphoto2; runs on the freedesktop.org runtime from Flathub |
| `.AppImage` | `chmod +x FloraStack-<version>-linux-x86_64.AppImage`, then run it | One file, nothing installed; uses the system's libgphoto2 if there is one |
| `.tar.gz` | Unpack, then run `bin/florastack` | The binaries with the desktop entry and icons, to install by hand |

The Flatpak may read and write your home folder, use the GPU and USB devices, and nothing else: no network. For a
camera to be usable, the host needs libgphoto2's device rules, which your distribution's `libgphoto2` package
installs.

On a Wayland desktop the app runs through XWayland when an X server is available, because dropping files on a native
Wayland window doesn't work yet; set `FLORASTACK_WAYLAND=1` to use Wayland itself.

## The command-line tool alone

`florastack-cli-<version>-<platform>.zip` (Windows) or `.tar.gz` (macOS, Linux) holds the command-line tool and the
license files, for servers and scripts. [The command-line tool](../reference/cli.md) describes it.

## Python

The Python package needs Python 3.9 or newer; one wheel per platform serves every version:

```sh
python -m pip install florastack-<version>-cp39-abi3-<platform>.whl
python -c "import florastack"
```

[Automation, MCP and Python](automation.md) shows how to use it.

## Building from source

Install [rustup](https://rustup.rs) and, on Windows, the Visual Studio C++ build tools ("Desktop development with
C++"). In a clone of the repository:

```sh
cargo run -p florastack --release        # the desktop app
cargo run -p florastack-cli --release -- --help
```

The toolchain is pinned in `rust-toolchain.toml`, and rustup installs it on the first build, which also downloads the
LightCraft image crates FloraStack uses. On Linux and macOS, `--features gphoto` adds built-in camera control, with
libgphoto2 installed by your package manager (`sudo apt install libgphoto2-6`, `brew install libgphoto2`).
[Packaging and releasing](../dev/packaging.md) describes how the packages above are built.
