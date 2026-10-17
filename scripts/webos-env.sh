#!/usr/bin/env bash
# Source this to cross-compile for webOS (32-bit ARM userland, glibc 2.12) with the webosbrew NDK.
# Codegen and link flags live in .cargo/config.toml; this only points the C toolchain at the NDK.

_gfn_root="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
WEBOS_SDK="${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
WEBOS_SYSROOT="$WEBOS_SDK/arm-webos-linux-gnueabi/sysroot"
export WEBOS_SDK WEBOS_SYSROOT

[ -d /opt/homebrew/opt/rustup/bin ] && export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export PATH="$HOME/.cargo/bin:$PATH"

export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABI_LINKER="$_gfn_root/scripts/arm-webos-gcc"
export CC_arm_unknown_linux_gnueabi="$_gfn_root/scripts/arm-webos-gcc"
export CXX_arm_unknown_linux_gnueabi="$_gfn_root/scripts/arm-webos-g++"
export AR_arm_unknown_linux_gnueabi="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-ar"
export CFLAGS_arm_unknown_linux_gnueabi="-mcpu=cortex-a9"
export CXXFLAGS_arm_unknown_linux_gnueabi="-mcpu=cortex-a9"
export BINDGEN_EXTRA_CLANG_ARGS_arm_unknown_linux_gnueabi="--sysroot=$WEBOS_SYSROOT --target=arm-linux-gnueabi"
export CMAKE_TOOLCHAIN_FILE_arm_unknown_linux_gnueabi="$_gfn_root/scripts/webos-toolchain.cmake"
# RUSTFLAGS would replace .cargo/config.toml's target rustflags wholesale (losing the SIGILL guard).
unset RUSTFLAGS

# -Zbuild-std needs a nightly; the CI image pins one through WEBOS_RUST_TOOLCHAIN.
WEBOS_RUST_TOOLCHAIN="${WEBOS_RUST_TOOLCHAIN:-nightly}"
export WEBOS_RUST_TOOLCHAIN
# Usage: cargo "+$WEBOS_RUST_TOOLCHAIN" build "${WEBOS_CARGO_ARGS[@]}" ...
WEBOS_CARGO_ARGS=(-Zbuild-std=std,panic_unwind --target arm-unknown-linux-gnueabi)
unset _gfn_root
