# Builds and checks Freya for Linux, and for Windows with MinGW, without GitHub.
# Used by ci/check.sh.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl build-essential pkg-config cmake file git shellcheck \
        libasound2-dev libfontconfig1-dev libxkbcommon-dev fonts-dejavu-core libfuse2 \
        gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 \
        xvfb x11-apps imagemagick sway grim libgl1-mesa-dri libegl1 procps \
        libx11-6 libx11-xcb1 libxcursor1 libxi6 libxrandr2 libxkbcommon-x11-0 \
        libwayland-client0 libwayland-cursor0 libwayland-egl1 \
    && rm -rf /var/lib/apt/lists/*


# The Rust in rust-toolchain.toml, passed in by ci/check.sh, so the mounted checkout uses the
# toolchain that is already here instead of downloading it on every run.
ARG RUST_VERSION
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN test -n "$RUST_VERSION" && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
        --default-toolchain "$RUST_VERSION" \
        --component clippy,rustfmt --target x86_64-pc-windows-gnu

# MinGW for the Windows target, including the C code built by cmake (libopus). libopus is
# built with GCC's stack protection, which MinGW keeps in libssp; MSVC builds do not need it.
RUN printf '#!/bin/sh\nexec x86_64-w64-mingw32-gcc "$@" -lssp\n' > /usr/local/bin/mingw-link \
    && chmod +x /usr/local/bin/mingw-link
ENV CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
    CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++ \
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=mingw-link

# Secret scanning (ci/check.sh secrets).
COPY install-gitleaks.sh /tmp/
RUN sh /tmp/install-gitleaks.sh && rm /tmp/install-gitleaks.sh

# The GUI test runs as a normal user: Sway refuses to run as root.
RUN useradd --create-home tester

WORKDIR /src
