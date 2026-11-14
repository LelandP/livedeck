//! Generated captions on videos and clips (captions-plan.md, phase 5): the menu rows, words that
//! wait in the queue for the video's clock (the tap reads up to 30 s ahead of the playhead), a
//! clock that moves at the video's speed and stands still while it is scrubbed, a seek that
//! clears everything until its first report has landed, and the live-only parts (the delay, the
//! held chat) staying out of it. The reducer has no clock, so these tests play the part of the
//! app: they send the player's health reports and the ticks it asks for.

use htpc_core::captions::{CLEAR_AFTER_MS, MAX_QUEUED};
use htpc_core::*;

const MIN: u64 = 60_000;
/// What a Twitch video's clock is ahead of its position (measured 2026-11-13: 62.05 s).
const START: u64 = 62_050;

fn video(id: &str) -> Video {
    Video {
        id: id.into(),
        channel: ChannelRef::twitch("sleepy"),
        channel_name: "Sleepy Fox".into(),
        kind: VideoKind::Archive,
        title: format!("video {id}"),
        category: "Hollow Knight".into(),
        length_s: 15_128,
        created_at: 1_794_335_302,
        views: 18_000,
        tint: Rgb::default(),
        thumbnail: None,
        muted: Vec::new(),
        sub_only: false,
        recording: false,
    }
}

fn clip(slug: &str) -> Clip {
    Clip {
        slug: slug.into(),
        channel: ChannelRef::twitch("sleepy"),
        title: format!("clip {slug}"),
        category: "Hollow Knight".into(),
        length_s: 28,
        views: 5,
        clipped_by: "mossy_k".into(),
        created_at: 1_794_335_302,
        tint: Rgb::default(),
        thumbnail: None,
    }
}

fn press(s: &AppState, intents: &[Intent]) -> (AppState, Vec<Command>) {
    let mut state = s.clone();
    let mut all = Vec::new();
    for intent in intents {
        let (next, cmds) = reduce(&state, *intent);
        state = next;
        all.extend(cmds);
    }
    (state, all)
}

/// Sleepy's channel page, loaded, focus on the first broadcast: broadcast `1`, a clip.
fn page() -> AppState {
    // Words come on at their start (no `SHOW_AFTER_MS`); tests/captions.rs covers the offset.
    let (mut s, _) = init();
    s.captions.show_after = Some(0);
    let offline = Tile::Channel(Channel {
        channel: ChannelRef::twitch("sleepy"),
        name: "sleepy".into(),
        tint: Rgb::default(),
    });
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![offline])],
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Select);
    let info = ChannelInfo {
        channel: ChannelRef::twitch("sleepy"),
        user_id: "77".into(),
        name: "Sleepy Fox".into(),
        avatar: None,
        live: None,
        last_live_at: Some(1_794_335_302),
        loaded_at: 1_794_346_102,
    };
    let rows = vec![
        Row::new(RowKind::PastBroadcasts, vec![Tile::Video(video("1"))]),
        Row::new(RowKind::ClipsWeek, vec![Tile::Clip(clip("a"))]),
    ];
    reduce(
        &s,
        Event::ChannelPageLoaded {
            info: Box::new(info),
            rows,
        },
    )
    .0
}

/// Chapters at 0 and 21:00.
fn details() -> Event {
    let chapter = |start_ms: u64, name: &str| Chapter {
        start_ms,
        length_ms: 0,
        name: name.into(),
    };
    Event::VideoDetails(VideoDetails {
        id: "1".into(),
        length_s: 15_128,
        chapters: vec![
            chapter(0, "Just Chatting"),
            chapter(21 * MIN, "Hollow Depths"),
        ],
        muted: Vec::new(),
        previews: None,
    })
}

/// A report of the video at `pos_ms`, on a clock `START` ahead of it.
fn health(pos_ms: u64) -> Event {
    Event::PlayerHealth {
        slot: MAIN_SLOT,
        at_ms: 0,
        health: Health {
            stalled: false,
            buffer_ms: 60_000,
            pos_ms: Some(pos_ms),
            length_ms: Some(15_128_000),
            stream_ms: Some(pos_ms + START),
        },
    }
}

/// Broadcast `1` playing, with captions on or off in the settings, at `pos_ms`.
fn watching(on: bool, pos_ms: u64) -> AppState {
    let mut s = page();
    s.settings.captions = on;
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, details()).0;
    reduce(&s, health(pos_ms)).0
}

/// The clip, playing.
fn watching_clip(on: bool) -> AppState {
    let mut s = page();
    s.settings.captions = on;
    // The clip is on the Clips tab.
    let (s, _) = press(&s, &[Intent::PageRight, Intent::Select]);
    assert!(s.video.as_ref().is_some_and(|v| v.item.is_clip()));
    reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0
}

fn at(s: &AppState, pos_ms: u64) -> (AppState, Vec<Command>) {
    reduce(s, health(pos_ms))
}

/// A caption with its words' own times, on the video's clock.
fn timed(s: &AppState, words: &[(u64, &str)]) -> (AppState, Vec<Command>) {
    let text: Vec<&str> = words.iter().map(|(_, w)| *w).collect();
    reduce(
        s,
        Event::Caption {
            start_ms: words[0].0,
            end_ms: words[words.len() - 1].0 + 500,
            text: text.join(" "),
            words: words
                .iter()
                .map(|(ms, w)| CaptionWord::new(*ms, *w))
                .collect(),
        },
    )
}

fn shown(s: &AppState) -> Vec<String> {
    s.captions.shown.clone()
}

fn queued(s: &AppState) -> Vec<String> {
    s.captions.queue.iter().map(|w| w.text.clone()).collect()
}

fn tick_in(cmds: &[Command]) -> Option<u32> {
    cmds.iter().find_map(|c| match c {
        Command::CaptionTick { id, .. } => Some(*id),
        _ => None,
    })
}

fn is_empty(s: &AppState) -> bool {
    s.captions.queue.is_empty() && s.captions.shown.is_empty() && s.captions.clock_ms.is_none()
}

fn seeks(cmds: &[Command]) -> Vec<u64> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Seek { to_ms, .. } => Some(*to_ms),
            _ => None,
        })
        .collect()
}

// ---- The menu --------------------------------------------------------------------------------

#[test]
fn a_videos_and_a_clips_menu_end_with_the_caption_rows_and_have_no_delay() {
    for rows in [&PlayerMenuRow::VIDEO[..], &PlayerMenuRow::CLIP[..]] {
        assert_eq!(
            rows[rows.len() - 2..],
            [PlayerMenuRow::Captions, PlayerMenuRow::CaptionsQuality]
        );
        assert!(!rows.contains(&PlayerMenuRow::CaptionDelay));
    }
    assert_eq!(
        watching(false, MIN).player_menu_rows(),
        &PlayerMenuRow::VIDEO
    );
    assert_eq!(
        watching_clip(false).player_menu_rows(),
        &PlayerMenuRow::CLIP
    );
}

/// The menu open on the *Captions* row of whatever plays.
fn on_row(s: &AppState) -> AppState {
    let (mut s, _) = press(s, &[Intent::Menu]);
    for _ in 0..12 {
        if s.player_menu == Some(PlayerMenuRow::Captions) {
            return s;
        }
        s = press(&s, &[Intent::Down]).0;
    }
    panic!("no Captions row in {:?}", s.player_menu_rows());
}

#[test]
fn the_row_turns_captions_on_for_a_video_or_a_clip_and_saves_with_no_delay() {
    for s in [watching(false, MIN), watching_clip(false)] {
        let s = on_row(&s);
        assert_eq!(s.captions_row(), CaptionsRow::Off);
        let (on, cmds) = reduce(&s, Intent::Right);
        assert!(on.settings.captions);
        assert_eq!(on.captions_row(), CaptionsRow::On);
        // No `SetLiveDelay`: a video is transcribed ahead of the playhead instead.
        assert_eq!(
            cmds,
            [
                Command::SetCaptions(true),
                Command::SaveSettings(on.settings)
            ]
        );
        assert!(on.captions_live());
        assert!(!on.caption_delay_wanted());
        assert!(on.settings.caption_delay, "the live setting is untouched");
        let (off, cmds) = reduce(&on, Intent::Left);
        assert_eq!(
            cmds,
            [
                Command::SetCaptions(false),
                Command::SaveSettings(off.settings)
            ]
        );
    }
}

#[test]
fn the_quality_row_works_in_a_videos_menu() {
    let s = on_row(&watching(true, MIN));
    let (s, _) = press(&s, &[Intent::Down]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::CaptionsQuality));
    let (fast, cmds) = reduce(&s, Intent::Right);
    assert_eq!(fast.settings.captions_quality, CaptionsQuality::Fast);
    assert_eq!(
        cmds,
        [
            Command::SetCaptionsQuality(CaptionsQuality::Fast),
            Command::SetCaptions(true),
            Command::SaveSettings(fast.settings)
        ]
    );
}

#[test]
fn where_captions_cannot_run_a_video_says_why_like_a_stream() {
    let mut audio = watching(true, MIN);
    audio.audio_only = true;
    assert_eq!(audio.captions_row(), CaptionsRow::NotWithSoundOnly);
    assert!(!audio.captions_live());
    let mut none = watching(true, MIN);
    none.captions_unavailable = true;
    assert_eq!(none.captions_row(), CaptionsRow::Unavailable);
    assert!(!none.captions_live());
}

// ---- The queue and the clock -----------------------------------------------------------------

#[test]
fn captions_off_show_nothing_and_ignore_what_arrives() {
    let s = at(&watching(false, MIN), MIN).0;
    assert!(!s.captions_live());
    let s = timed(&s, &[(MIN + START + 1_000, "no")]).0;
    assert!(is_empty(&s));
}

#[test]
fn words_30_s_ahead_wait_for_the_videos_clock_then_come_on_word_by_word() {
    let s = at(&watching(true, MIN), MIN).0;
    assert_eq!(s.captions.clock_ms, Some(MIN + START));
    let now = MIN + START;
    // A caption for 25 to 28 s ahead of the playhead: nothing yet.
    let (s, cmds) = timed(
        &s,
        &[
            (now + 25_000, "hello"),
            (now + 25_600, "there,"),
            (now + 27_000, "friends."),
        ],
    );
    assert_eq!(queued(&s), ["hello", "there,", "friends."]);
    assert!(shown(&s).is_empty());
    assert!(tick_in(&cmds).is_some(), "ticks count the clock on");
    // The playhead gets there (rebased position + the video's start time).
    let s = at(&s, MIN + 24_999).0;
    assert!(shown(&s).is_empty());
    let s = at(&s, MIN + 25_000).0;
    assert_eq!(shown(&s), ["hello"]);
    let s = at(&s, MIN + 25_600).0;
    assert_eq!(shown(&s), ["hello there,"]);
    let s = at(&s, MIN + 27_000).0;
    assert_eq!(shown(&s), ["hello there, friends."]);
    // After a pause in the speech the lines clear, as on a stream.
    let s = at(&s, MIN + 27_000 + CLEAR_AFTER_MS).0;
    assert!(shown(&s).is_empty());
}

#[test]
fn the_queue_holds_a_whole_lookahead_of_a_fast_talker() {
    let s = at(&watching(true, MIN), MIN).0;
    let now = MIN + START;
    // 30 s at 5 words a second, in captions of 20 words.
    let mut s = s;
    for caption in 0..8u64 {
        let words: Vec<(u64, String)> = (0..20u64)
            .map(|w| {
                (
                    now + 1_000 + caption * 4_000 + w * 200,
                    format!("w{caption}_{w}"),
                )
            })
            .collect();
        let words: Vec<(u64, &str)> = words.iter().map(|(t, w)| (*t, w.as_str())).collect();
        s = timed(&s, &words).0;
    }
    assert_eq!(s.captions.queue.len(), 160);
    assert!(s.captions.queue.len() < MAX_QUEUED);
    assert_eq!(
        queued(&s)[0],
        "w0_0",
        "nothing from the near end was dropped"
    );
}

#[test]
fn ticks_move_a_videos_clock_at_its_speed() {
    for (milli, step) in [
        (1_000u32, 250u64),
        (750, 188),
        (1_250, 313),
        (1_500, 375),
        (2_000, 500),
    ] {
        let mut s = at(&watching(true, MIN), MIN).0;
        s.video.as_mut().expect("a video").speed = milli;
        let now = MIN + START;
        let (s, cmds) = timed(&s, &[(now + 60_000, "later")]);
        let id = tick_in(&cmds).expect("a tick");
        let (s, _) = reduce(&s, Event::CaptionTickDue(id));
        assert_eq!(s.captions.clock_ms, Some(now + step), "at {milli}/1000");
    }
}

#[test]
fn a_paused_video_and_a_scrub_stand_still() {
    let s = at(&watching(true, MIN), MIN).0;
    let now = MIN + START;
    // Paused (A): the ticks stop with it.
    let (paused, _) = reduce(&s, Intent::Select);
    assert!(paused.paused);
    let (paused, cmds) = timed(&paused, &[(now + 5_000, "x")]);
    assert!(tick_in(&cmds).is_none());
    assert_eq!(paused.captions.clock_ms, Some(now));

    // A hold on Right scrubs: the picture is held, and so is the clock, though ticks come.
    let (held, cmds) = reduce(&timed(&s, &[(now + 5_000, "x")]).0, Intent::Right);
    let hold = cmds
        .iter()
        .find_map(|c| match c {
            Command::HoldCheck { id, .. } => Some(*id),
            _ => None,
        })
        .expect("a hold check");
    let (scrubbing, _) = reduce(&held, Event::HoldDue(hold));
    assert!(scrubbing.video_scrubbing());
    let id = scrubbing.captions.last_tick;
    let (after, _) = reduce(&scrubbing, Event::CaptionTickDue(id));
    assert_eq!(after.captions.clock_ms, Some(now), "the clock stood still");
}

// ---- Seeks and loads -------------------------------------------------------------------------

#[test]
fn a_seek_clears_the_queue_and_the_clock_until_the_playhead_lands() {
    let s = at(&watching(true, MIN), MIN).0;
    let now = MIN + START;
    let (s, _) = timed(&s, &[(now, "now"), (now + 20_000, "later")]);
    assert_eq!(shown(&s), ["now"]);
    assert_eq!(queued(&s), ["later"]);

    // RB: to the next chapter, 21:00.
    let (jumped, cmds) = reduce(&s, Intent::PageRight);
    assert_eq!(seeks(&cmds), [21 * MIN]);
    assert!(
        is_empty(&jumped),
        "the old place's words and clock are gone"
    );
    assert!(jumped.captions_live(), "captions stay on");

    // A report from before the jump is still on its way: it is not the clock.
    let (stale, _) = at(&jumped, MIN + 3_000);
    assert_eq!(stale.captions.clock_ms, None);
    // Words from the new place (the tap restarted there) wait for the clock.
    let there = 21 * MIN + START;
    let (waiting, _) = timed(&stale, &[(there + 4_000, "new")]);
    assert_eq!(queued(&waiting), ["new"]);
    // The first report that has landed sets it, and the words follow it.
    let (landed, _) = at(&waiting, 21 * MIN + 500);
    assert_eq!(landed.captions.clock_ms, Some(there + 500));
    let (reached, _) = at(&landed, 21 * MIN + 4_000);
    assert_eq!(shown(&reached), ["new"]);
}

#[test]
fn a_jump_back_cannot_show_words_of_the_place_it_left() {
    let s = at(&watching(true, 30 * MIN), 30 * MIN).0;
    let now = 30 * MIN + START;
    let s = timed(&s, &[(now + 10_000, "ahead")]).0;
    // LB: a chapter back.
    let (back, cmds) = reduce(&s, Intent::PageLeft);
    assert_eq!(seeks(&cmds), [21 * MIN]);
    assert!(is_empty(&back));
    // The old place's report: the clock would otherwise be 9 minutes ahead, and everything the
    // new tap made would show at once.
    let (stale, _) = at(&back, 30 * MIN + 1_000);
    assert_eq!(stale.captions.clock_ms, None);
}

#[test]
fn start_over_or_a_new_quality_clears_them_too() {
    let s = at(&watching(true, MIN), MIN).0;
    let now = MIN + START;
    let (s, _) = timed(&s, &[(now, "now"), (now + 9_000, "later")]);
    assert!(!is_empty(&s));
    // Start over, in the menu.
    let (menu, _) = press(
        &s,
        &[Intent::Menu, Intent::Down, Intent::Down, Intent::Down],
    );
    assert_eq!(menu.player_menu, Some(PlayerMenuRow::StartOver));
    let (over, cmds) = reduce(&menu, Intent::Select);
    assert_eq!(seeks(&cmds), [0]);
    assert!(is_empty(&over));
    // A new quality reloads the video where it is.
    let (menu, _) = press(&s, &[Intent::Menu]);
    let (reloaded, cmds) = reduce(&menu, Intent::Right);
    assert!(cmds.iter().any(|c| matches!(c, Command::PlayVideo { .. })));
    assert!(is_empty(&reloaded));
}

// ---- The end, and what stays live-only -------------------------------------------------------

#[test]
fn the_end_card_and_leaving_clear_them() {
    let s = at(&watching(true, MIN), MIN).0;
    let now = MIN + START;
    let (s, _) = timed(&s, &[(now, "now")]);
    assert_eq!(shown(&s), ["now"]);
    let (ended, _) = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT });
    assert!(ended.video.as_ref().is_some_and(|v| v.end_card.is_some()));
    assert!(!ended.captions_live());
    assert!(is_empty(&ended));
    // Captions that arrive meanwhile are ignored.
    let (late, _) = timed(&ended, &[(now + 500, "late")]);
    assert!(is_empty(&late));

    let (left, _) = press(&s, &[Intent::Back]);
    assert_ne!(left.screen, Screen::Player);
    assert!(is_empty(&left));
}

#[test]
fn the_delay_and_the_held_chat_are_for_live_streams_only() {
    let s = watching(true, MIN);
    assert!(s.settings.caption_delay);
    assert!(!s.caption_delay_wanted());
    assert_eq!(s.chat_due_ms(), None);
    assert!(!s.captions.delaying);
    // No message of a video's captions asks the player to hold the picture back.
    let (s, cmds) = at(&s, MIN + 1_000);
    assert!(!cmds.contains(&Command::SetLiveDelay(Some(captions::DELAY_BUFFER_MS))));
    let (s, cmds) = timed(&s, &[(MIN + START + 5_000, "hi")]);
    assert!(!cmds.iter().any(|c| matches!(c, Command::SetLiveDelay(_))));
    assert_eq!(s.chat_due_ms(), None);
}
