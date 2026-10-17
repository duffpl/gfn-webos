# Build and test environment for Linux and CI: the webosbrew NDK, the pinned nightly Rust for the
# TV build, and the simulator's host libraries. x86_64 only: the NDK release pinned here has no
# aarch64 build (see NDK_RELEASE).
#
#   docker build --platform linux/amd64 -t gfn-webos-build docker
#   docker run --rm -v "$PWD:/src" -w /src gfn-webos-build scripts/build-ipk.sh
FROM ubuntu:24.04

ARG TARGETARCH
# Bumping either rebuilds the image in CI (its tag is a hash of this file). The last NDK with GCC
# 12, the compiler the TV builds were tested with. GCC 14 and later make implicit declarations
# errors, and aws-lc calls getentropy(), which glibc 2.12's headers lack; native/compat supplies
# it at link time.
ARG NDK_RELEASE=webos-b17b4cc
ARG RUST_NIGHTLY=nightly-2026-10-08

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates clang cmake curl file git libclang-dev libdbus-1-dev libsdl2-dev \
        pkg-config python3 \
    && rm -rf /var/lib/apt/lists/*

ENV WEBOS_SDK=/opt/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot
RUN [ "$TARGETARCH" = amd64 ] || { echo "$NDK_RELEASE has no NDK for $TARGETARCH" >&2; exit 1; } \
    && mkdir -p /opt/webos-ndk \
    && curl -fsSL "https://github.com/openlgtv/buildroot-nc4/releases/download/$NDK_RELEASE/arm-webos-linux-gnueabi_sdk-buildroot.tar.gz" \
        | tar -xz -C /opt/webos-ndk \
    && "$WEBOS_SDK/relocate-sdk.sh"

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH \
    WEBOS_RUST_TOOLCHAIN=$RUST_NIGHTLY
# The nightly serves the host tests too, so the image carries one toolchain.
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "$RUST_NIGHTLY" \
        --component rust-src \
    && chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME"

# Checkouts are mounted or created by another user than the one git runs as.
RUN git config --system --add safe.directory '*'
