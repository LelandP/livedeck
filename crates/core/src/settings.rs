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

/// How good the generated captions are, and so how big the speech model is (captions-plan.md,
/// section 2). The app maps each to a model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionsQuality {
    /// whisper tiny.en: the smallest and fastest, with wrong words now and then.
    Fast,
    /// whisper base.en, quantised: the default.
    #[default]
    Standard,
    /// whisper small, quantised, multilingual. Needs a GPU build to keep up.
    Best,
}

/// The quality new installs start with. Phase 0b (the Deck run) may change it.
pub const DEFAULT_CAPTIONS_QUALITY: CaptionsQuality = CaptionsQuality::Standard;

impl CaptionsQuality {
    pub const ALL: [CaptionsQuality; 3] = [
        CaptionsQuality::Fast,
        CaptionsQuality::Standard,
        CaptionsQuality::Best,
    ];

    /// Stable name used in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            CaptionsQuality::Fast => "fast",
            CaptionsQuality::Standard => "standard",
            CaptionsQuality::Best => "best",
        }
    }

    /// Text for the player menu.
    pub fn label(self) -> &'static str {
        match self {
            CaptionsQuality::Fast => "Fast",
            CaptionsQuality::Standard => "Standard",
            CaptionsQuality::Best => "Best",
        }
    }

    /// The next (or previous) quality among `offered`, wrapping around. A quality that is not
    /// offered counts as the first of them, so it steps from there.
    pub fn cycled_in(self, offered: &[CaptionsQuality], forward: bool) -> Self {
        if offered.is_empty() {
            return self;
        }
        cycle(offered, self, if forward { 1 } else { -1 })
    }
}

/// How big the captions are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionSize {
    /// For the Deck in your hands.
    Small,
    #[default]
    Medium,
    /// For a TV across the room.
    Large,
}

impl CaptionSize {
    pub const ALL: [CaptionSize; 3] = [CaptionSize::Small, CaptionSize::Medium, CaptionSize::Large];

    /// Stable name used in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            CaptionSize::Small => "small",
            CaptionSize::Medium => "medium",
            CaptionSize::Large => "large",
        }
    }

    /// The next (or, with `forward == false`, the previous) size, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the Settings screen.
    pub fn label(self) -> &'static str {
        match self {
            CaptionSize::Small => "Small",
            CaptionSize::Medium => "Medium",
            CaptionSize::Large => "Large",
        }
    }

    /// The text size against Medium's.
    pub fn scale(self) -> f32 {
        match self {
            CaptionSize::Small => 0.8,
            CaptionSize::Medium => 1.0,
            CaptionSize::Large => 1.35,
        }
    }
}

/// Where on the picture the captions go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionPosition {
    /// At the bottom, above the HUD while it shows.
    #[default]
    Bottom,
    /// At the top, for streams with their own text or a scoreboard at the bottom.
    Top,
}

impl CaptionPosition {
    pub const ALL: [CaptionPosition; 2] = [CaptionPosition::Bottom, CaptionPosition::Top];

    /// Stable name used in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            CaptionPosition::Bottom => "bottom",
            CaptionPosition::Top => "top",
        }
    }

    /// The other position (two values, so both directions are the same).
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the Settings screen.
    pub fn label(self) -> &'static str {
        match self {
            CaptionPosition::Bottom => "Bottom",
            CaptionPosition::Top => "Top",
        }
    }
}

/// The box behind each caption line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionBackground {
    /// No box: the text gets a dark outline instead, so it still reads on a bright picture.
    None,
    /// A see-through box.
    Light,
    /// A nearly solid box: the easiest to read.
    #[default]
    Dark,
}

impl CaptionBackground {
    pub const ALL: [CaptionBackground; 3] = [
        CaptionBackground::None,
        CaptionBackground::Light,
        CaptionBackground::Dark,
    ];

    /// Stable name used in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            CaptionBackground::None => "none",
            CaptionBackground::Light => "light",
            CaptionBackground::Dark => "dark",
        }
    }

    /// The next (or, with `forward == false`, the previous) value, wrapping around.
    pub fn cycled(self, forward: bool) -> Self {
        cycle(&Self::ALL, self, if forward { 1 } else { -1 })
    }

    /// Text for the Settings screen.
    pub fn label(self) -> &'static str {
        match self {
            CaptionBackground::None => "None",
            CaptionBackground::Light => "Light",
            CaptionBackground::Dark => "Dark",
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

impl FromStr for CaptionsQuality {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for CaptionSize {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for CaptionPosition {
    type Err = UnknownSetting;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_named(&Self::ALL, Self::as_str, s)
    }
}

impl FromStr for CaptionBackground {
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
    /// The stream keeps playing while another app has focus. Off pauses it until the app is
    /// back in front.
    pub background_play: bool,
    /// Kick next to Twitch: a Top Live on Kick row on Home and Kick channels in Search. Off, the
    /// app makes no Kick requests at all.
    pub show_kick: bool,
    /// Generated captions on a Twitch stream, made on the device (captions-plan.md). Changed in
    /// the player menu and saved. Only does anything where the build and the models allow it
    /// (`AppState::captions_unavailable`).
    pub captions: bool,
    /// The speech model captions use (`captions_quality` in `config.toml`). Changing it loads the
    /// other model, downloading it first if it is missing.
    pub captions_quality: CaptionsQuality,
    /// With captions on, the picture and chat run far enough behind the speech for every caption
    /// to be ready before its moment (captions-plan.md, phase 3b; `caption_delay` in
    /// `config.toml`, the player menu's *Delay video for caption sync*). Off: live latency, and
    /// captions come a few seconds late.
    pub caption_delay: bool,
    /// How the captions look, from the Settings screen (`caption_size`, `caption_position` and
    /// `caption_background` in `config.toml`).
    pub caption_size: CaptionSize,
    pub caption_position: CaptionPosition,
    pub caption_background: CaptionBackground,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            chat_layout: ChatLayout::default(),
            quality: Quality::default(),
            live_notifications: true,
            background_play: false,
            // Kick is read through an unofficial API that can break, so it is opt-in.
            show_kick: false,
            captions: false,
            captions_quality: DEFAULT_CAPTIONS_QUALITY,
            caption_delay: true,
            caption_size: CaptionSize::default(),
            caption_position: CaptionPosition::default(),
            caption_background: CaptionBackground::default(),
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
        for v in CaptionSize::ALL {
            assert_eq!(v.as_str().parse::<CaptionSize>(), Ok(v));
            assert_eq!(v.cycled(true).cycled(false), v);
        }
        for v in CaptionPosition::ALL {
            assert_eq!(v.as_str().parse::<CaptionPosition>(), Ok(v));
            assert_eq!(v.cycled(true).cycled(false), v);
        }
        for v in CaptionBackground::ALL {
            assert_eq!(v.as_str().parse::<CaptionBackground>(), Ok(v));
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
    fn background_play_is_off_by_default() {
        assert!(!Settings::default().background_play);
    }

    #[test]
    fn captions_quality_names_round_trip_and_standard_is_the_default() {
        for v in CaptionsQuality::ALL {
            assert_eq!(v.as_str().parse::<CaptionsQuality>(), Ok(v));
            assert!(!v.label().is_empty());
        }
        assert_eq!(DEFAULT_CAPTIONS_QUALITY, CaptionsQuality::Standard);
        assert_eq!(
            Settings::default().captions_quality,
            DEFAULT_CAPTIONS_QUALITY
        );
    }

    #[test]
    fn captions_quality_cycles_through_what_is_offered() {
        use CaptionsQuality::*;
        assert_eq!(Fast.cycled_in(&CaptionsQuality::ALL, true), Standard);
        assert_eq!(Best.cycled_in(&CaptionsQuality::ALL, true), Fast);
        assert_eq!(Fast.cycled_in(&CaptionsQuality::ALL, false), Best);
        // Without a GPU: Best is not offered, and one saved as Best steps into the list.
        let cpu = [Fast, Standard];
        assert_eq!(Standard.cycled_in(&cpu, true), Fast);
        assert_eq!(Fast.cycled_in(&cpu, false), Standard);
        assert_eq!(Best.cycled_in(&cpu, true), Standard);
    }

    #[test]
    fn captions_are_off_by_default() {
        assert!(!Settings::default().captions);
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
