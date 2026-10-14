# gfn-webos

An unofficial, native GeForce NOW client for LG webOS TVs. Not affiliated with NVIDIA or LG.

It is built on [OpenNOW](https://github.com/OpenCloudGaming/OpenNOW): its Rust core
(`opennow-core`) handles the account and sessions, and its NVST streaming engine handles the
stream. This repository supplies the TV side: the UI, input (remote, keyboard, Bluetooth
controllers) and video output through the TV's hardware decoder.

Tested on an LG C9 (webOS 4.5).

## Features

- Sign-in, library, queue and session handling through `opennow-core`
- Hardware decode through Starfish (H.264, H.265, HDR10) or, optionally, NDL (H.264)
- Settings: resolution up to 4K, 30/60/120 fps, bitrate, codec, HDR, video player
- Bluetooth controller pairing; controllers are read directly from `/dev/input`
- Remote: Red toggles the stats overlay, Green opens the on-screen keyboard, Blue is right-click
- Stats overlay: profile, server, fps, bitrate, ping, jitter, loss, decode time and display lag

## Building

Clone with the OpenNOW submodule:

```sh
git clone --recursive https://github.com/duffpl/gfn-webos.git
```

TV build (needs the [webosbrew NDK](https://github.com/openlgtv/buildroot-nc4) at
`~/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot` or `$WEBOS_SDK`, and a nightly Rust toolchain):

```sh
scripts/build-ipk.sh                          # build/io.github.gfnwebos_<version>_arm.ipk
GFN_TV=<tv-ip> scripts/tv-install.sh --launch # Developer Mode TV, ssh key at ~/.ssh/lgtv_webos
```

Desktop simulator (decodes in software, shows the same UI):

```sh
cargo build -p gfn-app --features sim
cargo test -p gfn-app --features sim
```

## Licence

GPL-3.0-or-later (see `LICENSE`). `native/starfish.c`, `native/starfish.h` and
`native/compat/getauxval.c` come from [PlxNative](https://github.com/GLinnik21/plx-native)
(GPL-3.0-or-later); OpenNOW is MIT. See `THIRD-PARTY-NOTICES.md`.
