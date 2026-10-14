# Native seam (GPL-3.0-or-later)

`starfish.c`, `starfish.h` and `compat/getauxval.c` are copied unchanged from
[PlxNative](https://github.com/GLinnik21/plx-native) at commit `af036aa9`
(Copyright (c) 2026 Gleb Linnik and contributors, GPL-3.0-or-later).
Keep them byte-identical to upstream where possible so fixes can be pulled across.

- `starfish.c`: LG StarfishMediaAPIs (libplayerAPIs) + ACB / SDL exported-window video-plane binding.
- `compat/getauxval.c`: `getauxval` for the glibc 2.12 baseline; Rust std's executable startup needs it.
- `compat/getentropy.c` (ours): `getentropy` for glibc 2.12; aws-lc seeds from it.
- `elog.c`: the `elogf` event log that `starfish.c` writes to.
