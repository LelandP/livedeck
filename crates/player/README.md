# htpc-player

Video behind the `Player` trait. `MpvPlayer` (feature `mpv`) renders libmpv into an OpenGL
texture that Slint shows. `FakePlayer` and `UnavailablePlayer` need no libmpv.

Without the `mpv` feature the crate, and `cargo build/clippy/test --workspace`, never need libmpv.

## Build with video on Windows (dev box)

1. Get the prebuilt libmpv dev package (`mpv-dev-x86_64-<date>-git-<hash>.7z`) from
   https://github.com/shinchiro/mpv-winbuild-cmake/releases and extract it **outside the repo**,
   e.g. `%LOCALAPPDATA%\htpc-twitch-dev\mpv`. Windows' `tar -xf` can extract `.7z`.
2. Create the MSVC import library `mpv.lib` from the DLL (the package ships only a MinGW `.dll.a`).
   Run this from a shell where the MSVC tools (`dumpbin`, `lib`) are on `PATH`, for example a
   "Developer PowerShell for VS":

   ```powershell
   $d = "$env:LOCALAPPDATA\htpc-twitch-dev\mpv"
   $names = dumpbin /exports "$d\libmpv-2.dll" | ForEach-Object { if ($_ -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $Matches[1] } }
   Set-Content "$d\mpv.def" (@('LIBRARY libmpv-2', 'EXPORTS') + $names) -Encoding ascii
   lib /nologo /def:"$d\mpv.def" /machine:x64 /out:"$d\mpv.lib"
   ```

3. Install Streamlink (it resolves the playlist). This is for development builds only: a release package ships Streamlink next to the app (`streamlink/bin/`), and the app finds it there. For example, into a venv outside the repo:
   `python -m venv %LOCALAPPDATA%\htpc-twitch-dev\streamlink-venv` then `pip install streamlink`.
4. Build and run:

   ```powershell
   $env:MPV_LIB_DIR = "$env:LOCALAPPDATA\htpc-twitch-dev\mpv"        # link time: mpv.lib
   $env:PATH = "$env:MPV_LIB_DIR;$env:PATH"                           # run time: libmpv-2.dll
   $env:HTPC_STREAMLINK = "$env:LOCALAPPDATA\htpc-twitch-dev\streamlink-venv\Scripts\streamlink.exe"
   $env:HTPC_MPV_LOG = "$env:TEMP\htpc-mpv.log"                       # optional mpv log file
   cargo run -p livedeck --features mpv
   ```

## Linux / SteamOS

Install libmpv 2.x dev files (`libmpv-dev` / `mpv-libs`) and Streamlink, then
`cargo run -p livedeck --features mpv`. Not tested on Linux or SteamOS yet outside the Flatpak build (`packaging/flatpak`).
