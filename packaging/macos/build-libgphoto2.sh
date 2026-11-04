#!/usr/bin/env bash
# Builds the camera-control libraries of the macOS app: libltdl (GNU Libtool), libusb and libgphoto2 with the options
# in $GPHOTO_CONFIGURE, for arm64 and x86_64, joined into universal files and laid out as the app bundle holds them:
#
#   OUT/Frameworks/            libltdl.7.dylib, libusb-1.0.0.dylib, libgphoto2_port.12.dylib, libgphoto2.6.dylib
#   OUT/PlugIns/libgphoto2/camlibs/   the camera drivers (ptp2.so)
#   OUT/PlugIns/libgphoto2/iolibs/    the port drivers (usb1.so, ptpip.so, ...)
#
# Each library's install name becomes @rpath/<name>, and every reference from one of these files to another is made
# relative to the referring file (@loader_path), so that the app works from any folder. libgphoto2 finds the drivers
# through CAMLIBS and IOLIBS, which the app sets for itself (florastack-capture's gphoto module). Nothing comes from
# Homebrew or any other installed copy: each build sees only its own prefix.
#
# `cargo xtask dist` runs this on macOS with the archives that packaging/third-party.toml pins, after checking them,
# and signs the results with the app. Building needs Xcode's command-line tools and pkg-config.
#
# usage: build-libgphoto2.sh LIBTOOL_ARCHIVE LIBUSB_ARCHIVE LIBGPHOTO2_ARCHIVE OUT
set -euo pipefail

if [ $# -ne 4 ]; then
    echo "usage: $0 LIBTOOL_ARCHIVE LIBUSB_ARCHIVE LIBGPHOTO2_ARCHIVE OUT" >&2
    exit 2
fi
for archive in "$1" "$2" "$3"; do
    [ -f "$archive" ] || { echo "no such archive: $archive" >&2; exit 2; }
done
command -v pkg-config >/dev/null || { echo "pkg-config is required (brew install pkgconf)" >&2; exit 1; }

libtool_archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
libusb_archive=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
gphoto_archive=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
out=$4
work="$out.work"
rm -rf "$out" "$work"
mkdir -p "$out" "$work"
out=$(cd "$out" && pwd)
work=$(cd "$work" && pwd)

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
read -r -a gphoto_options <<< "${GPHOTO_CONFIGURE:---with-camlibs=ptp2 --disable-nls --disable-static}"
jobs=$(sysctl -n hw.ncpu)
frameworks=(libltdl.7.dylib libusb-1.0.0.dylib libgphoto2_port.12.dylib libgphoto2.6.dylib)

# Points the references in FILE to libraries in PREFIX at DIR/<library name>.
relink() {
    local file=$1 dir=$2 prefix=$3 dependency
    otool -L "$file" | tail -n +2 | awk '{print $1}' | while read -r dependency; do
        case $dependency in
            "$prefix"/lib/*) install_name_tool -change "$dependency" "$dir/$(basename "$dependency")" "$file" ;;
        esac
    done
}

build() {
    local arch=$1 host
    case $arch in
        arm64) host=aarch64-apple-darwin ;;
        x86_64) host=x86_64-apple-darwin ;;
    esac
    local prefix="$work/$arch/prefix" src="$work/$arch/src" bundle="$work/$arch/bundle"
    mkdir -p "$prefix" "$src"
    (
        export CC="clang -arch $arch"
        export CFLAGS="-O2 -arch $arch -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
        export CPPFLAGS="-I$prefix/include"
        # Room in the headers for the longer install names written below.
        export LDFLAGS="-arch $arch -L$prefix/lib -headerpad_max_install_names"
        # Only this prefix's libraries: Homebrew's are for one architecture, and aren't what the app ships.
        export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig" PKG_CONFIG_PATH=""
        configure() { ./configure --host="$host" --prefix="$prefix" --disable-dependency-tracking "$@"; }

        tar -xf "$libtool_archive" -C "$src"
        cd "$src"/libtool-*/
        configure --enable-ltdl-install --disable-static
        make -j"$jobs"
        make install

        tar -xf "$libusb_archive" -C "$src"
        cd "$src"/libusb-*/
        configure --disable-static
        make -j"$jobs"
        make install

        tar -xf "$gphoto_archive" -C "$src"
        cd "$src"/libgphoto2-*/
        configure "${gphoto_options[@]}"
        make -j"$jobs"
        make install
    )

    mkdir -p "$bundle/Frameworks" "$bundle/PlugIns/libgphoto2/camlibs" "$bundle/PlugIns/libgphoto2/iolibs"
    for name in "${frameworks[@]}"; do
        cp -L "$prefix/lib/$name" "$bundle/Frameworks/$name"
        install_name_tool -id "@rpath/$name" "$bundle/Frameworks/$name"
        relink "$bundle/Frameworks/$name" "@loader_path" "$prefix"
    done
    cp -L "$prefix"/lib/libgphoto2/*/*.so "$bundle/PlugIns/libgphoto2/camlibs/"
    cp -L "$prefix"/lib/libgphoto2_port/*/*.so "$bundle/PlugIns/libgphoto2/iolibs/"
    for file in "$bundle"/PlugIns/libgphoto2/*/*.so; do
        relink "$file" "@loader_path/../../../Frameworks" "$prefix"
    done
    local file path
    for file in "$bundle"/Frameworks/* "$bundle"/PlugIns/libgphoto2/*/*.so; do
        # Run paths into the build folder, which libtool may record, go.
        otool -l "$file" | awk '$1 == "cmd" { rpath = ($2 == "LC_RPATH") } rpath && $1 == "path" { print $2 }' | while read -r path; do
            case $path in "$prefix"*) install_name_tool -delete_rpath "$path" "$file" ;; esac
        done
        # No library may still be loaded from the build folder. (The default driver folders that libgphoto2 keeps
        # as strings may name it: CAMLIBS and IOLIBS override them.)
        if otool -L "$file" | tail -n +2 | grep -q "$prefix"; then
            echo "$arch: $file still loads a library from $prefix:" >&2
            otool -L "$file" >&2
            exit 1
        fi
    done
}

build arm64
build x86_64

(cd "$work/arm64/bundle" && find . -type f | sort) > "$work/arm64.files"
(cd "$work/x86_64/bundle" && find . -type f | sort) > "$work/x86_64.files"
if ! diff -u "$work/arm64.files" "$work/x86_64.files"; then
    echo "the arm64 and x86_64 builds made different files" >&2
    exit 1
fi
while read -r file; do
    mkdir -p "$out/$(dirname "$file")"
    lipo -create "$work/arm64/bundle/$file" "$work/x86_64/bundle/$file" -output "$out/$file"
done < "$work/arm64.files"
rm -rf "$work"
echo "camera libraries for arm64 and x86_64 in $out:"
(cd "$out" && find . -type f | sort)
