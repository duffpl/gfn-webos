# Third-party notices

## PlxNative — GPL-3.0-or-later

`native/starfish.c`, `native/starfish.h` and `native/compat/getauxval.c` are copied from
[PlxNative](https://github.com/GLinnik21/plx-native) at commit `af036aa9`.
Copyright (c) 2026 Gleb Linnik and contributors. See `native/README.md`.

Facts about LG's firmware that this app relies on (the Starfish load payload, callback numbering,
the layout of LG's SDL key events, remote key codes) were learned from PlxNative's code and
documentation, as credited in the source comments.

## OpenNOW — MIT

`vendor/OpenNOW` is a submodule pointing at a fork of
[OpenNOW](https://github.com/OpenCloudGaming/OpenNOW) with webOS support added
(branch `webos`). Copyright (c) 2025 Zortos; licensed under the MIT licence in
`vendor/OpenNOW/LICENSE`. Its own third-party notices are in `vendor/OpenNOW/THIRD_PARTY_NOTICES`.

The Opus decoder is built from the sources OpenNOW vendors
(`vendor/OpenNOW/native/opennow-streamer/vendor/audiopus-sys`): audiopus-sys under the licence in its
`LICENSE.md`, the Opus codec under its BSD licence in `opus/COPYING`.

## moonlight-tv (GPL-3.0) / ss4s (LGPL-3.0)

The NDL video path (`crates/gfn-sink/src/ndl.rs`) follows the call sequence of
[ss4s](https://github.com/mariotaku/ss4s)' `ndl-webos4` module, used by
[moonlight-tv](https://github.com/mariotaku/moonlight-tv). The Starfish payload's `WEBRTC`
content type and arrival-time PTS also come from ss4s.

## Inter — SIL Open Font License 1.1

`assets/fonts/Inter-*.ttf`. See `assets/fonts/OFL.txt`.

## Icons — Streamline

`assets/icons/cog.svg` is "Cog" from Streamline's free Core set, licensed CC BY 4.0
([streamlinehq.com](https://streamlinehq.com)). `assets/icons/controller.svg` is "Game Controller"
from Streamline's Ionic set, which redistributes [Ionicons](https://ionic.io/ionicons) (MIT).
