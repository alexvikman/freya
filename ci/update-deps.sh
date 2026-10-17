#!/bin/sh
# Updates dependencies as a step of its own, never in passing (see docs/DEPENDENCIES.md).
#
# Usage: ci/update-deps.sh [crate...]   (no crates: everything within Cargo.toml's versions)
#
# Prints every crate version that goes or comes, then runs the same checks as
# `ci/check.sh deps`. A version that nobody trusted has audited stops at cargo-vet; look at
# what changed with `cargo vet diff <crate> <old> <new>` before certifying or exempting it.
set -eu
cd "$(dirname "$0")/.."

if ! git diff --quiet -- Cargo.lock; then
    echo "Cargo.lock has uncommitted changes; commit or revert them first." >&2
    exit 1
fi

# "name version" for every crate in Cargo.lock.
crates() {
    awk -F'"' '/^name = / { name = $2 } /^version = "/ { print name, $2 }' Cargo.lock | sort
}

before=$(mktemp)
trap 'rm -f "$before"' EXIT
crates >"$before"

packages=""
for crate in "$@"; do
    packages="$packages -p $crate"
done
# shellcheck disable=SC2086 # one word per crate
cargo update $packages

changes=$(crates | diff "$before" - | sed -n 's/^</  -/p; s/^>/  +/p')
echo
echo "Changes (- removed, + added):"
echo "${changes:-  none}"
echo
cargo deny check
cargo vet
