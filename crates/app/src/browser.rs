//! Opening web pages in the system browser (the subscribe card's "Open in browser").
//!
//! - Windows: `ShellExecute`, through the `open` crate.
//! - Linux: `xdg-open`, which in a Flatpak goes through the OpenURI portal and needs no extra
//!   permission.
//! - Game Mode on the Deck: not offered at all. A browser opens behind gamescope or not at all,
//!   so the card only shows its QR code there.

/// True when a browser opened now could be seen: false in gamescope (Steam's Game Mode). Checked
/// once at startup.
pub fn can_open_browser() -> bool {
    !in_gamescope(|name| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()))
}

/// Gamescope sets `XDG_CURRENT_DESKTOP=gamescope` and `GAMESCOPE_WAYLAND_DISPLAY` for what runs
/// inside it. `var` looks a variable up, so tests need not touch the real environment.
fn in_gamescope(var: impl Fn(&str) -> Option<String>) -> bool {
    let desktop = var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    // A colon-separated list, like `ubuntu:GNOME`.
    let gamescope_desktop = desktop
        .split(':')
        .any(|d| d.trim().eq_ignore_ascii_case("gamescope"));
    let gamescope_display = var("GAMESCOPE_WAYLAND_DISPLAY").is_some_and(|v| !v.trim().is_empty());
    gamescope_desktop || gamescope_display
}

/// Opens `url` in the default browser without waiting for it. Failures are only logged.
pub fn open_url(url: &str) {
    match open::that_detached(url) {
        Ok(()) => tracing::info!(url, "opened in the browser"),
        Err(err) => tracing::warn!(url, %err, "could not open the browser"),
    }
}

#[cfg(test)]
mod tests {
    use super::in_gamescope;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn game_mode_is_found_by_its_desktop_name() {
        assert!(in_gamescope(env(&[("XDG_CURRENT_DESKTOP", "gamescope")])));
        assert!(in_gamescope(env(&[(
            "XDG_CURRENT_DESKTOP",
            "KDE:Gamescope"
        )])));
    }

    #[test]
    fn game_mode_is_found_by_its_wayland_display() {
        assert!(in_gamescope(env(&[(
            "GAMESCOPE_WAYLAND_DISPLAY",
            "gamescope-0"
        )])));
    }

    #[test]
    fn desktops_and_windows_are_not_game_mode() {
        assert!(!in_gamescope(env(&[])));
        assert!(!in_gamescope(env(&[("XDG_CURRENT_DESKTOP", "KDE")])));
        assert!(!in_gamescope(env(&[("GAMESCOPE_WAYLAND_DISPLAY", " ")])));
    }
}
