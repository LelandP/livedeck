# Third-party software in release packages

LiveDeck itself is GPL-3.0-only (see `LICENSE`). Release packages also contain:

| Software | Used for | Licence | Source |
|---|---|---|---|
| mpv / libmpv (Windows: shinchiro builds, which include FFmpeg and libplacebo) | Video playback | GPL (these builds include GPL components; mpv itself is GPL-2.0-or-later, libmpv LGPL-2.1-or-later). Check the build's own licence files before redistributing a changed package | <https://mpv.io>, <https://github.com/shinchiro/mpv-winbuild-cmake> |
| Streamlink (Windows: the portable build from streamlink/windows-builds, which includes Python) | Finds the playlist of a live stream | BSD-2-Clause (Python: PSF) | <https://streamlink.github.io> |
| Atkinson Hyperlegible | Interface font | SIL OFL 1.1 (`OFL-AtkinsonHyperlegible.txt` / `assets/fonts/OFL.txt`) | <https://github.com/googlefonts/atkinson-hyperlegible> |
| Slint and the Rust crates in `Cargo.lock` | Interface and everything else | GPL-3.0 (Slint), MIT / Apache-2.0 and similar (checked by `cargo deny`, see `deny.toml`) | <https://slint.dev>, <https://crates.io> |

Twitch and Steam are trademarks of their owners. LiveDeck is not affiliated with them.
