#!/bin/sh
# Puts the tools cargo-packager builds AppImages with (AppRun, linuxdeploy and its AppImage
# plugin) where it looks for them, at pinned versions checked against their SHA-256. Left to
# itself, cargo-packager downloads them from release URLs whose files can be replaced
# (linuxdeploy-plugin-appimage's "continuous"), and they would end up in signed releases
# unchecked. If a file changes upstream, this fails rather than build with it.
#
# Usage: ci/appimage-tools.sh   (on Linux, before `cargo packager --formats appimage`)
set -eu

arch=$(uname -m)
case "$arch" in
    x86_64)
        apprun=f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f
        linuxdeploy=e762bea85c8eb0d4b3508d46e5c1f037f717d0f9303ae3b4aafc8b04991fa1ef
        plugin=992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3
        ;;
    aarch64)
        apprun=072f17c0895a85c490282fe5395c5007e5fc75da727e553b3b8fb680feb11578
        linuxdeploy=b12b5cc57bd0921e1f98d73f58aa364503bc1a27f54b7a69fd2870bce7fa2f55
        plugin=83c292149274965a865dcd44c135cfca8ba28c6b7de3eb628d4b8b5f248af17c
        ;;
    *)
        echo "appimage-tools: no pinned tools for $arch" >&2
        exit 1
        ;;
esac

# Where cargo-packager 0.11 keeps them (dirs::cache_dir()/.cargo-packager/AppImage).
dir="${XDG_CACHE_HOME:-$HOME/.cache}/.cargo-packager/AppImage"
mkdir -p "$dir"

fetch() { # name, url, sha256
    tmp="$dir/.$1.download"
    curl -fsSL -o "$tmp" "$2"
    if ! printf '%s  %s\n' "$3" "$tmp" | sha256sum --check --status; then
        rm -f "$tmp"
        echo "appimage-tools: $2 is not the pinned file (SHA-256 differs)" >&2
        exit 1
    fi
    chmod 764 "$tmp"
    mv "$tmp" "$dir/$1"
    echo "appimage-tools: $1 checked"
}

fetch "AppRun-$arch" \
    "https://github.com/tauri-apps/binary-releases/releases/download/apprun-old/AppRun-$arch" \
    "$apprun"
fetch "linuxdeploy-$arch.AppImage" \
    "https://github.com/tauri-apps/binary-releases/releases/download/linuxdeploy/linuxdeploy-$arch.AppImage" \
    "$linuxdeploy"
fetch "linuxdeploy-plugin-appimage.AppImage" \
    "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-appimage-$arch.AppImage" \
    "$plugin"
