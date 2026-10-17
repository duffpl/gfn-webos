# gfn-webos

An unofficial, native GeForce NOW client for LG webOS TVs. Not affiliated with NVIDIA or LG.

It runs as a native app on the TV: no browser and no Chromium. It plays the stream through
the TV's hardware decoder, and has a UI made for the remote and game controllers.

## What it is built on

- **[OpenNOW](https://github.com/OpenCloudGaming/OpenNOW)** (MIT), through our fork
  [duffpl/OpenNOW](https://github.com/duffpl/OpenNOW), branch `webos`, vendored as the
  `vendor/OpenNOW` submodule. From it we use:
  - `opennow-core`: account sign-in, the library, sessions, the queue and settings, spoken to over
    OpenNOW's JSON protocol;
  - the NVST streaming engine (`opennow-streamer`): transport, audio and input.

  The fork adds:
  - an embeddable webOS media platform for the streaming engine;
  - a webOS build of the core. It saves credentials to a file, because the TV has no OS keyring;
  - an option to report a desktop platform. Without it, the cloud PC never turns HDR on;
  - a UDP receive thread for webOS only;
  - extra library fields: RTX, Reflex and HDR per store version, description, age rating and
    release date.
- **[PlxNative](https://github.com/GLinnik21/plx-native)** (GPL-3.0-or-later), a native Plex
  client for webOS. From it we use:
  - the C bridge to the TV's Starfish media player (`native/starfish.c`, `native/starfish.h`);
  - the `getauxval` shim for the TV's old glibc (`native/compat/getauxval.c`);
  - the device-tested Starfish load payload, which `gfn-starfish` builds from;
  - its notes on the remote's colour-key codes and on how webOS allocates decoder resources.
- **[moonlight-tv](https://github.com/mariotaku/moonlight-tv)** and its media layer
  [ss4s](https://github.com/mariotaku/ss4s). Its code isn't vendored, but we follow it closely:
  - the Starfish settings that bring latency down: the `WEBRTC` contents type, timestamps from
    when frames arrive, and a 1 MB source buffer;
  - the optional NDL video path, which follows the call sequence of ss4s' `ndl-webos4` module.

This repository adds the TV side:
- the UI and on-screen drawing;
- input from the remote, Magic Remote, keyboard and Bluetooth controllers;
- Bluetooth pairing through the TV's system services;
- video output through Starfish or NDL;
- the desktop simulator.

## Features

**Account**
- Sign-in with a code or QR code on your phone (device sign-in); the session is kept between runs.
- Sign-out, from the user's name in the library header.

**Library**
- The games in your GeForce NOW library, with box art.
- Search, genre filter, and sort by recently played or A–Z. The sort order is remembered.
- HDR tags on tiles for games that stream in HDR.
- Game detail screen:
  - description, age rating, release date, last played, publisher and developer;
  - RTX, Reflex and HDR badges;
  - which store version to launch (Steam, Epic and so on), when you own more than one;
  - screenshots, with a full-screen viewer.
- Green on the remote starts the selected game from the grid.

**Streaming**
- Queue position and session start, with resume or replace when a session is already running.
- Hardware decode through Starfish (H.264, H.265, AV1, HDR10). NDL is optional: it plays
  H.264 only, and the app falls back to Starfish if it won't open.
- Settings:
  - resolution 720p to 4K;
  - 30, 60 or 120 fps;
  - bitrate 10 to 75 Mbps;
  - codec, HDR and video player.

  They are applied when the next game starts.
- A stats overlay: profile, server, fps, bitrate, ping, jitter, packet loss, decode time and
  display lag.
- The stream menu: stats, the keyboard, and ending the session.
- Recovery when another app takes the screen: the decoder is reopened when you return.

**Updates**
- The version, commit and its date at the bottom of Settings.
- A check for newer releases on GitHub at start and twice a day. When there is one, an arrow
  leads the library header; it opens the release notes and a QR code to the release page.
  The app doesn't install anything itself.

**Input**
- Bluetooth controller pairing (Controllers screen). Paired pads are read directly from
  `/dev/input`.
- Magic Remote as a mouse, scaled to the stream's resolution. The pointer hides when idle.
- Colour keys:
  - Red: stats overlay;
  - Green: LG's on-screen keyboard while streaming, or play in the library;
  - Blue: right-click.
- The TV stays awake while a controller is in use, also outside games.

## Tested on a TV

All testing so far is on one TV:

| | |
| --- | --- |
| Model | LG OLED55C9PLA (2019, European DVB board `O18_DVB_EU`) |
| webOS | 4.5 (SDK version 4.10.0) |
| Firmware | 05.50.00 |
| Install | Developer Mode |
| Account | Performance membership |

These work on it:
- Sign-in, the library list and starting a game, including the queue.
- Streaming with sound, over Ethernet:
  - H.264 and H.265, up to 1440p at 75 Mbps;
  - display lag about 55–70 ms with Starfish in SDR.
- HDR10 with H.265, at about 78 ms display lag.
- The NDL video player with H.264. It feels about as responsive as Starfish.
  - Its stats show about 11 ms display lag. That number isn't comparable with Starfish's: NDL
    reports a different point in the pipeline, so it isn't evidence that NDL is faster.
- Bluetooth pairing and play with a DualSense controller.
- The Magic Remote as a mouse at stream resolutions above 1080p.
- The colour keys, the Back key and the stats overlay.
- LG's on-screen keyboard: it opens, typing reaches the game, and closing it is detected.
- Leaving with Home and coming back: the picture returns.

Known problem: over Wi-Fi the stream stutters about every half-second. The network pauses
(gaps of about 235 ms between packets, measured at the socket). Over Ethernet it is smooth.
- The TV also limits an app's UDP receive buffer to 512 KiB, so the default bitrate is 20 Mbps.

## Not tested yet

**Built after the last TV session, tested only in the desktop simulator**
- Library search, genre filter, sort and HDR tags.
- The game detail screen, screenshot viewer and store-version choice.
- Artwork loading only what is on screen during fast scrolling.
- Clicking the arrows and values on the Settings screen (a click bug was fixed).
- Hiding the idle Magic Remote pointer.
- Keeping the TV awake while a controller is used in the menus.
- Sign-out, and the new header icons.
- The release check, the update arrow and the release notes window.
- AV1. The C9 can't decode it, so it needs a 2020 or newer LG TV. The app doesn't check which
  codecs the TV supports: if a stream won't start, choose another codec.

**Never tried**
- Reopening LG's on-screen keyboard after the TV has closed it.
- 4K and 120 fps streams.
- Controllers other than the DualSense.
- Other TV models and webOS versions. Newer webOS releases change how Starfish allocates
  decoders, and webOS 4.5 is the only version tried.
- Free-tier accounts (ads in the queue).
- Long sessions, and networks other than the home one.

## Missing compared to other OpenNOW clients

The OpenNOW core can do these, but this app doesn't offer them yet:

- **Browsing:** the store catalog, favourites and collections. Only your own library is shown.
- **Linked stores:** connecting and syncing Steam, Epic or Ubisoft accounts.
- **Accounts:**
  - more than one saved account, and switching between them;
  - sign-in providers other than NVIDIA's (partner regions);
  - the parental PIN.
- **Servers:** picking a server region, or pinging regions.
- **Capture:** screenshots and recording the stream.
- **Microphone and voice chat.**
- **Your account:** subscription details and play-time information.
- **Support:** exporting diagnostics, sending feedback, Discord presence.
- **Updates:** the app says when a new release is out but doesn't install it; new versions are
  installed by hand.
- **Languages:** English only. OpenNOW is translated through Crowdin.

## Getting the app

There is no release download, and the app isn't in the
[webOS Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel) repository at the
moment. It may be proposed there once it has been tested on more TVs. Until then you build the
`.ipk` yourself and install it in Developer Mode, as below.

## Building

macOS and Linux both work as build hosts. You need:

- **Rust** through rustup, with a nightly toolchain and its standard-library source. The TV build
  compiles std itself (`-Zbuild-std`):

  ```sh
  rustup toolchain install nightly --component rust-src
  ```

- **The webosbrew NDK**: the `arm-webos-linux-gnueabi_sdk-buildroot` toolchain from
  [openlgtv/buildroot-nc4 releases](https://github.com/openlgtv/buildroot-nc4/releases).
  - Use one with GCC 12. On Linux x86_64 that is
    [`webos-b17b4cc`](https://github.com/openlgtv/buildroot-nc4/releases/tag/webos-b17b4cc), the one
    CI uses. Releases from `webos-36b9236` on ship GCC 14 or newer, and the TLS library then fails to
    build.
  - Unpack it to `~/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot`, or set `WEBOS_SDK` to
    where you put it.
  - Run its `relocate-sdk.sh` once after unpacking.
- **Other tools:** CMake, Clang (for the TLS library's build) and Python 3.

Then clone with the OpenNOW submodule and build:

```sh
git clone --recursive https://github.com/duffpl/gfn-webos.git
cd gfn-webos
scripts/build-ipk.sh
```

This builds three programs for the TV's 32-bit ARM userland: `gfn-app`, `opennow-core` from the
submodule, and the `gfn-probe` developer tool. It packages them as
`build/io.github.gfnwebos_<version>_arm.ipk`.

The build stops if any binary contains ARMv6 barrier instructions, which crash on the TV. The
compiler wrappers in `scripts/` keep them out, so this only trips when something bypasses them.

### Desktop simulator

The simulator runs the same UI on macOS or Linux. It signs in with your real account and can
stream:
- on macOS it decodes with VideoToolbox (H.264 and H.265);
- elsewhere it uses OpenH264 (H.264 only).

It needs SDL2, found through `sdl2-config` (`brew install sdl2` on macOS).

```sh
cargo run -p gfn-app --features sim
cargo test -p gfn-app --features sim
```

`--preview <screen>` shows a screen with sample data, without signing in. For example:
`library`, `detail`, `viewer`, `settings`, `controllers`, `signin`, `queue` and `stats`.

### Building in Docker (Linux)

`docker/Dockerfile` has everything above preinstalled: the NDK, the pinned nightly Rust and the
simulator's libraries. It is the environment CI uses. It is x86_64 only, because the GCC 12 NDK
has no aarch64 build.

```sh
docker build --platform linux/amd64 -t gfn-webos-build docker
docker run --rm -v "$PWD:/src" -w /src gfn-webos-build scripts/build-ipk.sh
docker run --rm -v "$PWD:/src" -w /src gfn-webos-build cargo test --workspace --features gfn-app/sim
```

On an Apple Silicon Mac this runs under x86_64 emulation and is slow, so the Mac tools above are
the better choice there.

### Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs in that image.

- **Every push to `main` and every pull request:** it runs the app's tests and the core's tests.
- **A `vX.Y.Z` tag:** it also writes that version into `pkg/appinfo.json`, builds the `.ipk`, and
  attaches it to a **draft** GitHub release. The release stays private until it is published by
  hand.
- **The image:** it is published to the GitHub Container Registry, tagged by a hash of the
  Dockerfile. It is rebuilt only when the Dockerfile changes, for example to bump the NDK or the
  nightly.

## Installing on a TV

The app installs like any other Developer Mode app.

1. **Turn on Developer Mode.** webosbrew's [Developer Mode guide](https://www.webosbrew.org/devmode/)
   covers this in detail. In short:
   - Install LG's *Developer Mode* app from the LG Content Store.
   - Sign in with an LG developer account (free at [webostv.developer.lge.com](https://webostv.developer.lge.com)).
   - Turn on *Dev Mode Status* and restart the TV.
   - The Developer Mode session expires unless you extend it in the app, and apps installed this
     way are removed when it runs out.
2. **Get the TV's ssh key.**
   - In the Developer Mode app, turn on *Key Server*.
   - Download the key and save it as `~/.ssh/lgtv_webos`:

     ```sh
     curl -o ~/.ssh/lgtv_webos http://<tv-ip>:9991/webos_rsa
     chmod 600 ~/.ssh/lgtv_webos
     ```

   - The key is protected by the passphrase shown in the Developer Mode app. Add it to your ssh
     agent (`ssh-add ~/.ssh/lgtv_webos`), or you'll be asked for the passphrase on each connection.
3. **Install, and optionally launch:**

   ```sh
   GFN_TV=<tv-ip> scripts/tv-install.sh --launch
   ```

   The script:
   - copies the `.ipk` over the TV's developer ssh (port 9922, user `prisoner`);
   - installs it through the TV's app installer and prints the result;
   - with `--launch`, closes any running copy and starts the new one.

   `GFN_TV_KEY` points it at a key stored somewhere else.

The app shows up in the launcher as **GFN webOS (dev)**.

If you already use LG's webOS CLI, `ares-install build/io.github.gfnwebos_<version>_arm.ipk`
works too. Only the script above has been used with this app.

## Licence

GPL-3.0-or-later (see `LICENSE`).
- `native/starfish.c`, `native/starfish.h` and `native/compat/getauxval.c` come from PlxNative
  (GPL-3.0-or-later).
- OpenNOW is MIT.

See `THIRD-PARTY-NOTICES.md`.
