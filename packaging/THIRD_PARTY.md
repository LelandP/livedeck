# Third-party software in release packages

LiveDeck itself is GPL-3.0-only (see `LICENSE`). Release packages also contain:

| Software | Used for | Licence | Source |
|---|---|---|---|
| mpv / libmpv (Windows: shinchiro builds, which include FFmpeg and libplacebo) | Video playback | GPL (these builds include GPL components; mpv itself is GPL-2.0-or-later, libmpv LGPL-2.1-or-later). Check the build's own licence files before redistributing a changed package | <https://mpv.io>, <https://github.com/shinchiro/mpv-winbuild-cmake> |
| Streamlink (Windows: the portable build from streamlink/windows-builds, which includes Python) | Finds the playlist of a live stream | BSD-2-Clause (Python: PSF) | <https://streamlink.github.io> |
| Atkinson Hyperlegible | Interface font | SIL OFL 1.1 (`assets/fonts/OFL.txt`) | <https://github.com/googlefonts/atkinson-hyperlegible> |
| whisper.cpp and ggml (built into `livedeck` through the `whisper-rs` crates; the Flatpak builds it with Vulkan, the Windows package for the CPU) | Generated captions: speech to text on the device | MIT (Copyright The ggml authors). `whisper-rs` and `whisper-rs-sys`, the Rust bindings, are Unlicense | <https://github.com/ggml-org/whisper.cpp>, <https://codeberg.org/tazz4843/whisper-rs> |
| Silero VAD, ggml conversion `ggml-silero-v5.1.2.bin` (**downloaded by the app**, not in the package) | Finds speech, so silence and music never reach the captions model | MIT (Silero Team, snakers4/silero-vad; the ggml conversion by ggml-org carries the same licence) | <https://github.com/snakers4/silero-vad>, <https://huggingface.co/ggml-org/whisper-vad> |
| OpenAI Whisper model weights in ggml format: `ggml-tiny.en.bin` (Fast), `ggml-base.en-q5_1.bin` (Standard), `ggml-small-q5_1.bin` (Best). **Downloaded by the app** the first time captions are turned on, into its data folder, and checked against a SHA-256. They are not in the package | The speech model of the captions | MIT (Copyright OpenAI). The ggml conversions and quantised versions are from `ggerganov/whisper.cpp` on Hugging Face (also MIT), pinned to one revision | <https://github.com/openai/whisper>, <https://huggingface.co/ggerganov/whisper.cpp> |
| Slint and the Rust crates in `Cargo.lock` | Interface and everything else | GPL-3.0 (Slint), MIT / Apache-2.0 and similar (checked by `cargo deny`, see `deny.toml`) | <https://slint.dev>, <https://crates.io> |
| libass (Flatpak) | Subtitle rendering for mpv | ISC | <https://github.com/libass/libass> |
| libplacebo (Flatpak) | Video rendering for mpv | LGPL-2.1-or-later | <https://code.videolan.org/videolan/libplacebo> |
| FFmpeg 9.0.2 (Flatpak, no GPL parts) | Video decoding for mpv | LGPL-2.1-or-later | <https://ffmpeg.org> |
| libXpresent (Flatpak) | X11 presentation for mpv | MIT | <https://gitlab.freedesktop.org/xorg/lib/libxpresent> |
| mpv 0.41.0 (Flatpak) | Video playback | GPL-2.0-or-later. The manifest does not set `-Dgpl=false`, so this build is GPL-2.0-or-later (LGPL-2.1-or-later only with that option) | <https://mpv.io> |

The captions' speech models are not redistributed in LiveDeck's packages. The app fetches them from Hugging Face on the user's request, so their MIT licences (copyright notices included) are those of the sources linked above. whisper.cpp's licence text is `whisper.cpp/LICENSE` in the `whisper-rs-sys` crate source.

Twitch and Steam are trademarks of their owners. LiveDeck is not affiliated with them.

## whisper.cpp and ggml

```
MIT License

Copyright (c) 2023-2024 The ggml authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
