# Steam Input

LiveDeck needs no custom Steam Input configuration. It reads the controller itself (`gilrs`, an
XInput-style pad) and also accepts the keyboard keys Steam Input can send.

## Adding it to Steam (Windows, or Linux desktop)

1. Steam, **Games**, **Add a Non-Steam Game to My Library**, browse to `livedeck.exe` (Windows
   package) or pick **LiveDeck** from the list (Flatpak install).
2. Open its **Controller** settings and choose the **Gamepad** template (the default). Do not pick
   a "keyboard and mouse" template.
3. Launch it from Game Mode.

## Buttons

| Button | Action |
|---|---|
| D-pad / left stick | Move focus |
| A | Select / watch / change a setting |
| B | Back. On a main screen (Home, Following, Browse, Settings) it asks whether to quit: A quits, B stays |
| X | Retry a failed load |
| Y | Show or hide chat |
| Start (Menu) | Open or close the left rail |

If you prefer a Steam Input layout that sends keys, these are the keys the app understands:
arrows, Enter (A), Esc or Backspace (B), `r` (X), `y` (Y) and the Menu key (Start). The list is
in `crates/input/src/keys.rs`.

## Duplicate controllers

With Steam running, the app can see the Deck's built-in controller and also Steam's virtual pad
that replaces it. Steam lists the replaced devices in `SDL_GAMECONTROLLER_IGNORE_DEVICES`, and the
app skips them as SDL games do, as long as another controller is connected. As a fallback, the same
button arriving from two controllers within 100 ms counts as one press. See
`crates/input/src/gamepad.rs`.

## Why no template file

A template (`.vdf`) would only repeat the Gamepad template. Steam also rewrites those files between
versions, so shipping one adds something to maintain and gives nothing.
