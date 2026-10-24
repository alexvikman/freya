#!/bin/sh
# Packages Freya, or Freya Server, for macOS, for this Mac's architecture, into target/dist/:
#
#   Freya-<version>-macos-<arch>.app.zip         what Freya's updater installs
#   Freya-<version>-macos-<arch>.dmg             for installing by hand
#   FreyaServer-<version>-macos-<arch>.app.zip   the same for Freya Server
#   FreyaServer-<version>-macos-<arch>.dmg
#
# The app is signed with the hardened runtime: ad hoc by default, which is enough for an app
# that Freya's updater installs (it is never quarantined) but makes Gatekeeper warn about a
# downloaded one. With a Developer ID it is also notarized, and the ticket stapled, so
# downloads open without a warning. Credentials are only read from the environment:
#
#   FREYA_MACOS_IDENTITY   the signing identity, e.g. "Developer ID Application: Name (TEAMID)";
#                          default "-" (ad hoc)
#   FREYA_NOTARY_PROFILE   a keychain profile made with `xcrun notarytool store-credentials`, or
#   APPLE_API_KEY_ID, APPLE_API_ISSUER, APPLE_API_KEY_PATH
#                          an App Store Connect API key (as in CI)
#
# Usage: ci/package-macos.sh [--skip-build] [freya | server]
set -eu
cd "$(dirname "$0")/.."

build=true
if [ "${1:-}" = "--skip-build" ]; then
    build=false
    shift
fi
case "${1:-freya}" in
    freya) package=freya product=Freya prefix=Freya ;;
    server) package=freya-server product="Freya Server" prefix=FreyaServer ;;
    *) echo "usage: ci/package-macos.sh [--skip-build] [freya | server]" >&2; exit 2 ;;
esac

identity="${FREYA_MACOS_IDENTITY:--}"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
case "$(uname -m)" in
    arm64) arch=aarch64 ;;
    *) arch=$(uname -m) ;;
esac
app="target/packages/$product.app"
dist=target/dist
name="$prefix-$version-macos-$arch"

if $build; then
    cargo packager -p "$package" --release --formats app
fi

# What an earlier run left of this product goes, an older version's too; the other product's
# packages stay.
mkdir -p "$dist"
rm -f "$dist/$prefix"-* "$dist/notarize.zip"

# The bundle signs its program and seals its resources.
xattr -cr "$app"
sign() {
    if [ "$identity" = "-" ]; then
        codesign --force --options runtime --sign - "$1"
    else
        codesign --force --options runtime --timestamp --sign "$identity" "$1"
    fi
}
sign "$app"
codesign --verify --deep --strict --verbose=2 "$app"

notarize() {
    if [ -n "${FREYA_NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$1" --wait --keychain-profile "$FREYA_NOTARY_PROFILE"
    else
        xcrun notarytool submit "$1" --wait \
            --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER"
    fi
}
can_notarize=false
if [ "$identity" != "-" ] && { [ -n "${FREYA_NOTARY_PROFILE:-}" ] || [ -n "${APPLE_API_KEY_ID:-}" ]; }; then
    can_notarize=true
fi

if $can_notarize; then
    ditto -c -k --keepParent "$app" "$dist/notarize.zip"
    notarize "$dist/notarize.zip"
    rm "$dist/notarize.zip"
    xcrun stapler staple "$app"
elif [ "$identity" != "-" ]; then
    echo "package-macos: no notarization credentials; the app is signed but not notarized" >&2
fi
ditto -c -k --keepParent "$app" "$dist/$name.app.zip"

# The disk image: the app and a link to Applications, to drag it onto.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/$product.app"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname "$product $version" -srcfolder "$stage" -fs HFS+ -format UDZO \
    -ov "$dist/$name.dmg"
if [ "$identity" != "-" ]; then
    codesign --force --timestamp --sign "$identity" "$dist/$name.dmg"
    if $can_notarize; then
        notarize "$dist/$name.dmg"
        xcrun stapler staple "$dist/$name.dmg"
    fi
fi

echo "package-macos: $(cd "$dist" && ls "$prefix"-*)"
