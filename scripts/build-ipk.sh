#!/usr/bin/env bash
# Builds the TV binaries (gfn-app, gfn-probe, opennow-core) and packages build/<id>_<version>_arm.ipk.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=webos-env.sh
source "$root/scripts/webos-env.sh"

cd "$root"
cargo "+$WEBOS_RUST_TOOLCHAIN" build "${WEBOS_CARGO_ARGS[@]}" --release -p gfn-app -p gfn-probe

# opennow-core is its own package in the OpenNOW submodule. Its TV flags are given here rather
# than inherited from this repo's .cargo/config.toml, which also exports a Starfish hook symbol
# that only gfn-app defines (see that file for why each flag is needed).
core_target="$root/target/opennow-core"
(
    cd "$root/vendor/OpenNOW/native/opennow-core"
    CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABI_RUSTFLAGS="-C target-cpu=cortex-a9 -C target-feature=-neon -C force-frame-pointers=yes --cfg getrandom_backend=\"linux_raw\"" \
        cargo "+$WEBOS_RUST_TOOLCHAIN" build "${WEBOS_CARGO_ARGS[@]}" --release --bin opennow-core --target-dir "$core_target"
)

out="$root/target/arm-unknown-linux-gnueabi/release"
core="$core_target/arm-unknown-linux-gnueabi/release/opennow-core"

# ARMv6 CP15 barriers (mcr p15, 0, rX, c7, c10, 4|5) SIGILL on the TV; refuse to package any.
for binary in "$out/gfn-app" "$out/gfn-probe" "$core"; do
    barriers="$("$WEBOS_SDK/bin/arm-webos-linux-gnueabi-objdump" -d "$binary" \
        | grep -cE 'mcr[[:space:]]+p?15, 0, r[0-9]+, c?r?7, c?r?10, \{?[45]\}?' || true)"
    if [[ "$barriers" != 0 ]]; then
        echo "$binary: $barriers CP15 barrier instructions (SIGILL on the TV); not packaging" >&2
        exit 1
    fi
done

python3 "$root/scripts/mkipk.py" "$out/gfn-app" "$root/build" "$out/gfn-probe" "$core"
