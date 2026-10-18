#!/bin/sh
# shellcheck disable=SC2016 # the commands are single-quoted: the container's shell expands them
# Checks Freya on Linux and cross-compiles it for Windows, in Docker.
#
# Usage: ci/check.sh [linux | windows | deps | secrets | workflows | package | gui]
#        (default: linux, windows, deps, secrets, workflows)
#
# `deps` checks the dependencies (see docs/DEPENDENCIES.md): known vulnerabilities, malicious
# or abandoned crates, licenses and sources with cargo-deny, and that every crate version is
# audited or knowingly exempted with cargo-vet.
#
# `secrets` scans every commit for credentials with gitleaks, as the pre-commit hook in
# .githooks does for each new one (see docs/RELEASING.md).
#
# `workflows` checks the GitHub workflows with actionlint, which also runs shellcheck on their
# scripts.
#
# `gui` runs Freya on virtual X11 and Wayland screens and writes screenshots, logs and CPU use
# to target/ci-gui/ in the checkout.
#
# The source is mounted read-only; build output and downloaded crates live in Docker volumes,
# so nothing is written to the checkout. The build gets at most 6 CPUs and 3.5 GB, so other
# containers keep running smoothly; FREYA_CI_CPUS and FREYA_CI_MEMORY change that. Debug info
# is kept to line numbers, which keeps linking the test binaries within that memory, and
# release builds use thin rather than whole-program optimization, which needs more memory than
# a container usually has.
set -eu
cd "$(dirname "$0")/.."

rust=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
docker build --quiet --build-arg RUST_VERSION="$rust" -t freya-ci ci >/dev/null
# Build output is kept per checkout: the source is always /src in the container, so two
# checkouts sharing one volume would each find the other's newer build and skip rebuilding.
target_volume="freya-ci-target-$(printf '%s' "$PWD" | cksum | cut -d ' ' -f 1)"
run() {
    # `extra` holds whole options (another volume): split into words on purpose.
    # shellcheck disable=SC2086
    docker run --rm \
        --cpus "${FREYA_CI_CPUS:-6}" --memory "${FREYA_CI_MEMORY:-3500m}" \
        -v "$PWD:/src:ro" ${extra:-} \
        -v "$target_volume":/target \
        -v freya-ci-cargo:/usr/local/cargo/registry \
        -e CARGO_TARGET_DIR=/target \
        -e CARGO_PROFILE_DEV_DEBUG=line-tables-only \
        -e CARGO_PROFILE_RELEASE_LTO=thin -e CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
        freya-ci sh -euxc "$1"
}

linux='cargo fmt --all --check
       cargo clippy --all-targets -- -D warnings
       cargo test --all
       shellcheck /src/ci/*.sh /src/.githooks/pre-commit'
windows='cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings
         cargo build --release -p freya --target x86_64-pc-windows-gnu
         file /target/x86_64-pc-windows-gnu/release/freya.exe
         x86_64-w64-mingw32-objdump -h /target/x86_64-pc-windows-gnu/release/freya.exe | grep -q " .rsrc " &&
             echo "freya.exe has its icon and version resources"'
# The tools are pinned and built once into the volume. cargo-vet reads a copy of the
# checked-in supply-chain/ folder (it takes a lock file there, and the source is read-only);
# cargo-deny downloads the current RustSec advisory database.
# actionlint's own image, pinned to the exact build checked when it was added.
workflows() {
    docker run --rm -v "$PWD:/repo:ro" -w /repo \
        rhysd/actionlint:1.7.12@sha256:b1934ee5f1c509618f2508e6eb47ee0d3520686341fec936f3b79331f9315667 \
        -color=false
    echo "workflows: actionlint found nothing"
}
# Git refuses to read a repository owned by another user, as the mounted checkout is.
secrets='git config --global --add safe.directory /src
         gitleaks git --redact --no-banner /src'
deps='cargo install cargo-deny --locked --version 0.20.2 --root /target/tools
      cargo install cargo-vet --locked --version 0.10.2 --root /target/tools
      export PATH="/target/tools/bin:$PATH"
      cargo deny check
      cp -R /src/supply-chain /tmp/supply-chain
      cargo vet --locked --store-path /tmp/supply-chain'
# cargo-packager writes next to the source, so it works on a copy. The checkout's own target
# folder holds this Mac's binaries; the copy's target is the Linux build in the volume instead.
package='cargo install cargo-packager --locked --version 0.11.8 --root /target/tools
         mkdir /tmp/freya && tar -C /src --exclude=./target -cf - . | tar -C /tmp/freya -xf -
         ln -s /target /tmp/freya/target && cd /tmp/freya
         rm -rf /target/packages
         sh /src/ci/appimage-tools.sh
         APPIMAGE_EXTRACT_AND_RUN=1 /target/tools/bin/cargo-packager -p freya --release \
             --formats appimage,deb
         file /target/release/freya
         ls -la /target/packages'

case "${1:-all}" in
    linux) run "$linux" ;;
    windows) run "$windows" ;;
    deps) run "$deps" ;;
    secrets) run "$secrets" ;;
    workflows) workflows ;;
    package) run "$package" ;;
    gui)
        rm -rf target/ci-gui && mkdir -p target/ci-gui
        extra="-v $PWD/target/ci-gui:/out"
        run "sh /src/ci/gui-test.sh" ;;
    all) run "$linux"; run "$windows"; run "$deps"; run "$secrets"; workflows ;;
    *) echo "usage: ci/check.sh [linux | windows | deps | secrets | workflows | package | gui]" >&2; exit 2 ;;
esac
