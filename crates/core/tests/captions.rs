//! Generated captions on the live player (captions-plan.md, phases 3 and 3b): the menu rows, a
//! queue of words that come on as the player's stream clock gets to them and roll up two lines,
//! the delay that holds the picture back for them, and chat held back with it. The reducer has
//! no clock, so these tests play the part of the app: they send the player's health reports and
//! the ticks it asks for.

use htpc_core::captions::{
    CLEAR_AFTER_MS, DELAY_BUFFER_MS, LINE_CHARS, MAX_LINES, MAX_QUEUED, NORMAL_BUFFER_MS,
    SHOW_AFTER_MS, TICK_MS,
};
use htpc_core::*;

fn stream(channel: ChannelRef) -> Tile {
    Tile::Stream(Stream {
        user_id: "77".into(),
        title: format!("{} title", channel.login),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
        channel,
    })
}

/// Home with a Twitch stream, a Kick stream and another Twitch one, signed in. Words come on at
/// their start (no `SHOW_AFTER_MS`), so the timing tests read in Whisper's own times;
/// `words_come_on_half_a_second_after_their_start_by_default` covers the offset.
fn home() -> AppState {
    let (mut s, _) = init();
    s.captions.show_after = Some(0);
    let rows = vec![Row::new(
        RowKind::TopLive,
        vec![
            stream(ChannelRef::twitch("chan")),
            stream(ChannelRef::kick("kicker")),
            stream(ChannelRef::twitch("other")),
        ],
    )];
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0;
    let s = reduce(
        &s,
        Event::SignedIn(User {
            login: "viewer".into(),
            can_chat: true,
        }),
    )
    .0;
    // The followed channels multiview's picker lists.
    let following = vec![Row::new(
        RowKind::FollowedLive,
        vec![stream(ChannelRef::twitch("friend"))],
    )];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Following,
            rows: following,
        },
    )
    .0
}

/// `chan` playing, with captions on or off in the settings.
fn playing(on: bool) -> AppState {
    let mut s = home();
    s.settings.captions = on;
    let (s, _) = reduce(&s, Intent::Select);
    reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0
}

/// `chan` playing with captions on and the delay off: the timing tests leave chat alone.
fn captioned() -> AppState {
    let mut s = home();
    s.settings.captions = true;
    s.settings.caption_delay = false;
    let (s, _) = reduce(&s, Intent::Select);
    reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0
}

/// The Kick stream, playing.
fn kick_playing() -> AppState {
    let mut s = home();
    s.settings.captions = true;
    let (s, _) = reduce(&s, Intent::Right);
    let (s, _) = reduce(&s, Intent::Select);
    reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0
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

/// The menu open on the *Captions* row.
fn on_row(s: &AppState) -> AppState {
    let (s, _) = press(s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 10]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Captions));
    s
}

fn report(slot: Slot, stream_ms: Option<u64>, buffer_ms: u32) -> Event {
    Event::PlayerHealth {
        slot,
        at_ms: 0,
        health: Health {
            stalled: false,
            buffer_ms,
            pos_ms: None,
            length_ms: None,
            stream_ms,
        },
    }
}

fn health(slot: Slot, stream_ms: Option<u64>) -> Event {
    report(slot, stream_ms, 4_000)
}

/// The player's report that the stream is at `ms`.
fn at(s: &AppState, ms: u64) -> (AppState, Vec<Command>) {
    reduce(s, health(MAIN_SLOT, Some(ms)))
}

/// A caption whose words are spread over it.
fn say(s: &AppState, start_ms: u64, end_ms: u64, text: &str) -> (AppState, Vec<Command>) {
    reduce(
        s,
        Event::Caption {
            start_ms,
            end_ms,
            text: text.into(),
            words: Vec::new(),
        },
    )
}

/// A caption with its words' own times.
fn timed(s: &AppState, start_ms: u64, end_ms: u64, words: &[(u64, &str)]) -> AppState {
    let text: Vec<&str> = words.iter().map(|(_, w)| *w).collect();
    reduce(
        s,
        Event::Caption {
            start_ms,
            end_ms,
            text: text.join(" "),
            words: words
                .iter()
                .map(|(ms, w)| CaptionWord::new(*ms, *w))
                .collect(),
        },
    )
    .0
}

fn shown(s: &AppState) -> Vec<String> {
    s.captions.shown.clone()
}

fn queued(s: &AppState) -> Vec<String> {
    s.captions.queue.iter().map(|w| w.text.clone()).collect()
}

/// The id of the tick asked for in `cmds`, if there is one.
fn tick_in(cmds: &[Command]) -> Option<u32> {
    cmds.iter().find_map(|c| match c {
        Command::CaptionTick { id, .. } => Some(*id),
        _ => None,
    })
}

fn is_empty(s: &AppState) -> bool {
    s.captions.queue.is_empty() && s.captions.shown.is_empty() && s.captions.clock_ms.is_none()
}

const DELAY_ON: Command = Command::SetLiveDelay(Some(DELAY_BUFFER_MS));
const DELAY_OFF: Command = Command::SetLiveDelay(None);

// ---- The setting and the menu row ------------------------------------------------------------

#[test]
fn captions_start_off() {
    assert!(!playing(false).settings.captions);
    assert!(!AppState::default().settings.captions);
    assert!(!AppState::default().captions_unavailable);
}

#[test]
fn the_rows_end_a_live_streams_menu_and_a_video_has_all_but_the_delay() {
    let all = PlayerMenuRow::ALL;
    assert_eq!(
        all[all.len() - 3..],
        [
            PlayerMenuRow::Captions,
            PlayerMenuRow::CaptionsQuality,
            PlayerMenuRow::CaptionDelay
        ]
    );
    for row in [
        PlayerMenuRow::Captions,
        PlayerMenuRow::CaptionsQuality,
        PlayerMenuRow::CaptionDelay,
    ] {
        // The delay is live only (a video is transcribed ahead of the playhead instead): see
        // `captions_video.rs` for a video's rows.
        let live_only = row == PlayerMenuRow::CaptionDelay;
        assert_eq!(!PlayerMenuRow::VIDEO.contains(&row), live_only);
        assert_eq!(!PlayerMenuRow::CLIP.contains(&row), live_only);
        assert!(!row.is_action());
    }
}

#[test]
fn the_row_turns_captions_on_and_off_and_saves() {
    let s = on_row(&playing(false));
    assert_eq!(s.captions_row(), CaptionsRow::Off);

    // On: the picture is held back for them too (the delay is on by default), first.
    let (on, cmds) = reduce(&s, Intent::Right);
    assert!(on.settings.captions);
    assert_eq!(on.captions_row(), CaptionsRow::On);
    assert_eq!(
        cmds,
        [
            DELAY_ON,
            Command::SetCaptions(true),
            Command::SaveSettings(on.settings)
        ]
    );

    // Both directions and A toggle.
    let (off, cmds) = reduce(&on, Intent::Left);
    assert!(!off.settings.captions);
    assert_eq!(
        cmds,
        [
            DELAY_OFF,
            Command::SetCaptions(false),
            Command::SaveSettings(off.settings)
        ]
    );
    let (again, _) = reduce(&off, Intent::Select);
    assert!(again.settings.captions);
    assert_eq!(again.screen, Screen::Player, "the stream plays on");
}

#[test]
fn the_row_does_nothing_where_captions_cannot_run() {
    // Kick.
    let kick = on_row(&kick_playing());
    assert_eq!(kick.captions_row(), CaptionsRow::TwitchOnly);
    // Sound only.
    let mut audio = on_row(&playing(false));
    audio.audio_only = true;
    assert_eq!(audio.captions_row(), CaptionsRow::NotWithSoundOnly);
    // No engine or no models.
    let mut none = on_row(&playing(false));
    none.captions_unavailable = true;
    assert_eq!(none.captions_row(), CaptionsRow::Unavailable);
    // Multiview.
    let (mut mv, _) = press(&playing(false), &[Intent::View, Intent::Select]);
    assert!(mv.multiview.is_some());
    assert_eq!(mv.captions_row(), CaptionsRow::NotInMultiview);
    mv.player_menu = Some(PlayerMenuRow::Captions);
    mv.screen = Screen::Player;

    for s in [kick, audio, none, mv] {
        for intent in [Intent::Right, Intent::Left, Intent::Select] {
            let (next, cmds) = reduce(&s, intent);
            assert_eq!(next.settings, s.settings, "{:?}", s.captions_row());
            assert!(cmds.is_empty(), "{:?}: {cmds:?}", s.captions_row());
        }
    }
}

#[test]
fn a_saved_on_setting_stays_on_where_it_cannot_run_but_shows_nothing() {
    let mut s = kick_playing();
    assert!(s.settings.captions);
    s = at(&s, 10_000).0;
    s = say(&s, 10_000, 12_000, "hello").0;
    assert!(is_empty(&s), "Kick gets no captions");
    assert!(!s.captions.delaying, "nor a delay");
}

// ---- The model download and the quality ------------------------------------------------------

/// The menu open on the *Caption quality* row.
fn on_quality_row(s: &AppState) -> AppState {
    let s = on_row(s);
    let (s, _) = reduce(&s, Intent::Down);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::CaptionsQuality));
    s
}

#[test]
fn the_row_follows_the_model_download() {
    let mut s = on_row(&playing(true));
    assert_eq!(s.captions_row(), CaptionsRow::On);

    s = reduce(&s, Event::CaptionsModel(ModelState::Downloading(34))).0;
    assert_eq!(s.captions_row(), CaptionsRow::Downloading(34));
    s = reduce(&s, Event::CaptionsModel(ModelState::Failed)).0;
    assert_eq!(s.captions_row(), CaptionsRow::DownloadFailed);
    s = reduce(&s, Event::CaptionsModel(ModelState::Ready)).0;
    assert_eq!(s.captions_row(), CaptionsRow::On);
}

#[test]
fn nothing_shows_while_the_model_downloads() {
    let mut s = playing(true);
    s = at(&s, 10_000).0;
    let (next, cmds) = reduce(&s, Event::CaptionsModel(ModelState::Downloading(5)));
    assert_eq!(cmds, [DELAY_OFF], "no captions to wait for yet");
    s = say(&next, 10_000, 12_000, "hello").0;
    assert!(is_empty(&s), "captions are not live yet");
    let (_, cmds) = reduce(&s, Event::CaptionsModel(ModelState::Ready));
    assert_eq!(cmds, [DELAY_ON], "and back once the model is there");
}

#[test]
fn a_press_while_it_downloads_cancels_by_turning_captions_off() {
    let mut s = on_row(&playing(false));
    s = reduce(&s, Intent::Right).0;
    s = reduce(&s, Event::CaptionsModel(ModelState::Downloading(10))).0;
    let (off, cmds) = reduce(&s, Intent::Select);
    assert!(!off.settings.captions);
    assert_eq!(off.captions_row(), CaptionsRow::Off);
    assert_eq!(
        cmds,
        [
            Command::SetCaptions(false),
            Command::SaveSettings(off.settings)
        ]
    );
    // The model's last words from the cancelled download change nothing.
    let late = reduce(&off, Event::CaptionsModel(ModelState::Downloading(11))).0;
    assert_eq!(late.captions_row(), CaptionsRow::Off);
    let on = reduce(&late, Intent::Select).0;
    assert_eq!(
        on.captions_row(),
        CaptionsRow::On,
        "a new start is not left failed"
    );
}

#[test]
fn a_failed_download_is_tried_again_with_a() {
    let mut s = on_row(&playing(true));
    s = reduce(&s, Event::CaptionsModel(ModelState::Failed)).0;
    for intent in [Intent::Select, Intent::Right] {
        let (retry, cmds) = reduce(&s, intent);
        assert!(retry.settings.captions, "still on");
        assert_eq!(retry.captions_row(), CaptionsRow::Downloading(0));
        assert_eq!(cmds, [Command::SetCaptions(true)], "nothing to save");
    }
}

#[test]
fn the_quality_row_cycles_and_saves_without_a_gpu() {
    let s = on_quality_row(&playing(false));
    assert_eq!(s.captions_quality(), CaptionsQuality::Standard);
    assert_eq!(
        s.captions_qualities(),
        [CaptionsQuality::Fast, CaptionsQuality::Standard]
    );

    // Captions off: the app is told, nothing is loaded.
    let (fast, cmds) = reduce(&s, Intent::Right);
    assert_eq!(fast.settings.captions_quality, CaptionsQuality::Fast);
    assert_eq!(
        cmds,
        [
            Command::SetCaptionsQuality(CaptionsQuality::Fast),
            Command::SaveSettings(fast.settings)
        ]
    );
    // Best is not offered: it wraps back to Standard, both ways.
    let (back, _) = reduce(&fast, Intent::Right);
    assert_eq!(back.settings.captions_quality, CaptionsQuality::Standard);
    let (left, _) = reduce(&fast, Intent::Left);
    assert_eq!(left.settings.captions_quality, CaptionsQuality::Standard);
}

#[test]
fn best_is_offered_with_a_gpu_build() {
    let mut s = on_quality_row(&playing(false));
    s.captions_gpu = true;
    let (best, _) = reduce(&s, Intent::Right);
    assert_eq!(best.settings.captions_quality, CaptionsQuality::Best);
    let (wrapped, _) = reduce(&best, Intent::Right);
    assert_eq!(wrapped.settings.captions_quality, CaptionsQuality::Fast);
}

#[test]
fn a_saved_best_without_a_gpu_acts_as_standard() {
    let mut s = on_quality_row(&playing(false));
    s.settings.captions_quality = CaptionsQuality::Best;
    assert_eq!(s.captions_quality(), CaptionsQuality::Standard);
    // The next press goes on from what is shown.
    let (next, _) = reduce(&s, Intent::Right);
    assert_eq!(next.settings.captions_quality, CaptionsQuality::Fast);
}

#[test]
fn a_new_quality_with_captions_on_reloads_the_engine() {
    let mut s = on_quality_row(&playing(true));
    s = reduce(&s, Event::CaptionsModel(ModelState::Failed)).0;
    let (fast, cmds) = reduce(&s, Intent::Right);
    assert_eq!(
        cmds,
        [
            DELAY_ON,
            Command::SetCaptionsQuality(CaptionsQuality::Fast),
            Command::SetCaptions(true),
            Command::SaveSettings(fast.settings)
        ],
        "the quality first, so the load sees it"
    );
    assert_eq!(
        fast.captions_row(),
        CaptionsRow::On,
        "a failure of the old one is forgotten"
    );
}

#[test]
fn the_quality_row_does_nothing_without_an_engine() {
    let mut s = on_quality_row(&playing(false));
    s.captions_unavailable = true;
    for intent in [Intent::Right, Intent::Left] {
        let (next, cmds) = reduce(&s, intent);
        assert_eq!(next.settings, s.settings);
        assert!(cmds.is_empty());
    }
}

// ---- Word by word ------------------------------------------------------------------------------

#[test]
fn nothing_shows_before_the_player_reports_where_it_is() {
    let s = captioned();
    let (s, cmds) = say(&s, 1_000, 3_000, "early");
    assert_eq!(queued(&s), ["early"]);
    assert!(shown(&s).is_empty());
    assert_eq!(tick_in(&cmds), None, "no clock, nothing to tick");
    // A report without the stream clock does not give one either.
    let (s, _) = reduce(&s, health(MAIN_SLOT, None));
    assert_eq!(s.captions.clock_ms, None);
    // The first report with it brings the word out.
    let (s, _) = at(&s, 1_000);
    assert_eq!(shown(&s), ["early"]);
}

#[test]
fn each_word_comes_on_when_the_stream_reaches_it() {
    let s = at(&captioned(), 10_000).0;
    let s = timed(
        &s,
        11_000,
        13_000,
        &[(11_000, "hello"), (11_600, "there,"), (12_400, "friends.")],
    );
    assert_eq!(queued(&s), ["hello", "there,", "friends."]);
    assert!(shown(&s).is_empty());
    let s = at(&s, 10_999).0;
    assert!(shown(&s).is_empty());
    let s = at(&s, 11_000).0;
    assert_eq!(shown(&s), ["hello"]);
    let s = at(&s, 11_599).0;
    assert_eq!(shown(&s), ["hello"]);
    let s = at(&s, 11_600).0;
    assert_eq!(shown(&s), ["hello there,"]);
    let s = at(&s, 12_400).0;
    assert_eq!(shown(&s), ["hello there, friends."]);
    assert!(queued(&s).is_empty());
}

#[test]
fn words_come_on_half_a_second_after_their_start_by_default() {
    let mut s = at(&captioned(), 10_000).0;
    s.captions.show_after = None;
    let s = timed(&s, 11_000, 12_000, &[(11_000, "hello"), (11_400, "there")]);
    let s = at(&s, 11_000 + SHOW_AFTER_MS - 1).0;
    assert!(shown(&s).is_empty(), "not yet at its start");
    let s = at(&s, 11_000 + SHOW_AFTER_MS).0;
    assert_eq!(shown(&s), ["hello"]);
    let s = at(&s, 11_400 + SHOW_AFTER_MS).0;
    assert_eq!(shown(&s), ["hello there"]);
}

#[test]
fn words_without_times_are_spread_over_the_caption() {
    let s = at(&captioned(), 0).0;
    let (s, _) = say(&s, 10_000, 18_000, "aaaa bbbb cccccccc");
    let starts: Vec<u64> = s.captions.queue.iter().map(|w| w.start_ms).collect();
    assert_eq!(starts, [10_000, 12_000, 14_000]);
}

#[test]
fn a_full_line_starts_another_and_a_third_scrolls_the_oldest_off() {
    let s = at(&captioned(), 0).0;
    let forty = "a".repeat(40);
    let s = timed(
        &s,
        1_000,
        9_000,
        &[(1_000, &forty), (2_000, "bbb"), (3_000, "cc")],
    );
    let s = at(&s, 2_000).0;
    assert_eq!(shown(&s), [format!("{forty} bbb")], "44 characters fit");
    let s = at(&s, 3_000).0;
    assert_eq!(shown(&s), [format!("{forty} bbb"), "cc".to_owned()]);
    let s = timed(&s, 4_000, 9_000, &[(4_000, &"d".repeat(LINE_CHARS))]);
    let s = at(&s, 4_000).0;
    assert_eq!(shown(&s), ["cc".to_owned(), "d".repeat(LINE_CHARS)]);
    assert_eq!(s.captions.shown.len(), MAX_LINES);
}

#[test]
fn the_lines_clear_after_a_pause_in_the_speech() {
    let s = at(&captioned(), 10_000).0;
    let s = timed(&s, 10_000, 11_000, &[(10_000, "one"), (10_500, "two")]);
    let s = at(&s, 10_500).0;
    assert_eq!(shown(&s), ["one two"]);
    let s = at(&s, 10_500 + CLEAR_AFTER_MS - 1).0;
    assert_eq!(shown(&s), ["one two"]);
    let (s, cmds) = at(&s, 10_500 + CLEAR_AFTER_MS);
    assert!(shown(&s).is_empty());
    assert_eq!(tick_in(&cmds), None, "nothing left to wait for");
    // The next words start a fresh line.
    let s = timed(&s, 14_000, 15_000, &[(14_000, "three")]);
    let s = at(&s, 14_000).0;
    assert_eq!(shown(&s), ["three"]);
}

#[test]
fn ticks_carry_the_clock_between_reports() {
    let s = at(&captioned(), 10_000).0;
    let (mut s, cmds) = say(&s, 11_000, 12_000, "tick");
    let mut id = tick_in(&cmds).expect("a tick is asked for");
    assert!(cmds.contains(&Command::CaptionTick { id, ms: TICK_MS }));

    // Four ticks of 250 ms: 11.0 s, and the word is up.
    for n in 1..=4u64 {
        let (next, cmds) = reduce(&s, Event::CaptionTickDue(id));
        s = next;
        assert_eq!(s.captions.clock_ms, Some(10_000 + n * TICK_MS));
        id = tick_in(&cmds).expect("and the next one");
    }
    assert_eq!(shown(&s), ["tick"]);

    // A report corrects the clock (here: the stream was 100 ms ahead of the ticks).
    let (s, _) = at(&s, 11_100);
    assert_eq!(s.captions.clock_ms, Some(11_100));
}

#[test]
fn a_tick_asks_for_the_next_only_while_there_is_something_to_do() {
    let s = at(&captioned(), 10_000).0;
    let (s, cmds) = say(&s, 10_000, 10_100, "short");
    // It showed at once, so the tick keeps it company until it clears.
    assert_eq!(shown(&s), ["short"]);
    let mut id = tick_in(&cmds).unwrap();
    let mut s = s;
    let mut ticks = 0;
    loop {
        let (next, cmds) = reduce(&s, Event::CaptionTickDue(id));
        s = next;
        ticks += 1;
        match tick_in(&cmds) {
            Some(n) => id = n,
            None => break,
        }
        assert!(ticks < 40, "ticks never stop");
    }
    assert!(shown(&s).is_empty());
    // 3 s with no new word: gone at the 12th tick of 250 ms.
    assert_eq!(ticks, 12);
}

#[test]
fn an_old_tick_is_ignored() {
    let s = at(&captioned(), 10_000).0;
    let (s, cmds) = say(&s, 20_000, 21_000, "later");
    let id = tick_in(&cmds).unwrap();
    let (same, cmds) = reduce(&s, Event::CaptionTickDue(id + 7));
    assert_eq!(same, s);
    assert!(cmds.is_empty());
    // A second caption does not start a second tick chain.
    let (_, cmds) = say(&s, 22_000, 23_000, "and later");
    assert_eq!(tick_in(&cmds), None);
}

#[test]
fn a_paused_stream_stands_still() {
    let s = at(&captioned(), 10_000).0;
    let (s, cmds) = say(&s, 11_000, 12_000, "waiting");
    let id = tick_in(&cmds).unwrap();
    let (paused, _) = reduce(&s, Intent::Select);
    assert!(paused.paused);
    let (after, cmds) = reduce(&paused, Event::CaptionTickDue(id));
    assert_eq!(
        after.captions.clock_ms,
        Some(10_000),
        "the clock did not move"
    );
    assert_eq!(
        tick_in(&cmds),
        None,
        "and no more ticks until it plays again"
    );
    // Playing again: the next report starts the ticks.
    let (resumed, _) = reduce(&after, Intent::Select);
    let (_, cmds) = at(&resumed, 10_000);
    assert!(tick_in(&cmds).is_some());
}

#[test]
fn a_report_from_a_tile_or_another_slot_is_not_the_clock() {
    let s = captioned();
    let (s, _) = reduce(&s, health(3, Some(50_000)));
    assert_eq!(s.captions.clock_ms, None);
}

#[test]
fn a_jump_over_words_skips_the_ones_missed() {
    let s = at(&captioned(), 0).0;
    let s = timed(&s, 1_000, 2_000, &[(1_000, "missed")]);
    let s = timed(&s, 9_000, 12_000, &[(9_000, "kept")]);
    let s = at(&s, 10_000).0;
    assert_eq!(shown(&s), ["kept"]);
    assert!(queued(&s).is_empty());
}

#[test]
fn a_late_caption_shows_at_once_and_one_long_over_is_dropped() {
    let s = at(&captioned(), 30_000).0;
    let (s, cmds) = say(&s, 20_000, 22_000, "stale");
    assert!(queued(&s).is_empty() && shown(&s).is_empty());
    assert_eq!(tick_in(&cmds), None);
    // Late, but its speech ended under 3 s ago: every word at once, in order, even the first
    // ones from more than 3 s ago.
    let s = timed(
        &s,
        26_000,
        28_000,
        &[(26_000, "just"), (26_500, "too"), (27_900, "late")],
    );
    assert_eq!(shown(&s), ["just too late"]);
}

#[test]
fn the_queue_keeps_the_newest_words() {
    let mut s = at(&captioned(), 0).0;
    for i in 0..(MAX_QUEUED as u64 + 30) {
        s = timed(
            &s,
            10_000 + i * 100,
            10_000 + i * 100,
            &[(10_000 + i * 100, "w")],
        );
    }
    assert_eq!(s.captions.queue.len(), MAX_QUEUED);
    assert_eq!(
        s.captions.queue.front().map(|w| w.start_ms),
        Some(10_000 + 30 * 100)
    );
}

#[test]
fn captions_arriving_out_of_order_are_queued_by_time() {
    let s = at(&captioned(), 0).0;
    let s = say(&s, 8_000, 9_000, "second").0;
    let s = say(&s, 4_000, 5_000, "first").0;
    assert_eq!(queued(&s), ["first", "second"]);
}

// ---- Clearing ----------------------------------------------------------------------------------

/// Words showing and waiting, and a tick on its way.
fn busy() -> AppState {
    let s = at(&captioned(), 10_000).0;
    let s = say(&s, 10_000, 12_000, "showing").0;
    let s = say(&s, 20_000, 22_000, "waiting").0;
    assert_eq!(shown(&s), ["showing"]);
    assert_eq!(queued(&s), ["waiting"]);
    s
}

#[test]
fn a_load_clears_them() {
    // Another quality: the stream loads again.
    let s = on_row(&busy());
    let (s, _) = press(&s, &[Intent::Up; 10]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Quality));
    let (s, cmds) = reduce(&s, Intent::Right);
    assert!(cmds.iter().any(|c| matches!(
        c,
        Command::Play {
            slot: MAIN_SLOT,
            ..
        }
    )));
    assert!(is_empty(&s), "{:?}", s.captions);
}

#[test]
fn a_retry_after_a_failure_clears_them() {
    let s = reduce(
        &busy(),
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "x".into(),
        },
    )
    .0;
    assert!(is_empty(&s), "a failed stream has none: {:?}", s.captions);
}

#[test]
fn the_stream_ending_clears_them() {
    let s = reduce(&busy(), Event::PlaybackEnded { slot: MAIN_SLOT }).0;
    assert_eq!(s.playback, Playback::Offline);
    assert!(is_empty(&s));
    // A late caption for the stream that ended is not kept.
    let s = say(&s, 30_000, 31_000, "late").0;
    assert!(is_empty(&s));
}

#[test]
fn a_new_channel_clears_them() {
    let s = busy();
    let (s, _) = reduce(
        &s,
        Event::Raided {
            from_login: "chan".into(),
            to_login: "raided".into(),
            to_name: "Raided".into(),
            viewers: 50,
        },
    );
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(cmds.iter().any(
        |c| matches!(c, Command::Play { slot: MAIN_SLOT, channel, .. } if channel.login == "raided")
    ));
    assert!(is_empty(&s), "{:?}", s.captions);
    // The new channel's clock is another one: its words wait for its first report.
    let s = say(&s, 100, 900, "raided hello").0;
    assert_eq!(queued(&s), ["raided", "hello"]);
    assert!(shown(&s).is_empty());
}

#[test]
fn leaving_the_player_clears_them() {
    let (s, cmds) = reduce(&busy(), Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopPlayback { slot: MAIN_SLOT }));
    assert!(is_empty(&s));
    // Nothing more is taken while away.
    let s = say(&s, 1, 2, "away").0;
    let s = at(&s, 10).0;
    assert!(is_empty(&s));
}

#[test]
fn turning_them_off_clears_them_and_later_captions_are_ignored() {
    let s = on_row(&busy());
    let (s, cmds) = reduce(&s, Intent::Right);
    assert!(!s.settings.captions);
    assert!(cmds.contains(&Command::SetCaptions(false)));
    assert!(is_empty(&s));
    let s = at(&s, 11_000).0;
    let s = say(&s, 11_000, 12_000, "ignored").0;
    assert!(is_empty(&s));
}

#[test]
fn sound_only_clears_them() {
    let s = on_row(&busy());
    let (s, _) = press(&s, &[Intent::Up; 5]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::AudioOnly));
    let (s, _) = reduce(&s, Intent::Right);
    assert!(s.audio_only);
    assert!(is_empty(&s));
}

#[test]
fn a_stale_tick_after_clearing_does_nothing() {
    let s = busy();
    // The tick that was on its way when the stream left.
    let id = s.captions.last_tick;
    let (s, _) = reduce(&s, Intent::Back);
    let (after, cmds) = reduce(&s, Event::CaptionTickDue(id));
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

#[test]
fn captions_are_shown_by_the_menu_and_hud_untouched() {
    // The stream keeps playing and the menu works as before with words on screen.
    let s = busy();
    let (s, _) = reduce(&s, Intent::Menu);
    assert_eq!(shown(&s), ["showing"]);
    assert_eq!(s.screen, Screen::Player);
}

// ---- The delay ---------------------------------------------------------------------------------

/// The menu open on *Delay video for caption sync*.
fn on_delay_row(s: &AppState) -> AppState {
    let (s, _) = press(s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 12]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::CaptionDelay));
    s
}

#[test]
fn the_delay_is_on_by_default_and_held_before_the_load() {
    assert!(Settings::default().caption_delay);
    let mut s = home();
    s.settings.captions = true;
    let (s, cmds) = reduce(&s, Intent::Select);
    let play = cmds
        .iter()
        .position(|c| matches!(c, Command::Play { .. }))
        .expect("plays");
    assert_eq!(cmds[0], DELAY_ON, "before the load, so it starts behind");
    assert!(play > 0);
    assert!(s.captions.delaying);
    // The stream after it keeps it: nothing new to say.
    let (s, cmds) = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT });
    assert!(!cmds.contains(&DELAY_ON));
    // Back home: the player stops, and the delay goes with the stream.
    let (_, cmds) = reduce(&s, Intent::Back);
    assert!(cmds.contains(&DELAY_OFF));
}

#[test]
fn the_delay_row_turns_it_off_and_on_and_saves() {
    let s = on_delay_row(&playing(true));
    assert!(s.captions.delaying);
    let (off, cmds) = reduce(&s, Intent::Right);
    assert!(!off.settings.caption_delay);
    assert_eq!(cmds, [DELAY_OFF, Command::SaveSettings(off.settings)]);
    let (on, cmds) = reduce(&off, Intent::Select);
    assert!(on.settings.caption_delay);
    assert_eq!(cmds, [DELAY_ON, Command::SaveSettings(on.settings)]);
    // With captions off it only saves: there is nothing to hold the picture for.
    let s = on_delay_row(&playing(false));
    let (off, cmds) = reduce(&s, Intent::Left);
    assert!(!off.settings.caption_delay);
    assert_eq!(cmds, [Command::SaveSettings(off.settings)]);
}

#[test]
fn no_delay_where_captions_do_not_run() {
    // Kick, sound only and multiview hold nothing back.
    assert!(!kick_playing().captions.delaying);
    let s = on_row(&playing(true));
    let (s, _) = press(&s, &[Intent::Up; 5]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::AudioOnly));
    let (audio, cmds) = reduce(&s, Intent::Right);
    assert!(audio.audio_only);
    assert_eq!(cmds[0], DELAY_OFF);
    let (mv, cmds) = press(&playing(true), &[Intent::View, Intent::Select]);
    assert!(mv.multiview.is_some());
    assert!(cmds.contains(&DELAY_OFF));
    // The channel page over the stream leaves it held back: coming back neither jumps nor waits.
    let mut covered = playing(true);
    covered.screen = Screen::Channel;
    assert!(covered.caption_delay_wanted());
}

#[test]
fn a_held_picture_stops_the_clock_until_it_goes_on() {
    let s = at(&playing(true), 10_000).0;
    let (s, cmds) = say(&s, 11_000, 12_000, "held");
    let id = tick_in(&cmds).unwrap();
    let (held, cmds) = reduce(
        &s,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: true,
        },
    );
    assert!(held.captions.held);
    assert_eq!(tick_in(&cmds), None, "one is on its way already");
    let (after, cmds) = reduce(&held, Event::CaptionTickDue(id));
    assert_eq!(after.captions.clock_ms, Some(10_000), "the picture stands");
    assert_eq!(after.captions.held_ms, TICK_MS, "and the hold is counted");
    assert_eq!(shown(&after), Vec::<String>::new());
    let id = tick_in(&cmds).expect("the ticks go on while held, to count it");
    // Another slot's hold is not this picture's.
    let (other, _) = reduce(
        &s,
        Event::PlayerHeld {
            slot: 2,
            held: true,
        },
    );
    assert!(!other.captions.held);
    // Going on: the clock moves with the ticks again, and the next report sets it right.
    let (on, _) = reduce(
        &after,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: false,
        },
    );
    assert!(!on.captions.held);
    let (on, cmds) = reduce(&on, Event::CaptionTickDue(id));
    assert_eq!(on.captions.clock_ms, Some(10_000 + TICK_MS));
    assert!(tick_in(&cmds).is_some());
    let (on, _) = at(&on, 10_400);
    assert_eq!(on.captions.held_ms, 0);
}

#[test]
fn a_hold_the_user_pauses_asks_for_no_more_ticks() {
    let s = at(&playing(true), 10_000).0;
    let (s, cmds) = reduce(
        &s,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: true,
        },
    );
    let id = tick_in(&cmds).unwrap();
    let (s, _) = press(&s, &[Intent::Select]);
    assert!(s.paused);
    let (s, cmds) = reduce(&s, Event::CaptionTickDue(id));
    assert_eq!(tick_in(&cmds), None);
    assert!(!s.captions.ticking);
}

// ---- Chat with the delay -----------------------------------------------------------------------

fn line(login: &str, text: &str) -> ChatMessage {
    ChatMessage {
        login: login.into(),
        ..ChatMessage::plain(login, text)
    }
}

fn chat(s: &AppState, lines: Vec<ChatMessage>) -> (AppState, Vec<Command>) {
    reduce(
        s,
        Event::ChatMessages {
            channel: ChannelRef::twitch("chan"),
            messages: lines,
        },
    )
}

fn chat_texts(s: &AppState) -> Vec<String> {
    s.chat.iter().map(ChatMessage::text).collect()
}

/// `chan` playing with captions and the delay on, its picture 8 s behind where it would be
/// (a buffer 8 s over a normal one), at 100 s on the stream clock.
fn delayed() -> AppState {
    let s = playing(true);
    reduce(
        &s,
        report(MAIN_SLOT, Some(100_000), NORMAL_BUFFER_MS + 8_000),
    )
    .0
}

#[test]
fn chat_waits_for_the_delayed_picture() {
    let s = delayed();
    let (s, cmds) = chat(&s, vec![line("a", "first"), line("b", "second")]);
    assert!(chat_texts(&s).is_empty(), "held back");
    assert_eq!(s.chat.held_len(), 2);
    assert!(tick_in(&cmds).is_some(), "ticks run while chat waits");
    let s = reduce(
        &s,
        report(MAIN_SLOT, Some(107_999), NORMAL_BUFFER_MS + 8_000),
    )
    .0;
    assert!(chat_texts(&s).is_empty());
    let s = reduce(
        &s,
        report(MAIN_SLOT, Some(108_000), NORMAL_BUFFER_MS + 8_000),
    )
    .0;
    assert_eq!(chat_texts(&s), ["first", "second"]);
    assert_eq!(s.chat.held_len(), 0);
}

#[test]
fn chat_goes_with_the_ticks_and_stands_while_the_picture_is_held() {
    let s = delayed();
    let (mut s, cmds) = chat(&s, vec![line("a", "soon")]);
    let mut id = tick_in(&cmds).unwrap();
    // Held: the clock stands, chat waits with it.
    s = reduce(
        &s,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: true,
        },
    )
    .0;
    let (after, cmds) = reduce(&s, Event::CaptionTickDue(id));
    assert!(chat_texts(&after).is_empty());
    assert_eq!(after.captions.clock_ms, Some(100_000));
    id = tick_in(&cmds).unwrap();
    // Going on: 32 ticks of 250 ms are the 8 s.
    let (mut s, _) = reduce(
        &after,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: false,
        },
    );
    for _ in 0..31 {
        let (next, cmds) = reduce(&s, Event::CaptionTickDue(id));
        s = next;
        id = tick_in(&cmds).unwrap();
    }
    assert!(chat_texts(&s).is_empty());
    let (s, _) = reduce(&s, Event::CaptionTickDue(id));
    assert_eq!(chat_texts(&s), ["soon"]);
}

#[test]
fn chat_during_a_hold_waits_as_long_as_the_hold_has_lasted() {
    // Captions just turned on while the stream plays at a normal buffer: the picture is held
    // while its buffer grows, and live chat would run ahead of it.
    let s = reduce(
        &playing(true),
        report(MAIN_SLOT, Some(100_000), NORMAL_BUFFER_MS),
    )
    .0;
    let (s, _) = chat(&s, vec![line("a", "before")]);
    assert_eq!(chat_texts(&s), ["before"], "the picture is not behind yet");
    let (mut s, cmds) = reduce(
        &s,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: true,
        },
    );
    let mut id = tick_in(&cmds).expect("the hold is timed");
    // 2 s into the hold: the picture is 2 s further behind live than it was.
    for _ in 0..8 {
        let (next, cmds) = reduce(&s, Event::CaptionTickDue(id));
        s = next;
        id = tick_in(&cmds).unwrap();
    }
    assert_eq!(s.captions.held_ms, 2_000);
    let (mut s, _) = chat(&s, vec![line("a", "during")]);
    assert_eq!(chat_texts(&s), ["before"]);
    assert_eq!(s.chat.held_len(), 1);
    // Let go: the line shows 2 s after the picture goes on, as it would have without the hold.
    s = reduce(
        &s,
        Event::PlayerHeld {
            slot: MAIN_SLOT,
            held: false,
        },
    )
    .0;
    for _ in 0..7 {
        let (next, cmds) = reduce(&s, Event::CaptionTickDue(id));
        s = next;
        id = tick_in(&cmds).unwrap();
    }
    assert_eq!(chat_texts(&s), ["before"]);
    let (s, _) = reduce(&s, Event::CaptionTickDue(id));
    assert_eq!(chat_texts(&s), ["before", "during"]);
    // The report after the hold has the grown buffer, and the hold's count is done.
    let (s, _) = reduce(&s, report(MAIN_SLOT, Some(102_000), DELAY_BUFFER_MS));
    assert_eq!(s.captions.held_ms, 0);
    assert_eq!(
        s.chat_due_ms(),
        Some(102_000 + u64::from(DELAY_BUFFER_MS - NORMAL_BUFFER_MS))
    );
}

#[test]
fn the_users_own_lines_show_at_once() {
    let s = delayed();
    let (s, _) = chat(&s, vec![line("someone", "held"), line("Viewer", "mine")]);
    assert_eq!(chat_texts(&s), ["mine"], "just sent, so shown");
    assert_eq!(s.chat.held_len(), 1);
}

#[test]
fn chat_shows_at_once_without_the_delay_and_held_lines_come_out_when_it_goes() {
    // Delay off: as before.
    let mut s = captioned();
    s = at(&s, 100_000).0;
    let (s, _) = chat(&s, vec![line("a", "now")]);
    assert_eq!(chat_texts(&s), ["now"]);
    // Delay on, a line held, then the delay turned off: it comes out at once.
    let s = delayed();
    let (s, _) = chat(&s, vec![line("a", "held")]);
    let s = on_delay_row(&s);
    let (s, _) = reduce(&s, Intent::Right);
    assert!(!s.settings.caption_delay);
    assert_eq!(chat_texts(&s), ["held"]);
    // Or captions turned off.
    let s = delayed();
    let (s, _) = chat(&s, vec![line("a", "held too")]);
    let s = on_row(&s);
    let (s, _) = reduce(&s, Intent::Right);
    assert_eq!(chat_texts(&s), ["held too"]);
}

#[test]
fn chat_before_the_first_report_and_with_a_normal_buffer_is_not_held() {
    // No clock yet: nothing to time it by.
    let (s, _) = chat(&playing(true), vec![line("a", "early")]);
    assert_eq!(chat_texts(&s), ["early"]);
    // A buffer no longer than a normal one: the picture is not behind, nor is chat.
    let s = reduce(
        &playing(true),
        report(MAIN_SLOT, Some(5_000), NORMAL_BUFFER_MS - 500),
    )
    .0;
    let (s, _) = chat(&s, vec![line("a", "x")]);
    assert_eq!(chat_texts(&s), ["x"]);
    assert_eq!(s.chat.held_len(), 0);
}

#[test]
fn moderation_reaches_held_lines() {
    let s = delayed();
    let (s, _) = chat(&s, vec![line("troll", "bad"), line("nice", "good")]);
    let (s, _) = reduce(
        &s,
        Event::ChatModerated {
            channel: ChannelRef::twitch("chan"),
            action: Moderation::ClearUser("troll".into()),
        },
    );
    let s = reduce(
        &s,
        report(MAIN_SLOT, Some(120_000), NORMAL_BUFFER_MS + 8_000),
    )
    .0;
    assert_eq!(chat_texts(&s), ["good"]);
}
