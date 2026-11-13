//! Videos and clips (vod-plan.md): the domain types of a channel page and of the player's video
//! details. UI-agnostic like `domain.rs`: raw numbers, no formatted strings.

use crate::domain::{ChannelRef, Rgb, Stream};
use crate::replay::ReplayChat;

/// A stretch of a video, in whole seconds from its start (a muted part).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_s: u32,
    pub length_s: u32,
}

impl Span {
    /// The second just after it.
    pub fn end_s(&self) -> u32 {
        self.start_s.saturating_add(self.length_s)
    }

    /// `second` falls inside it.
    pub fn contains(&self, second: u32) -> bool {
        (self.start_s..self.end_s()).contains(&second)
    }
}

/// Which kind of video. Uploads are left out (vod-plan.md section 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoKind {
    /// A past broadcast.
    Archive,
    Highlight,
}

/// A video of a channel (Twitch: `https://www.twitch.tv/videos/<id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    /// Twitch's id: digits only.
    pub id: String,
    /// Whose video it is.
    pub channel: ChannelRef,
    /// The channel's name as the streamer writes it.
    pub channel_name: String,
    pub kind: VideoKind,
    pub title: String,
    /// The category it was streamed in (the first one, for a broadcast with several).
    pub category: String,
    pub length_s: u32,
    /// When it was streamed, in Unix seconds. 0 when unknown.
    pub created_at: u64,
    pub views: u64,
    pub tint: Rgb,
    /// Preview image URL, already sized for a card. `None` keeps the tint placeholder.
    pub thumbnail: Option<String>,
    /// Parts Twitch muted (copyrighted music), in order.
    pub muted: Vec<Span>,
    /// Only the channel's subscribers can watch it.
    pub sub_only: bool,
    /// Still being recorded: the channel is live and this is its broadcast so far.
    pub recording: bool,
}

/// A clip of a channel (Twitch: `https://clips.twitch.tv/<slug>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// Twitch's slug: letters, digits, `_` and `-`.
    pub slug: String,
    /// Whose stream it was cut from.
    pub channel: ChannelRef,
    pub title: String,
    pub category: String,
    pub length_s: u32,
    pub views: u64,
    /// Who made it. Empty when unknown.
    pub clipped_by: String,
    /// When it was made, in Unix seconds. 0 when unknown.
    pub created_at: u64,
    pub tint: Rgb,
    pub thumbnail: Option<String>,
}

/// A saved place in a video: one entry of *Continue watching* (vod-plan.md section 5). Phase 5
/// fills `AppState::continue_watching` from `continue.toml`. Until then it is empty, and the
/// channel page already reads it for *Resume at …*, *Start over* and the progress bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The video's card, kept whole so Home's row can draw it without asking Twitch.
    pub video: Video,
    /// Where it was left, in whole seconds from the start.
    pub pos_s: u32,
    /// When it was saved, in Unix seconds. Newest first decides the order.
    pub saved_at: u64,
}

impl Progress {
    /// How far in, in thousandths of the video's length (0..=1000). 0 for a video of no length.
    pub fn permille(&self) -> u32 {
        if self.video.length_s == 0 {
            return 0;
        }
        let done = u64::from(self.pos_s.min(self.video.length_s)) * 1000;
        (done / u64::from(self.video.length_s)) as u32
    }
}

/// What the video player plays: a video or a clip, as its card had it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoItem {
    Video(Video),
    Clip(Clip),
}

impl VideoItem {
    /// What `Command::PlayVideo` asks for.
    pub fn target(&self) -> VideoTarget {
        match self {
            VideoItem::Video(v) => VideoTarget::Video(v.id.clone()),
            VideoItem::Clip(c) => VideoTarget::Clip(c.slug.clone()),
        }
    }

    pub fn channel(&self) -> &ChannelRef {
        match self {
            VideoItem::Video(v) => &v.channel,
            VideoItem::Clip(c) => &c.channel,
        }
    }

    /// The channel's name as the streamer writes it (a clip only knows the login).
    pub fn channel_name(&self) -> &str {
        match self {
            VideoItem::Video(v) if !v.channel_name.is_empty() => &v.channel_name,
            VideoItem::Video(v) => &v.channel.login,
            VideoItem::Clip(c) => &c.channel.login,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            VideoItem::Video(v) => &v.title,
            VideoItem::Clip(c) => &c.title,
        }
    }

    pub fn length_ms(&self) -> u64 {
        let s = match self {
            VideoItem::Video(v) => v.length_s,
            VideoItem::Clip(c) => c.length_s,
        };
        u64::from(s) * 1_000
    }

    pub fn is_clip(&self) -> bool {
        matches!(self, VideoItem::Clip(_))
    }

    /// The Twitch video id, for a video (`Command::LoadVideoDetails`).
    pub fn video_id(&self) -> Option<&str> {
        match self {
            VideoItem::Video(v) => Some(&v.id),
            VideoItem::Clip(_) => None,
        }
    }
}

/// The rows of the card at the end of a video, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndRow {
    #[default]
    WatchAgain,
    ChannelVideos,
    Back,
}

impl EndRow {
    pub const ALL: [EndRow; 3] = [EndRow::WatchAgain, EndRow::ChannelVideos, EndRow::Back];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The row above, stopping at the top.
    pub fn prev(self) -> Self {
        Self::ALL[self.index().saturating_sub(1)]
    }

    /// The row below, stopping at the bottom.
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }
}

/// Whether the player can take a `Command::Seek` now. While it cannot, a seek goes into the
/// `start_ms` of a fresh `Command::PlayVideo` instead (vod-plan.md, phase 2's notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoLoad {
    /// The first load: nothing is loaded yet, so a seek would be dropped.
    #[default]
    Starting,
    /// It plays: seeks go to the player.
    Shown,
    /// Another quality (or sound only) loads behind the picture: a seek to the player on screen
    /// would be lost when the new one takes over. `reports` counts health reports since, for
    /// `seek::RELOAD_GIVE_UP_REPORTS`.
    Reloading { reports: u32 },
}

/// A seek on its way: health reports from before it are ignored for a while.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landing {
    pub to_ms: u64,
    /// Reports still to ignore if they are not near `to_ms`.
    pub reports: u8,
}

/// The video on the Player screen (`AppState::video`). `now_playing` stays `None` meanwhile, so
/// everything that follows a live stream (chat, raids, hype trains, the offline recheck, the
/// slow-network rule) keeps out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSession {
    pub item: VideoItem,
    /// Where it is, in ms: the latest health report, or where it was told to start or seek to.
    /// The resume point when it is left (Continue watching, phase 5).
    pub pos_ms: u64,
    /// How long it is, in ms: from the player once it says, before that from the card. 0 when
    /// unknown.
    pub length_ms: u64,
    /// From `Event::VideoDetails`. Empty until then, and for a clip.
    pub chapters: Vec<Chapter>,
    /// The parts Twitch muted: the card's, then the details'.
    pub muted: Vec<Span>,
    /// The seek-preview storyboard (`VideoDetails::previews`). Not drawn yet (phase 8).
    pub previews: Option<String>,
    /// The speed, in thousandths. Every video starts at 1×.
    pub speed: u32,
    /// ◀ ▶: taps, holds and scrubs.
    pub seeking: crate::seek::Seeking,
    /// The id of the latest seek timer (`SettleSeek`, `HoldCheck`, `ScrubTick`). Only its
    /// events count.
    pub timer: u32,
    pub load: VideoLoad,
    pub landing: Option<Landing>,
    /// The card at the end of the video, on this row.
    pub end_card: Option<EndRow>,
    /// The chapter the menu's *Chapters* row points at (A jumps there).
    pub menu_chapter: usize,
    /// The time the card's page was fetched (`ChannelPage::now`), for *Streamed Nov 10*. 0 when
    /// unknown, which leaves the year out.
    pub now: u64,
    /// The chat replay (phase 6): what was fetched and is not due yet, and what is on its way.
    /// The lines shown are in `AppState::chat`.
    pub replay: ReplayChat,
}

impl VideoSession {
    pub fn new(item: VideoItem, start_ms: u64, now: u64) -> Self {
        let muted = match &item {
            VideoItem::Video(v) => v.muted.clone(),
            VideoItem::Clip(_) => Vec::new(),
        };
        Self {
            length_ms: item.length_ms(),
            item,
            pos_ms: start_ms,
            chapters: Vec::new(),
            muted,
            previews: None,
            speed: crate::seek::NORMAL_SPEED,
            seeking: crate::seek::Seeking::Idle,
            timer: 0,
            load: VideoLoad::Starting,
            landing: None,
            end_card: None,
            menu_chapter: 0,
            now,
            replay: ReplayChat::default(),
        }
    }

    /// The furthest a seek goes: a little before the end. No limit while the length is unknown.
    pub fn end_ms(&self) -> u64 {
        if self.length_ms == 0 {
            u64::MAX
        } else {
            self.length_ms.saturating_sub(crate::seek::END_GUARD_MS)
        }
    }

    /// `ms`, kept inside the video.
    pub fn clamp(&self, ms: u64) -> u64 {
        ms.min(self.end_ms())
    }

    /// Where the playhead is drawn: the scrub's, where the taps lead, or where the video is.
    pub fn shown_ms(&self) -> u64 {
        use crate::seek::Seeking;
        match &self.seeking {
            Seeking::Scrub(scrub) => scrub.at_ms,
            Seeking::Pressed { run, .. } | Seeking::Taps { run, .. } => {
                self.clamp(run.raw_target())
            }
            Seeking::Idle => self.pos_ms,
        }
    }

    /// The chapter at `ms`: the last one starting at or before it.
    pub fn chapter_at(&self, ms: u64) -> Option<usize> {
        self.chapters.iter().rposition(|c| c.start_ms <= ms)
    }

    /// `ms` falls in a part Twitch muted.
    pub fn is_muted_at(&self, ms: u64) -> bool {
        let second = u32::try_from(ms / 1_000).unwrap_or(u32::MAX);
        self.muted.iter().any(|m| m.contains(second))
    }
}

/// What `Command::PlayVideo` plays: a Twitch video by its id, or a Twitch clip by its slug. The
/// app checks both before they reach Streamlink.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VideoTarget {
    /// A past broadcast or highlight (`Video::id`).
    Video(String),
    /// A clip (`Clip::slug`).
    Clip(String),
}

/// One chapter of a video: where the streamer changed category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub start_ms: u64,
    pub length_ms: u64,
    /// The category's name.
    pub name: String,
}

/// What the player needs about a video beyond its card (`Command::LoadVideoDetails`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoDetails {
    pub id: String,
    pub length_s: u32,
    /// In order, at least one: a video without chapter markers is one chapter.
    pub chapters: Vec<Chapter>,
    pub muted: Vec<Span>,
    /// The seek-preview storyboard description (`…-info.json`), if Twitch has one.
    pub previews: Option<String>,
}

/// The top of a channel page (`Event::ChannelPageLoaded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub channel: ChannelRef,
    /// The broadcaster's id on its platform.
    pub user_id: String,
    /// The name as the streamer writes it.
    pub name: String,
    /// Profile picture URL.
    pub avatar: Option<String>,
    /// The live stream, while the channel is live.
    pub live: Option<Stream>,
    /// When the last broadcast started, in Unix seconds (live or not). `None` if it never
    /// streamed, or Twitch does not say.
    pub last_live_at: Option<u64>,
    /// The time the page was fetched, in Unix seconds. The reference for *last live 3 hours ago*
    /// and the dates on the cards: the reducer and the view-model have no clock of their own.
    pub loaded_at: u64,
}

/// Unix seconds of an RFC 3339 time as Twitch writes it (`2026-11-10T18:28:22Z`, with or without
/// fractions, `Z` or an offset). `None` for anything else.
pub fn unix_from_rfc3339(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut d = date.splitn(3, '-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if date.len() != 10 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // The zone: `Z`, or `+hh:mm` / `-hh:mm` after the time.
    let (time, offset_s) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0i64)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (t, zone) = rest.split_at(at);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (h, m) = zone[1..].split_once(':')?;
        let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
        (t, sign * (h * 3600 + m * 60))
    };
    let time = time.split('.').next()?;
    let mut t = time.splitn(3, ':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next()?.parse().ok()?;
    if time.len() != 8 || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_s;
    u64::try_from(secs).ok()
}

/// The date (year, month 1-12, day 1-31) of a Unix time, in UTC. The inverse of
/// `unix_from_rfc3339`'s date part (Howard Hinnant's `civil_from_days`).
pub fn date_from_unix(secs: u64) -> (i64, u32, u32) {
    let z = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_as_twitch_writes_it() {
        assert_eq!(unix_from_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            unix_from_rfc3339("2026-11-10T18:28:22Z"),
            Some(1_794_335_302)
        );
        // Fractions (lastBroadcast.startedAt) are dropped.
        assert_eq!(
            unix_from_rfc3339("2026-11-11T13:14:46.344941Z"),
            unix_from_rfc3339("2026-11-11T13:14:46Z")
        );
        // An offset is the same instant.
        assert_eq!(
            unix_from_rfc3339("2026-11-10T20:28:22+02:00"),
            Some(1_794_335_302)
        );
        assert_eq!(
            unix_from_rfc3339("2026-11-10T13:28:22-05:00"),
            Some(1_794_335_302)
        );
        // Leap day.
        assert_eq!(
            unix_from_rfc3339("2024-03-01T00:00:00Z").unwrap()
                - unix_from_rfc3339("2024-02-28T00:00:00Z").unwrap(),
            2 * 86_400
        );
    }

    #[test]
    fn rfc3339_rejects_other_text() {
        for bad in [
            "",
            "2026-11-10",
            "2026-11-10T18:28:22",
            "2026-13-10T18:28:22Z",
            "2026-11-10T25:00:00Z",
            "20261110T182822Z",
            "1969-12-31T23:59:59Z",
            "yesterday",
        ] {
            assert_eq!(unix_from_rfc3339(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dates_from_unix_time() {
        assert_eq!(date_from_unix(0), (1970, 1, 1));
        assert_eq!(date_from_unix(1_794_335_302), (2026, 11, 10));
        // Round trip over leap days, year ends and a few random days.
        for text in [
            "2024-02-29T23:59:59Z",
            "2024-03-01T00:00:00Z",
            "2025-12-31T12:00:00Z",
            "2026-01-01T00:00:00Z",
            "2000-02-29T00:00:00Z",
            "2100-03-01T00:00:00Z",
        ] {
            let (y, m, d) = date_from_unix(unix_from_rfc3339(text).unwrap());
            assert_eq!(format!("{y:04}-{m:02}-{d:02}"), text[..10], "{text}");
        }
    }

    #[test]
    fn progress_in_thousandths() {
        let video = Video {
            id: "1".into(),
            channel: ChannelRef::twitch("c"),
            channel_name: "c".into(),
            kind: VideoKind::Archive,
            title: String::new(),
            category: String::new(),
            length_s: 4_000,
            created_at: 0,
            views: 0,
            tint: Rgb::default(),
            thumbnail: None,
            muted: Vec::new(),
            sub_only: false,
            recording: false,
        };
        let at = |pos_s| Progress {
            video: video.clone(),
            pos_s,
            saved_at: 0,
        };
        assert_eq!(at(0).permille(), 0);
        assert_eq!(at(1_000).permille(), 250);
        assert_eq!(at(9_999).permille(), 1_000, "never past the end");
        let empty = Progress {
            video: Video {
                length_s: 0,
                ..video
            },
            pos_s: 5,
            saved_at: 0,
        };
        assert_eq!(empty.permille(), 0);
    }

    #[test]
    fn spans() {
        let s = Span {
            start_s: 5400,
            length_s: 200,
        };
        assert_eq!(s.end_s(), 5600);
        assert!(s.contains(5400) && s.contains(5599));
        assert!(!s.contains(5399) && !s.contains(5600));
    }
}
