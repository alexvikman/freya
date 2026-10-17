#!/bin/sh
# Installs gitleaks for Linux into a folder (default /usr/local/bin): a pinned version, checked
# against the checksum it was published with. Used by ci/Dockerfile and the GitHub workflows.
set -eu
version=8.30.1
case "$(uname -m)" in
    x86_64 | amd64)
        arch=x64
        sum=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb ;;
    aarch64 | arm64)
        arch=arm64
        sum=e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080 ;;
    *)
        echo "install-gitleaks: no gitleaks build for $(uname -m)" >&2
        exit 1 ;;
esac
file="gitleaks_${version}_linux_${arch}.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$file" "https://github.com/gitleaks/gitleaks/releases/download/v${version}/$file"
echo "$sum  $tmp/$file" | sha256sum -c -
tar -xzf "$tmp/$file" -C "$tmp" gitleaks
install -m 755 "$tmp/gitleaks" "${1:-/usr/local/bin}/gitleaks"
