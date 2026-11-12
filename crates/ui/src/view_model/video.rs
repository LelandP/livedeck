//! The video player's view (vod-plan.md, phase 4): the HUD (channel, title, *Streamed Nov 10*,
//! the chapter), the seek bar (chapter ticks, muted parts, the playhead), the tap chip, the time
//! above the playhead while seeking, and the end card. The storyboard preview is not drawn yet
//! (phase 8): while seeking the time and chapter show above the playhead instead.

use htpc_core::{AppState, Screen, Seeking, VideoItem, VideoKind, VideoSession};

use super::{format_date, format_duration, format_viewers, meta_line, to_i32, CardVm};

/// The seek bar's scale: positions are in ten-thousandths of the length.
const BAR: u64 = 10_000;

/// A part of the seek bar, in ten-thousandths of the length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanVm {
    pub start: i32,
    pub width: i32,
}

/// Everything the Player screen shows for a video. `on` is false (and the rest empty) while no
/// video is on the Player screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VideoVm {
    pub on: bool,
    /// "PAST BROADCAST", "HIGHLIGHT" or "CLIP", in a chip beside the channel's name.
    pub kind: String,
    pub channel: String,
    pub title: String,
    /// "Streamed Nov 10 · 18K views · Chapter 2 of 5: Hollow Depths".
    pub meta: String,
    /// Where the playhead is, and the length: "1:12:40", "4:12:08". The length is empty while
    /// unknown.
    pub pos: String,
    pub length: String,
    /// The playhead, in ten-thousandths of the length.
    pub head: i32,
    /// The chapter at the playhead, beside the time. Empty with fewer than two chapters.
    pub chapter: String,
    /// Where each chapter after the first starts: ticks on the bar.
    pub ticks: Vec<i32>,
    /// The parts Twitch muted: striped on the bar.
    pub muted: Vec<SpanVm>,
    /// The playhead is in a muted part: the chip *Twitch muted the audio here*.
    pub in_muted: bool,
    /// Taps adding up: "+30 s", "−10 s". Empty otherwise.
    pub chip: String,
    /// Where the chip shows: -1 on the left (back), 1 on the right (forward).
    pub chip_side: i32,
    /// Taps or a scrub under way: the bar stays up, with the time above the playhead.
    pub seeking: bool,
    /// Holding to scrub: the picture is still and only the playhead moves. A jumps, B cancels.
    pub scrubbing: bool,
    /// "1.5×". Empty at normal speed.
    pub speed: String,
    /// LB / RB have chapters to jump between (a video, not a clip).
    pub chapters: bool,
    /// The card at the end of the video, and its focused row (`EndRow` order).
    pub end_card: bool,
    pub end_focus: i32,
}

/// A speed in thousandths as the menu and the HUD say it: "0.75×", "1×", "1.5×", "2×".
pub fn speed_label(milli: u32) -> String {
    let whole = milli / 1_000;
    let frac = milli % 1_000;
    let text = if frac == 0 {
        whole.to_string()
    } else {
        let digits = format!("{frac:03}");
        format!("{whole}.{}", digits.trim_end_matches('0'))
    };
    format!("{text}\u{d7}")
}

/// What a run of taps adds up to: "+30 s", "−10 s", "+1:10" from a minute.
pub fn seek_chip(delta_ms: i64) -> String {
    let sign = if delta_ms < 0 { "\u{2212}" } else { "+" };
    let secs = delta_ms.unsigned_abs() / 1_000;
    if secs < 60 {
        format!("{sign}{secs} s")
    } else {
        format!("{sign}{}:{:02}", secs / 60, secs % 60)
    }
}

/// `ms` of `length_ms`, in ten-thousandths. 0 while the length is unknown.
fn on_bar(ms: u64, length_ms: u64) -> i32 {
    if length_ms == 0 {
        return 0;
    }
    to_i32((ms.min(length_ms) * BAR / length_ms) as usize)
}

fn seconds(ms: u64) -> u32 {
    u32::try_from(ms / 1_000).unwrap_or(u32::MAX)
}

/// "Chapter 2 of 5: Hollow Depths" at `ms`, or the only chapter's name, or the category.
fn chapter_text(v: &VideoSession, ms: u64) -> String {
    match v.chapters.len() {
        0 => match &v.item {
            VideoItem::Video(video) => video.category.clone(),
            VideoItem::Clip(clip) => clip.category.clone(),
        },
        1 => v.chapters[0].name.clone(),
        n => {
            let i = v.chapter_at(ms).unwrap_or(0);
            format!("Chapter {} of {n}: {}", i + 1, v.chapters[i].name)
        }
    }
}

/// The line under the title.
fn meta(v: &VideoSession, ms: u64) -> String {
    match &v.item {
        VideoItem::Video(video) => {
            let date = format_date(video.created_at, v.now);
            meta_line(&[
                if date.is_empty() {
                    String::new()
                } else {
                    format!("Streamed {date}")
                },
                if video.views > 0 {
                    format!("{} views", format_viewers(video.views))
                } else {
                    String::new()
                },
                chapter_text(v, ms),
            ])
        }
        VideoItem::Clip(clip) => meta_line(&[
            if clip.clipped_by.is_empty() {
                String::new()
            } else {
                format!("Clipped by {}", clip.clipped_by)
            },
            format_date(clip.created_at, v.now),
            if clip.views > 0 {
                format!("{} views", format_viewers(clip.views))
            } else {
                String::new()
            },
        ]),
    }
}

/// The menu's *Chapters* value: the chapter it points at, "2 of 5 · Hollow Depths · 21:00".
pub(super) fn chapters_value(v: &VideoSession) -> String {
    match v.chapters.get(v.menu_chapter) {
        Some(c) => format!(
            "{} of {} \u{b7} {} \u{b7} {}",
            v.menu_chapter + 1,
            v.chapters.len(),
            c.name,
            format_duration(seconds(c.start_ms))
        ),
        None => "None".to_owned(),
    }
}

/// The card the player screen's `stream` shows for a video: its channel and title, for the
/// loading tint, the audio-only watermark and the error screen.
pub(super) fn now_playing_card(v: &VideoSession) -> CardVm {
    let (tint, thumb) = match &v.item {
        VideoItem::Video(video) => (video.tint, video.thumbnail.clone()),
        VideoItem::Clip(clip) => (clip.tint, clip.thumbnail.clone()),
    };
    CardVm {
        channel: v.item.channel_name().to_owned(),
        title: v.item.title().to_owned(),
        tint,
        thumb_url: thumb.unwrap_or_default(),
        ..CardVm::default()
    }
}

/// The video's view, or an empty one while no video is on the Player screen.
pub fn video_vm(state: &AppState) -> VideoVm {
    let Some(v) = state
        .video
        .as_ref()
        .filter(|_| state.screen == Screen::Player)
    else {
        return VideoVm::default();
    };
    let at = v.shown_ms();
    let length = v.length_ms;
    let (chip, chip_side) = match &v.seeking {
        Seeking::Pressed { run, .. } | Seeking::Taps { run, .. } => (
            seek_chip(run.delta_ms),
            if run.delta_ms < 0 { -1 } else { 1 },
        ),
        Seeking::Idle | Seeking::Scrub(_) => (String::new(), 0),
    };
    VideoVm {
        on: true,
        kind: match &v.item {
            VideoItem::Video(video) => match video.kind {
                VideoKind::Archive => "PAST BROADCAST",
                VideoKind::Highlight => "HIGHLIGHT",
            },
            VideoItem::Clip(_) => "CLIP",
        }
        .to_owned(),
        channel: v.item.channel_name().to_owned(),
        title: v.item.title().to_owned(),
        meta: meta(v, at),
        pos: format_duration(seconds(at)),
        length: if length == 0 {
            String::new()
        } else {
            format_duration(seconds(length))
        },
        head: on_bar(at, length),
        chapter: if v.chapters.len() > 1 {
            v.chapter_at(at)
                .map(|i| v.chapters[i].name.clone())
                .unwrap_or_default()
        } else {
            String::new()
        },
        ticks: v
            .chapters
            .iter()
            .skip(1)
            .map(|c| on_bar(c.start_ms, length))
            .collect(),
        muted: v
            .muted
            .iter()
            .map(|m| {
                let start = on_bar(u64::from(m.start_s) * 1_000, length);
                let end = on_bar(u64::from(m.end_s()) * 1_000, length);
                SpanVm {
                    start,
                    width: (end - start).max(1),
                }
            })
            .filter(|_| length > 0)
            .collect(),
        in_muted: v.is_muted_at(at),
        chip,
        chip_side,
        seeking: !v.seeking.is_idle(),
        scrubbing: v.seeking.scrub().is_some(),
        speed: if v.speed == htpc_core::seek::NORMAL_SPEED {
            String::new()
        } else {
            speed_label(v.speed)
        },
        chapters: !v.item.is_clip(),
        end_card: v.end_card.is_some(),
        end_focus: v.end_card.map_or(0, |r| to_i32(r.index())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_read_as_people_say_them() {
        assert_eq!(speed_label(750), "0.75\u{d7}");
        assert_eq!(speed_label(1_000), "1\u{d7}");
        assert_eq!(speed_label(1_250), "1.25\u{d7}");
        assert_eq!(speed_label(1_500), "1.5\u{d7}");
        assert_eq!(speed_label(2_000), "2\u{d7}");
    }

    #[test]
    fn the_chip_counts_seconds_then_minutes() {
        assert_eq!(seek_chip(10_000), "+10 s");
        assert_eq!(seek_chip(-30_000), "\u{2212}30 s");
        assert_eq!(seek_chip(0), "+0 s");
        assert_eq!(seek_chip(70_000), "+1:10");
        assert_eq!(seek_chip(-120_000), "\u{2212}2:00");
    }

    #[test]
    fn the_bar_is_in_ten_thousandths() {
        assert_eq!(on_bar(0, 1_000), 0);
        assert_eq!(on_bar(500, 1_000), 5_000);
        assert_eq!(on_bar(2_000, 1_000), 10_000, "never past the end");
        assert_eq!(on_bar(5, 0), 0, "unknown length");
    }
}
