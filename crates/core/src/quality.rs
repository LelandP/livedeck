//! Which rendition of a stream to play: from the size it has on screen, the Settings quality and
//! the network limit (plan.md, section 3). Pure, so the reducer and its tests share it.
//!
//! Streamlink needs a rendition's exact name, and Twitch and Kick name them differently: Twitch
//! puts the frame rate on every one (`480p30`, `720p60`), Kick only on 60 fps ones (`480p`,
//! `720p60`). A bare `720p` matches neither of Twitch's. So each request is a fallback list that
//! names every spelling of a height, then the heights below it, then `worst` (the lowest there
//! is, which is the nearest one above when nothing lower exists).

use crate::settings::Quality;

/// A video height LiveDeck asks for. Ordered from the lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rendition {
    P160,
    P360,
    P480,
    P720,
    P1080,
}

/// Which frame rate a fallback list tries first at each height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameRate {
    /// 60 fps where the stream has it: full screen and the single player.
    #[default]
    Smooth,
    /// 30 fps where the stream has both: half the decoding, for small tiles.
    Light,
}

/// A tile may be upscaled this much: a rendition is enough once it is at least this share of the
/// tile's height in pixels. The difference can't be seen from the couch.
pub const UPSCALE_ALLOWED: f64 = 0.9;

impl Rendition {
    /// Every rendition, lowest first.
    pub const ALL: [Rendition; 5] = [
        Rendition::P160,
        Rendition::P360,
        Rendition::P480,
        Rendition::P720,
        Rendition::P1080,
    ];

    /// Lines of video.
    pub fn height(self) -> u32 {
        match self {
            Rendition::P160 => 160,
            Rendition::P360 => 360,
            Rendition::P480 => 480,
            Rendition::P720 => 720,
            Rendition::P1080 => 1080,
        }
    }

    /// Text for a tile's quality chip.
    pub fn label(self) -> &'static str {
        match self {
            Rendition::P160 => "160p",
            Rendition::P360 => "360p",
            Rendition::P480 => "480p",
            Rendition::P720 => "720p",
            Rendition::P1080 => "1080p",
        }
    }

    /// The highest rendition the Settings quality allows. `Best` allows everything, `Worst` only
    /// the lowest.
    pub fn ceiling(quality: Quality) -> Rendition {
        match quality {
            Quality::Best | Quality::P1080 => Rendition::P1080,
            Quality::P720 => Rendition::P720,
            Quality::P480 => Rendition::P480,
            Quality::Worst => Rendition::P160,
        }
    }

    /// `steps` renditions lower, stopping at the lowest.
    pub fn lowered(self, steps: u8) -> Rendition {
        let at = Self::ALL.iter().position(|r| *r == self).unwrap_or(0);
        Self::ALL[at.saturating_sub(usize::from(steps))]
    }

    /// The Streamlink fallback list for this rendition: each height from this one down, in every
    /// spelling Twitch and Kick use, then `worst`. For example `480p30,480p,480p60,360p30,...`.
    pub fn streamlink_list(self, rate: FrameRate) -> String {
        let suffixes = match rate {
            FrameRate::Smooth => ["60", "", "30"],
            FrameRate::Light => ["30", "", "60"],
        };
        let mut names: Vec<String> = Self::ALL
            .iter()
            .rev()
            .filter(|r| **r <= self)
            .flat_map(|r| suffixes.map(|fps| format!("{}p{fps}", r.height())))
            .collect();
        names.push("worst".to_owned());
        names.join(",")
    }
}

/// The rendition for a tile `tile_pixels` physical pixels tall: the lowest that covers it (allowing
/// `UPSCALE_ALLOWED`), never above the Settings quality `cap`, then lowered `net_steps` times by
/// the network limit. Whether the tile has sound plays no part, so moving the sound never reloads.
pub fn rendition(tile_pixels: f32, cap: Quality, net_steps: u8) -> Rendition {
    let need = f64::from(tile_pixels) * UPSCALE_ALLOWED;
    let fit = Rendition::ALL
        .into_iter()
        .find(|r| f64::from(r.height()) >= need - 1e-6)
        .unwrap_or(Rendition::P1080);
    fit.min(Rendition::ceiling(cap)).lowered(net_steps)
}

impl Quality {
    /// What the single player asks Streamlink for at this quality: `best` and `worst` as they
    /// are, a height as its fallback list (60 fps first).
    pub fn streamlink_list(self) -> String {
        match self {
            Quality::Best => "best".to_owned(),
            Quality::Worst => "worst".to_owned(),
            q => Rendition::ceiling(q).streamlink_list(FrameRate::Smooth),
        }
    }
}
