//! User settings. Persisted by `htpc-storage`, edited on the Settings screen.

use std::fmt;
use std::str::FromStr;

/// Where chat is drawn while watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatLayout {
    /// A panel on the right. The video shrinks to make room.
    #[default]
    Side,
    /// Translucent lines over the video.
    Overlay,
}

impl ChatLayout {
    pub const ALL: [ChatLayout; 2] = [ChatLayout::Side, ChatLayout::Overlay];

    /// Stable name used in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            ChatLayout::Side => "side",
            ChatLayout::Overlay => "overlay",
        }
    }
}

/// Preferred stream quality, passed to the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// The best available variant (Streamlink `best`).
    #[default]
    Best,
    P1080,
    P720,
    P480,
    /// Lowest bandwidth (Streamlink `worst`).
    Worst,
}

impl Quality {
    pub const ALL: [Quality; 5] = [
        Quality::Best,
        Quality::P1080,
        Quality::P720,
        Quality::P480,
        Quality::Worst,
    ];

    /// Stable name used in the config file. Also a valid Streamlink stream name prefix.
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Best => "best",
            Quality::P1080 => "1080p",
            Quality::P720 => "720p",
            Quality::P480 => "480p",
            Quality::Worst => "worst",
        }
    }
}

/// The value `step` places after `current` in `all`, wrapping around. `step` is 1 or -1.
fn cycle<T: Copy + PartialEq>(all: &[T], current: T, step: isize) -> T {
    let len = all.len() as isize;
    let at = all.iter().position(|v| *v == current).unwrap_or(0) as isize;
    all[(at + step).rem_euclid(len) as usize]
}

impl ChatLayout {
    /// The next (or, with `forward == false`, the previous) layout, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the Settings screen.
    pub fn label(self) -> &'static str {
        match self {
            ChatLayout::Side => "Side panel",
            ChatLayout::Overlay => "Over the video",
        }
    }
}

impl Quality {
    /// The next (or, with `forward == false`, the previous) quality, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the Settings screen.
    pub fn label(self) -> &'static str {
        match self {
            Quality::Best => "Best available",
            Quality::P1080 => "1080p",
            Quality::P720 => "720p",
            Quality::P480 => "480p",
            Quality::Worst => "Lowest (saves data)",
        }
    }
}

/// Whether the player may decode video on the GPU. A player-menu option (not saved yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hwdec {
    /// Hardware decoding where the player considers it safe (mpv `auto-safe`).
    #[default]
    Auto,
    /// Decode on the CPU.
    Off,
}

impl Hwdec {
    pub const ALL: [Hwdec; 2] = [Hwdec::Auto, Hwdec::Off];

    /// Stable name, for a config file later.
    pub fn as_str(self) -> &'static str {
        match self {
            Hwdec::Auto => "auto",
            Hwdec::Off => "off",
        }
    }

    /// The next (or, with `forward == false`, the previous) value, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the player menu.
    pub fn label(self) -> &'static str {
        match self {
            Hwdec::Auto => "Auto",
            Hwdec::Off => "Off",
        }
    }
}

/// How far behind live the player sits. Changing it reloads the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Latency {
    /// Small buffers: close to live, but a slow network stalls sooner.
    #[default]
    Low,
    /// Bigger buffers: a few seconds further behind, fewer stalls.
    Normal,
}

impl Latency {
    pub const ALL: [Latency; 2] = [Latency::Low, Latency::Normal];

    /// Stable name, for a config file later.
    pub fn as_str(self) -> &'static str {
        match self {
            Latency::Low => "low",
            Latency::Normal => "normal",
        }
    }

    /// The next (or, with `forward == false`, the previous) value, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the player menu.
    pub fn label(self) -> &'static str {
        match self {
            Latency::Low => "Low",
            Latency::Normal => "Normal (fewer stalls)",
        }
    }
}

/// How the picture fills a screen of another shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zoom {
    /// The whole picture, with black bars where the shapes differ.
    #[default]
    Fit,
    /// No bars: the picture is cropped to fill the screen (16:9 on the Deck's 16:10).
    Fill,
}

impl Zoom {
    pub const ALL: [Zoom; 2] = [Zoom::Fit, Zoom::Fill];

    /// Stable name, for a config file later.
    pub fn as_str(self) -> &'static str {
        match self {
            Zoom::Fit => "fit",
            Zoom::Fill => "fill",
        }
    }

    /// The next (or, with `forward == false`, the previous) value, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the player menu.
    pub fn label(self) -> &'static str {
        match self {
            Zoom::Fit => "Fit",
            Zoom::Fill => "Fill (crop)",
        }
    }
}

/// A value that is not one of the known setting names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSetting(pub String);

impl fmt::Display for UnknownSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown setting value {:?}", self.0)
    }
}

impl std::error::Error for UnknownSetting {}

fn parse_named<T: Copy>(
    all: &[T],
    name: fn(T) -> &'static str,
    s: &str,
) -> Result<T, UnknownSetting> {
    all.iter()
        .copied()
        .find(|v| name(*v).eq_ignore_ascii_case(s.trim()))
        .ok_or_else(|| UnknownSetting(s.to_owned()))
}

impl FromStr for ChatLayout {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for Quality {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for Hwdec {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for Latency {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for Zoom {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

/// Settings that affect behaviour. Part of `AppState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub chat_layout: ChatLayout,
    pub quality: Quality,
    /// A popup when a followed channel goes live (signed in only). Off stops the polling.
    pub live_notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            chat_layout: ChatLayout::default(),
            quality: Quality::default(),
            live_notifications: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for v in ChatLayout::ALL {
            assert_eq!(v.as_str().parse::<ChatLayout>(), Ok(v));
        }
        for v in Quality::ALL {
            assert_eq!(v.as_str().parse::<Quality>(), Ok(v));
        }
        for v in Hwdec::ALL {
            assert_eq!(v.as_str().parse::<Hwdec>(), Ok(v));
            assert_eq!(v.cycled(true).cycled(false), v);
        }
        for v in Latency::ALL {
            assert_eq!(v.as_str().parse::<Latency>(), Ok(v));
            assert_eq!(v.cycled(true).cycled(false), v);
        }
        for v in Zoom::ALL {
            assert_eq!(v.as_str().parse::<Zoom>(), Ok(v));
            assert_eq!(v.cycled(true).cycled(false), v);
        }
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        assert_eq!(ChatLayout::Side.cycled(true), ChatLayout::Overlay);
        assert_eq!(ChatLayout::Overlay.cycled(true), ChatLayout::Side);
        assert_eq!(ChatLayout::Side.cycled(false), ChatLayout::Overlay);
        assert_eq!(Quality::Worst.cycled(true), Quality::Best);
        assert_eq!(Quality::Best.cycled(false), Quality::Worst);
        for q in Quality::ALL {
            assert_eq!(q.cycled(true).cycled(false), q);
            assert!(!q.label().is_empty());
        }
    }

    #[test]
    fn live_notifications_are_on_by_default() {
        assert!(Settings::default().live_notifications);
    }

    #[test]
    fn parsing_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(" Overlay ".parse::<ChatLayout>(), Ok(ChatLayout::Overlay));
        assert_eq!("720P".parse::<Quality>(), Ok(Quality::P720));
        assert_eq!(
            "4k".parse::<Quality>(),
            Err(UnknownSetting("4k".to_owned()))
        );
    }
}
