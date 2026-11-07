# LiveDeck: an unofficial Twitch client for Steam Deck, SteamOS and Windows, made for a TV and a controller

LiveDeck is a free, open-source Twitch app you drive entirely with a gamepad: browse live streams and categories, watch with chat, and sign in to see who you follow and write in chat. It is built for the couch and for SteamOS Game Mode on the **Steam Deck**, and it also runs on **Windows** (and Linux) with a controller or keyboard. Written in Rust with [Slint](https://slint.dev), [libmpv](https://mpv.io) and [Streamlink](https://streamlink.github.io).

> **This is not an official Twitch or Valve product, and it is not affiliated with or endorsed by either.** Twitch and Steam are trademarks of their owners.
>
> **Use it at your own risk.** It finds streams through unofficial, undocumented Twitch endpoints. They can break without notice, and using them may conflict with the [Twitch Terms of Service](https://www.twitch.tv/p/legal/terms-of-service/). The software comes with no warranty (see [LICENSE](LICENSE)). You are responsible for how you use it.
>
> **Why it exists:** I made this for myself, because I wanted to watch Twitch on my TV from the couch without a mouse and keyboard, and the existing options did not fit. I am sharing it in case it is useful to someone else. It is a hobby project: expect rough edges, and things get fixed when I have time. Contributions are very welcome (see [Contributing](#contributing)).

**What works**

- Home with a featured stream, Top Live and Categories; a Browse screen for all categories; a screen per category
- Watching live streams (video through libmpv, hardware decoding where available)
- A player menu (Start while watching) for quality, hardware decoding, latency and zoom, plus a stats box with resolution, codec, bitrate, buffer and dropped frames
- Audio only, per stream from the player menu: plays just the sound to save bandwidth and battery, on a black screen with a slowly drifting watermark
- A sleep timer in the player menu (15 to 120 minutes): asks "Still watching?" a minute before the end, then fades out, stops the stream and puts the device to sleep
- Subscribe from the player menu: a QR code of the channel's subscribe page to finish on your phone, or (outside Game Mode) open it in a browser
- Chat with coloured names, badges and emotes, as a side panel or an overlay on the video
- Write in chat (signed in) with an on-screen keyboard (letters, punctuation, common emotes) or one-press quick messages, while the stream keeps playing
- Sign in with a code and a QR code (no password typed on the TV), then a Followed row and a Following tab
- Search for channels and categories by name with an on-screen keyboard (signed in)
- Live notifications: a silent popup when a channel you follow goes live, on any screen (signed in, can be turned off)
- Follow raids: when the channel you watch raids another one, a card counts down 10 seconds and takes you there (A goes now, B stays). Signed in
- Hype trains: while one runs in the channel you watch, a bar at the top of the video shows its level, progress and time left. Signed in or not, through the same unofficial API the website uses
- Settings for chat layout, video quality and live notifications, saved between runs
- Everything controllable with a gamepad (D-pad, A/B/X/Y, Start, LB/RB)

**What does not**

- No VODs or clips, no 7TV/BTTV/FFZ emotes yet
- Signed out, each row shows only its first page (about 30 items), because Twitch's anonymous API allows no more. Sign in for full paging
- It does not block or skip ads
- Linux/Steam Deck builds are new and **not yet tested on a real Deck**

## To do

Ideas and plans, from easiest to hardest. Nothing here exists yet.

**Big project**

- [ ] **Kick support:** watch Kick streams next to Twitch. The app is Twitch-shaped throughout (API trait, feeds, chat, sign-in, config), so this starts with a platform abstraction. Then a read-only Kick platform (browse, play through Streamlink, chat over Kick's own WebSocket), with login and follows later

**Parked**

- **Predictions and polls:** viewers can't take part from LiveDeck. Betting channel points and voting only work on the Twitch website or app, and polls can't even be read without a twitch.tv login. Showing a prediction you can't join wasn't worth it

## Install

Download the latest build from the **Releases** page of this repository.

### Windows

1. Download `LiveDeck-vX.Y.Z-windows-x64.zip` and unpack it anywhere.
2. Run `livedeck.exe`. That is all: video (libmpv) and Streamlink are in the folder, nothing else needs installing.

The program is not code-signed, so Windows SmartScreen may warn about an unknown publisher. Choose *More info*, then *Run anyway*. To remove it, delete the folder (settings stay in `%APPDATA%\livedeck`).

### Steam Deck / Linux (Flatpak, experimental)

```bash
flatpak install --user LiveDeck-vX.Y.Z-linux-x86_64.flatpak
flatpak run io.github.livedeck.LiveDeck
```

To use it in Game Mode, switch to Desktop Mode, then in Steam choose *Add a Non-Steam Game* and pick **LiveDeck**. Leave the controller layout on the default **Gamepad** template (see [packaging/steam-input](packaging/steam-input/README.md)). The Flatpak is experimental: if it does not build or run for you, please open an issue with the output.

### Build from source

Needs [Rust](https://rustup.rs) (stable). Without video:

```powershell
cargo run -p livedeck --release
```

With video you also need libmpv and Streamlink:

- **Windows:** download the `mpv-dev-x86_64-*` package from [shinchiro/mpv-winbuild-cmake releases](https://github.com/shinchiro/mpv-winbuild-cmake/releases), unpack it, and run `packaging/windows/make-import-lib.ps1 -Dir <folder>` from a Developer PowerShell (it creates the `mpv.lib` that MSVC needs). Install Streamlink (`pip install streamlink`, or point `HTPC_STREAMLINK` at a copy). Then:

  ```powershell
  $env:MPV_LIB_DIR = "<folder with libmpv-2.dll and mpv.lib>"
  $env:PATH = "$env:MPV_LIB_DIR;$env:PATH"
  cargo run -p livedeck --release --features mpv
  ```

- **Linux:** install libmpv 2.x development files (`libmpv-dev` / `mpv-libs`), `libudev-dev`, `libfontconfig1-dev`, `libxkbcommon-dev` and Streamlink, then `cargo run -p livedeck --release --features mpv`.

To make the portable Windows zip yourself, see `packaging/windows/package.ps1`.

## Use

| Input | Action |
|---|---|
| D-pad / left stick / arrow keys | Move focus |
| A / Enter | Select, watch, change a setting |
| B / Esc / Backspace | Back (on Search: delete a character first) |
| X / `r` | Retry a failed load (while watching with chat shown: write in chat) |
| Y / `y` | Show or hide chat |
| Start / Menu key | Open or close the left menu (while watching: the player menu) |
| LB / RB / Page Up / Page Down | Change the page of the chat keyboard |

- **Sign in:** open the left menu, choose *Sign in*, and scan the QR code (or go to the address shown and type the code) on your phone. Signing out is on the Settings screen.
- **Search:** in the left menu (sign in first). Type with the on-screen keyboard: the D-pad moves, A types, X deletes, Y adds a space, Start jumps to the results. Results update as you type, from two characters on: live channels, categories, then offline channels. Up from the first row (or B) goes back to the keyboard. On a PC you can also just type.
- **Write in chat:** while signed in, press X with chat shown (or choose *Write in chat* in the player menu). A keyboard opens under the video (side panel) or over its bottom (overlay), and the stream keeps playing. The D-pad moves and A types; LB/RB switch between letters, digits and punctuation, and common emotes (typed as their codes, which Twitch turns into emotes). Shift capitalises the next letter, pressed twice it locks. X deletes, Y adds a space, Start sends, B closes the keyboard (the text waits until you send it or change channel). Up from the top row reaches the quick messages: A sends one at once. Slow mode, followers-only and subscribers-only show above the box, and if Twitch refuses a message the reason shows there and the text stays. On a PC, or with Steam's keyboard (STEAM + X), you can also just type. Messages go through Twitch's official API with the `user:write:chat` permission. If you signed in before this feature, LiveDeck asks you to sign in once more. The quick messages are `quick_messages` in `config.toml`. LiveDeck keeps to Twitch's limit of 20 messages in 30 seconds.
- **Settings:** chat layout (side panel or over the video), video quality (applies to the next stream you open) and live notifications.
- **Live notifications:** while signed in, LiveDeck checks every 30 seconds which channels you follow are live, and shows a popup at the top right for each one that just went live (not the one you are watching, and not after a quick stream restart). Popups stay 8 seconds and take no buttons. Turning them off in Settings stops the checks.
- **Follow raids:** while signed in, LiveDeck listens through Twitch EventSub (a WebSocket, one subscription for the channel on screen) for that channel raiding another one. A card shows who is raiding whom and counts down 10 seconds; then, or when you press A, the player switches to the raided channel with its chat, keeping your player-menu choices, the sleep timer and audio only. B keeps you where you are. Back from the new channel returns to the browse screens.
- **Player menu:** press Start while watching. Up/Down pick a row, Left/Right change it, B closes it. Quality and latency reload the stream; hardware decoding and zoom (*Fill* crops the picture to fill a 16:10 screen) apply at once. *Stats* shows a box with live numbers and stays on from stream to stream. *Sleep timer* counts down whatever you watch (switching channels does not reset it); a minute before the end any button keeps going for the same length again, otherwise the sound fades, the stream stops and the device suspends. Nothing in this menu is saved: it lasts until the app quits, and every run starts from the Settings screen's values.
- **Config file:** `config.toml` is created on first run in `%APPDATA%\livedeck\config` (Windows) or `~/.config/livedeck` (Linux). It holds `chat_layout`, `quality`, `live_notifications`, `background_play`, `image_cache_max_mb`, `quick_messages` and an optional `client_id`.
- **Try it offline:** `livedeck --mock` runs with fixture data and no network.
- **Logs:** set `RUST_LOG=livedeck=debug`. `livedeck --help` lists every option and environment variable.

### Using your own Twitch application (forks)

Signing in needs a Twitch application's client ID. This project's public ID is built in (`DEFAULT_CLIENT_ID` in `crates/app/src/main.rs`); a client ID is public, not a secret, and the app has no client secret. If you fork or repackage the app, register your own at <https://dev.twitch.tv/console> (Client Type **Public**, any OAuth redirect URL such as `http://localhost:3000`, the Device Code flow never uses it), then set `TWITCH_CLIENT_ID`, or `client_id` in `config.toml`, or replace the constant. Never commit tokens.

## Contributing

Bug reports, testing on real hardware (especially a Steam Deck), fixes and features are all welcome. Please open an issue first for anything large so we do not duplicate work.

**Good places to start**

- Test the Flatpak on a Steam Deck / under Gamescope and report what breaks or fix it (`packaging/flatpak`)
- Resume from sleep, 4K and ultrawide behaviour
- 7TV / BTTV / FFZ emotes, VODs and clips
- Translations, accessibility, controller glyphs for other pads

**Setup**

```powershell
git clone <this repository>
cd livedeck
cargo run -p livedeck -- --mock      # no account, no network needed
```

**Before you open a pull request** run all of these; CI runs the same:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check        # optional locally, needs cargo-deny (see deny.toml)
```

**How the code is organised**

| Crate | Role |
|---|---|
| `crates/core` (`htpc-core`) | Pure state: `AppState`, `Intent`, `Event`, `Command` and the `reduce` function. No UI, network or IO, fully unit-tested |
| `crates/ui` (`htpc-ui`) | The Slint files (a pure view) and the view-model that turns `AppState` into Slint models |
| `crates/input` | Keyboard and gamepad (`gilrs`) to `Intent` |
| `crates/twitch` (`htpc-twitch-api`) | Helix client (including sending chat), anonymous GQL client, device-code login |
| `crates/chat` | Anonymous IRC-over-WebSocket chat for reading, emotes, badges, room modes |
| `crates/resolver` | Stream URL lookup through Streamlink |
| `crates/player` | libmpv rendering into the Slint window (feature `mpv`) |
| `crates/storage` | Config, token storage (keyring or private file), image cache |
| `crates/app` | The `livedeck` binary: wiring, store loop, command execution |

Data flows one way: input becomes an `Intent`, `reduce` returns the new state and a list of `Command`s, the app runs the commands off the UI thread and feeds results back as `Event`s. Navigation logic belongs in `reduce`, not in the `.slint` files. Network, chat, player and resolver sit behind traits and have fakes, so tests need no network. Colours, sizes and durations live in `crates/ui/ui/theme.slint`. Platform-specific code goes behind `#[cfg]`, as development happens on Windows and the target is Linux.

By contributing you agree your work is released under the project's licence (GPL-3.0-only).

## Frequently asked questions

**Is this the official Twitch app for Steam Deck?** No. It is an independent hobby project.

**Is it safe, and will my Twitch account be banned?** I do not know, and cannot promise. The app reads public information and, if you sign in, uses Twitch's official OAuth Device Code flow with permission to see your follows and to write chat messages as you (only when you send one). Stream lookup uses the same unofficial approach as Streamlink. Use it at your own risk.

**Does it block ads?** No, and it does not try to.

**Why Streamlink and libmpv?** Twitch's official API does not give video URLs. Streamlink finds the stream, libmpv plays it. Both are bundled in the releases. Stream lookup sits behind a `StreamResolver` trait so it can be replaced.

**Why does signed-out browsing show so few streams?** The anonymous web API only returns a first page. Signing in switches to the official API, which pages properly.

## Licence

GPL-3.0-only, see [LICENSE](LICENSE). Release packages bundle other software under its own licences, listed in [packaging/THIRD_PARTY.md](packaging/THIRD_PARTY.md). The interface font is Atkinson Hyperlegible (SIL OFL 1.1).

<!-- Keywords: Twitch client, Twitch for Steam Deck, SteamOS Game Mode Twitch app, controller Twitch app, gamepad Twitch, couch Twitch, HTPC Twitch, unofficial Twitch client, Rust, Slint, mpv, Streamlink, open source -->
