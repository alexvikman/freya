#!/bin/sh
# Prepares a release: sets the version, scans every commit for secrets, and makes the commit
# "Release X.Y.Z" and the tag vX.Y.Z. It pushes nothing; pushing the tag starts the release
# workflow (see docs/RELEASING.md).
#
# Usage: ci/release.sh X.Y.Z
set -eu
cd "$(dirname "$0")/.."

version="${1:-}"
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "usage: ci/release.sh X.Y.Z" >&2
    exit 2
fi
if [ -n "$(git status --porcelain)" ]; then
    echo "release: commit or stash your changes first" >&2
    exit 1
fi
if [ "$(git rev-parse --abbrev-ref HEAD)" != main ]; then
    echo "release: releases are made from main" >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
    echo "release: v$version exists already" >&2
    exit 1
fi
current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
newest=$(printf '%s\n%s\n' "$current" "$version" | sort -t . -k 1,1n -k 2,2n -k 3,3n | tail -n 1)
if [ "$version" = "$current" ] || [ "$newest" != "$version" ]; then
    echo "release: $version is not newer than $current" >&2
    exit 1
fi

gitleaks git --redact --no-banner --log-level warn .

# Should anything below fail, such as a commit that cannot be signed while the key is locked,
# the version goes back: a half-made release would otherwise make the next try "not newer".
trap 'git restore --staged --worktree Cargo.toml Cargo.lock' EXIT

# The workspace version, in [workspace.package], is the first `version = ` line.
awk -v v="$version" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
    Cargo.toml > Cargo.toml.new
mv Cargo.toml.new Cargo.toml
cargo update --workspace --offline --quiet
cargo run --quiet -p freya -- --version | grep -qx "freya $version"

git add Cargo.toml Cargo.lock
git commit --quiet -m "Release $version"
trap - EXIT
git tag -a "v$version" -m "Freya $version"
echo "release: v$version is ready. Publish it with:"
echo "    git push origin main v$version"
