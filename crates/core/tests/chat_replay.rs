//! Chat replay of a video (vod-plan.md, phase 6): Y opens the panel, lines come due as the health
//! ticks pass their seconds, the next window is fetched while less than 30 s of lines are left,
//! every jump clears the lines and fetches from the new spot, stale answers are dropped, and a
//! video Twitch gives no replay for says so once. Live chat is untouched.

use htpc_core::*;

const MIN: u64 = 60_000;

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

fn run(s: &AppState, msgs: Vec<Msg>) -> (AppState, Vec<Command>) {
    let mut state = s.clone();
    let mut all = Vec::new();
    for msg in msgs {
        let (next, cmds) = reduce(&state, msg);
        state = next;
        all.extend(cmds);
    }
    (state, all)
}

fn press(s: &AppState, intents: &[Intent]) -> (AppState, Vec<Command>) {
    run(s, intents.iter().map(|i| Msg::from(*i)).collect())
}

/// Sleepy's channel page with broadcast `1` (focused), a highlight and a clip.
fn page() -> AppState {
    let (s, _) = init();
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

fn health(pos_ms: u64) -> Event {
    Event::PlayerHealth {
        slot: MAIN_SLOT,
        at_ms: 0,
        health: Health {
            stalled: false,
            buffer_ms: 60_000,
            pos_ms: Some(pos_ms),
            length_ms: Some(15_128_000),
        },
    }
}

/// Chapters at 0, 21:00 and 2:10:00.
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
            chapter(130 * MIN, "Night Circuit"),
        ],
        muted: Vec::new(),
        previews: None,
    })
}

/// Broadcast `1` playing at `pos_ms`, the panel closed.
fn playing_at(pos_ms: u64) -> AppState {
    let (s, _) = reduce(&page(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, details()).0;
    reduce(&s, health(pos_ms)).0
}

/// The panel opened at `pos_ms`, the request for the window not answered yet.
fn asked_at(pos_ms: u64) -> (AppState, Vec<Command>) {
    press(&playing_at(pos_ms), &[Intent::ToggleChat])
}

/// A line at `offset_s`: its id is the offset, its text "line <offset>".
fn at(offset_s: u32) -> ReplayLine {
    ReplayLine {
        offset_s,
        message: ChatMessage {
            id: format!("c{offset_s}"),
            ..ChatMessage::plain("chatter", &format!("line {offset_s}"))
        },
    }
}

/// A line with its own id, for several in one second.
fn said(offset_s: u32, id: &str) -> ReplayLine {
    ReplayLine {
        offset_s,
        message: ChatMessage {
            id: id.into(),
            ..ChatMessage::plain("chatter", &format!("line {id}"))
        },
    }
}

/// A line every 5 s from `from` to `to`, both inclusive.
fn window(from: u32, to: u32) -> Vec<ReplayLine> {
    (from..=to).step_by(5).map(at).collect()
}

fn answer(offset_s: u32, lines: Vec<ReplayLine>) -> Event {
    Event::ChatReplay {
        id: "1".into(),
        offset_s,
        lines,
    }
}

fn failure(offset_s: u32) -> Event {
    Event::ChatReplayFailed {
        id: "1".into(),
        offset_s,
    }
}

/// The offsets of the `LoadChatReplay` requests among `cmds`.
fn requests(cmds: &[Command]) -> Vec<u32> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::LoadChatReplay { id, offset_s } => {
                assert_eq!(id, "1", "the video on screen");
                Some(*offset_s)
            }
            _ => None,
        })
        .collect()
}

/// What the panel shows, oldest first.
fn shown(s: &AppState) -> Vec<String> {
    s.chat.iter().map(ChatMessage::text).collect()
}

fn texts(offsets: impl IntoIterator<Item = u32>) -> Vec<String> {
    offsets.into_iter().map(|o| format!("line {o}")).collect()
}

fn replay(s: &AppState) -> &ReplayChat {
    &s.video.as_ref().expect("a video is on screen").replay
}

/// The panel open at 10:00 with the window from 9:20 to 11:40 answered. Lines up to 10:00 show.
fn open_at_10_min() -> AppState {
    let (s, _) = asked_at(10 * MIN);
    reduce(&s, answer(600, window(560, 700))).0
}

/// The id of the last seek timer asked for.
fn seek_timer(cmds: &[Command]) -> (u32, u64) {
    cmds.iter()
        .rev()
        .find_map(|c| match c {
            Command::SettleSeek { id, ms } => Some((*id, *ms)),
            _ => None,
        })
        .expect("a seek timer")
}

// ---------------------------------------------------------------------------------------------
// Y
// ---------------------------------------------------------------------------------------------

#[test]
fn y_opens_the_panel_and_fetches_a_window_at_the_playhead() {
    let (s, cmds) = asked_at(10 * MIN);
    assert!(s.chat_open);
    assert_eq!(requests(&cmds), [600]);
    assert!(replay(&s).loading());
    assert!(s.chat.is_empty(), "nothing shown before the answer");
    // Live chat was not joined: there is no live channel.
    assert!(!cmds.contains(&Command::JoinChat {
        channel: ChannelRef::twitch("sleepy")
    }));
    assert!(s.now_playing.is_none());
}

#[test]
fn the_panel_closed_asks_for_nothing() {
    let s = playing_at(10 * MIN);
    assert!(!s.chat_open);
    let (s, cmds) = run(&s, vec![health(11 * MIN).into(), health(12 * MIN).into()]);
    assert!(requests(&cmds).is_empty());
    assert!(!replay(&s).loading());
}

#[test]
fn y_again_closes_the_panel_and_forgets_the_lines() {
    let s = open_at_10_min();
    assert!(!s.chat.is_empty());
    let (closed, cmds) = press(&s, &[Intent::ToggleChat]);
    assert!(!closed.chat_open);
    assert!(closed.chat.is_empty());
    assert_eq!(replay(&closed).waiting(), 0);
    assert!(requests(&cmds).is_empty());
    // Ticks ask for nothing while it is closed.
    let (_, cmds) = reduce(&closed, health(11 * MIN));
    assert!(requests(&cmds).is_empty());
    // And it opens again where the video is now.
    let (open, cmds) = run(
        &closed,
        vec![health(12 * MIN).into(), Intent::ToggleChat.into()],
    );
    assert_eq!(requests(&cmds), [720]);
    assert!(open.chat_open && open.chat.is_empty());
}

#[test]
fn y_in_the_player_menu_toggles_the_replay_too() {
    let (s, cmds) = press(&playing_at(10 * MIN), &[Intent::Menu, Intent::ToggleChat]);
    assert!(s.chat_open);
    assert_eq!(requests(&cmds), [600]);
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert!(!s.chat_open);
}

#[test]
fn a_clip_has_no_replay() {
    let (s, _) = press(&page(), &[Intent::PageRight, Intent::Select]);
    assert!(s.video.as_ref().unwrap().item.is_clip());
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let (next, cmds) = reduce(&s, Intent::ToggleChat);
    assert!(!next.chat_open);
    assert!(cmds.is_empty(), "{cmds:?}");
}

#[test]
fn y_does_nothing_while_the_video_has_failed_or_is_on_its_end_card() {
    let failed = reduce(
        &playing_at(MIN),
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "gone".into(),
        },
    )
    .0;
    let (next, cmds) = reduce(&failed, Intent::ToggleChat);
    assert!(!next.chat_open && cmds.is_empty());
    let ended = reduce(&playing_at(MIN), Event::PlaybackEnded { slot: MAIN_SLOT }).0;
    let (next, cmds) = reduce(&ended, Intent::ToggleChat);
    assert!(!next.chat_open && cmds.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Sync: lines appear as the health ticks pass them
// ---------------------------------------------------------------------------------------------

#[test]
fn lines_before_the_playhead_show_at_once_and_later_ones_wait() {
    let s = open_at_10_min();
    // The window starts 40 s before the playhead: those lines are the context.
    assert_eq!(shown(&s), texts((560..=600).step_by(5)));
    assert_eq!(replay(&s).waiting(), 20, "605 to 700 wait");
}

#[test]
fn lines_appear_as_ticks_pass_their_seconds() {
    let s = open_at_10_min();
    let (s, _) = reduce(&s, health(10 * MIN + 3_000));
    assert_eq!(shown(&s).last().unwrap(), "line 600", "605 is not due yet");
    let (s, _) = reduce(&s, health(10 * MIN + 5_000));
    assert_eq!(
        shown(&s).last().unwrap(),
        "line 605",
        "due at its second, not before"
    );
    // A tick can pass several at once (a slow report, or 2x).
    let (s, _) = reduce(&s, health(10 * MIN + 16_000));
    assert_eq!(shown(&s).last().unwrap(), "line 615");
    assert_eq!(shown(&s).len(), 9 + 3);
    // The lines keep their order and none repeats.
    let all = shown(&s);
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(all.len(), sorted.len());
}

#[test]
fn new_lines_ask_for_their_emote_images_once() {
    let mut with_emote = at(605);
    with_emote.message.spans = vec![
        ChatSpan::Text("nice".into()),
        ChatSpan::Emote {
            name: "Kappa".into(),
            url: "https://cdn/kappa".into(),
        },
    ];
    let (s, _) = asked_at(10 * MIN);
    let (s, cmds) = reduce(&s, answer(600, vec![at(590), with_emote]));
    assert!(!cmds
        .iter()
        .any(|c| matches!(c, Command::FetchChatImages { .. })));
    let (_, cmds) = reduce(&s, health(10 * MIN + 6_000));
    assert!(cmds.contains(&Command::FetchChatImages {
        urls: vec!["https://cdn/kappa".into()]
    }));
}

#[test]
fn no_tick_no_lines_so_a_paused_video_shows_nothing_new() {
    // The player sends no health while paused; the panel just holds.
    let s = open_at_10_min();
    let before = shown(&s);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(s.paused);
    assert_eq!(shown(&s), before);
}

// ---------------------------------------------------------------------------------------------
// Fetching the next window
// ---------------------------------------------------------------------------------------------

#[test]
fn the_next_window_is_asked_for_when_less_than_30_s_of_lines_are_left() {
    let s = open_at_10_min();
    // Fetched up to 11:40. 31 s ahead: nothing to ask yet.
    let (s, cmds) = reduce(&s, health(11 * MIN + 9_000));
    assert!(requests(&cmds).is_empty(), "{cmds:?}");
    // Exactly 30 s left is enough.
    let (s, cmds) = reduce(&s, health(11 * MIN + 10_000));
    assert!(requests(&cmds).is_empty(), "{cmds:?}");
    // Less than 30 s left: the next window, from the newest line.
    let (s, cmds) = reduce(&s, health(11 * MIN + 11_000));
    assert_eq!(requests(&cmds), [700]);
    assert!(replay(&s).loading());
}

#[test]
fn one_request_at_a_time() {
    let s = open_at_10_min();
    let (s, cmds) = reduce(&s, health(11 * MIN + 15_000));
    assert_eq!(requests(&cmds), [700]);
    let (_, cmds) = reduce(&s, health(11 * MIN + 16_000));
    assert!(requests(&cmds).is_empty(), "the answer is still on its way");
}

#[test]
fn a_window_that_overlaps_adds_only_what_is_new() {
    // The window asked at 700 starts 40 s earlier and repeats the lines up to 700. Two lines
    // share the second 700, and only one of them was in the first window.
    let (s, _) = asked_at(10 * MIN);
    let mut first = window(560, 695);
    first.push(said(700, "a"));
    let s = reduce(&s, answer(600, first)).0;
    let (s, cmds) = reduce(&s, health(11 * MIN + 15_000));
    assert_eq!(requests(&cmds), [700]);

    let mut second = window(660, 695);
    second.extend([said(700, "a"), said(700, "b"), at(705), at(710)]);
    let (s, _) = reduce(&s, answer(700, second));
    let (s, _) = reduce(&s, health(11 * MIN + 50_000));
    let all = shown(&s);
    assert_eq!(all.iter().filter(|t| *t == "line a").count(), 1);
    assert_eq!(all.iter().filter(|t| *t == "line b").count(), 1);
    assert_eq!(all.iter().filter(|t| *t == "line 650").count(), 1);
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(all.len(), sorted.len(), "no line twice: {all:?}");
    assert_eq!(&all[all.len() - 3..], ["line b", "line 705", "line 710"]);
}

#[test]
fn a_quiet_chat_fetches_rarely_and_a_busy_one_one_request_per_tick() {
    // Quiet: a window reaching 4 minutes ahead is not asked again for a long while.
    let (s, _) = asked_at(10 * MIN);
    let s = reduce(&s, answer(600, window(560, 840))).0;
    let (_, cmds) = reduce(&s, health(10 * MIN + 100_000));
    assert!(requests(&cmds).is_empty());
    // Busy: 59 lines that cover 10 s. Every tick asks again (one request at a time), from the
    // newest line, so the chat keeps up without back-to-back requests.
    let (s, _) = asked_at(10 * MIN);
    let busy: Vec<ReplayLine> = (0..59)
        .map(|i| said(595 + i / 6, &format!("b{i}")))
        .collect();
    let s = reduce(&s, answer(600, busy)).0;
    let (s, cmds) = reduce(&s, health(10 * MIN + 2_000));
    assert_eq!(requests(&cmds), [604]);
    let (_, cmds) = reduce(&s, answer(604, vec![said(604, "b58"), said(606, "x")]));
    assert!(
        requests(&cmds).is_empty(),
        "an answer does not chain another request"
    );
}

#[test]
fn the_end_of_the_chat_stops_the_requests_until_the_next_jump() {
    let s = open_at_10_min();
    let (s, _) = reduce(&s, health(11 * MIN + 15_000));
    // Nothing after 700: the same lines again.
    let (s, cmds) = reduce(&s, answer(700, window(660, 700)));
    assert!(requests(&cmds).is_empty());
    let (s, cmds) = reduce(&s, health(11 * MIN + 40_000));
    assert!(
        requests(&cmds).is_empty(),
        "no more asking at the end of the chat"
    );
    assert!(s.chat_open, "the panel stays");
    assert!(s.toasts.is_empty(), "the end of a chat is not a failure");
    // A jump asks again.
    let (_, cmds) = press(&s, &[Intent::PageLeft]);
    assert_eq!(requests(&cmds), [0]);
}

// ---------------------------------------------------------------------------------------------
// Jumps: seek, scrub release, chapter, start over
// ---------------------------------------------------------------------------------------------

/// The panel open at 10:00, shown lines up to 10:00, and a next request out (so a stale answer
/// has something to be late for).
fn open_with_a_request_out() -> (AppState, u32) {
    let s = open_at_10_min();
    let (s, cmds) = reduce(&s, health(11 * MIN + 15_000));
    let [offset] = requests(&cmds)[..] else {
        panic!("one request: {cmds:?}");
    };
    (s, offset)
}

#[test]
fn a_chapter_jump_clears_the_lines_and_fetches_from_the_new_spot() {
    let (s, _) = open_with_a_request_out();
    let (s, cmds) = press(&s, &[Intent::PageRight]);
    // RB: the next chapter, 21:00.
    assert!(cmds.contains(&Command::Seek {
        slot: MAIN_SLOT,
        to_ms: 21 * MIN
    }));
    assert_eq!(requests(&cmds), [1_260]);
    assert!(s.chat.is_empty());
    assert_eq!(replay(&s).waiting(), 0);
    assert!(s.chat_open, "the panel stays");
    // The new window, with the lines just before the spot.
    let (s, _) = reduce(&s, answer(1_260, window(1_220, 1_340)));
    assert_eq!(shown(&s), texts((1_220..=1_260).step_by(5)));
}

#[test]
fn a_tap_seek_clears_and_fetches_at_the_settled_spot() {
    let s = open_at_10_min();
    // Three taps forward: +30 s, sent once, 300 ms after the last.
    let mut state = s;
    let mut timer = (0, 0);
    for _ in 0..3 {
        let (next, _) = reduce(&state, Intent::Right);
        let (next, cmds) = reduce(&next, Intent::Released(Dir::Right));
        timer = seek_timer(&cmds);
        state = next;
    }
    assert!(!state.chat.is_empty(), "taps alone do not clear the lines");
    let (s, cmds) = reduce(&state, Event::SeekSettleDue(timer.0));
    assert!(cmds.contains(&Command::Seek {
        slot: MAIN_SLOT,
        to_ms: 10 * MIN + 30_000
    }));
    assert_eq!(requests(&cmds), [630]);
    assert!(s.chat.is_empty());
}

#[test]
fn a_scrub_asks_nothing_until_it_lets_go() {
    let s = open_at_10_min();
    let (s, cmds) = reduce(&s, Intent::Right);
    let id = cmds
        .iter()
        .find_map(|c| match c {
            Command::HoldCheck { id } => Some(*id),
            _ => None,
        })
        .unwrap();
    let (held, cmds) = reduce(&s, Event::HoldDue(id));
    assert!(requests(&cmds).is_empty(), "scrubbing is not a jump yet");
    assert!(
        !held.chat.is_empty(),
        "the lines stay while the picture is held"
    );
    let (released, cmds) = reduce(&held, Intent::Released(Dir::Right));
    let (settled, cmds) = reduce(&released, Event::SeekSettleDue(seek_timer(&cmds).0));
    let [offset] = requests(&cmds)[..] else {
        panic!("one request: {cmds:?}");
    };
    assert!(offset > 600, "from the scrubbed spot");
    assert!(settled.chat.is_empty());
}

#[test]
fn cancelling_a_scrub_keeps_the_lines() {
    let s = open_at_10_min();
    let before = shown(&s);
    let (s, cmds) = reduce(&s, Intent::Right);
    let id = cmds
        .iter()
        .find_map(|c| match c {
            Command::HoldCheck { id } => Some(*id),
            _ => None,
        })
        .unwrap();
    let (s, _) = reduce(&s, Event::HoldDue(id));
    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(requests(&cmds).is_empty());
    assert_eq!(shown(&s), before, "the picture never left");
}

#[test]
fn start_over_and_the_chapter_list_jump_too() {
    let s = open_at_10_min();
    // Start over: Start, then down to its row (Quality, Speed, Chapters, Start over).
    let (over, cmds) = press(
        &s,
        &[
            Intent::Menu,
            Intent::Down,
            Intent::Down,
            Intent::Down,
            Intent::Select,
        ],
    );
    assert_eq!(requests(&cmds), [0]);
    assert!(over.chat.is_empty());
    // The chapter list: A on the chapter it points at.
    let (listed, cmds) = press(
        &s,
        &[
            Intent::Menu,
            Intent::Down,
            Intent::Down,
            Intent::Right,
            Intent::Select,
        ],
    );
    assert_eq!(requests(&cmds), [1_260]);
    assert!(listed.chat.is_empty());
}

#[test]
fn watching_again_from_the_end_card_starts_the_replay_again() {
    let s = open_at_10_min();
    let (ended, _) = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT });
    // Watch again is the card's first row.
    let (again, cmds) = reduce(&ended, Intent::Select);
    assert_eq!(requests(&cmds), [0]);
    assert!(again.chat.is_empty());
    assert!(again.chat_open);
}

#[test]
fn a_report_from_before_the_seek_does_not_bring_old_lines_back() {
    let (s, _) = open_with_a_request_out();
    let (s, _) = press(&s, &[Intent::PageRight]);
    let s = reduce(&s, answer(1_260, window(1_220, 1_340))).0;
    let before = shown(&s);
    // Still playing the old spot for a moment: ignored until mpv has landed.
    let (s, cmds) = reduce(&s, health(11 * MIN + 20_000));
    assert_eq!(shown(&s), before);
    assert!(requests(&cmds).is_empty());
    // Landed: the lines go on from there.
    let (s, _) = reduce(&s, health(21 * MIN + 6_000));
    assert_eq!(shown(&s).last().unwrap(), "line 1265");
}

#[test]
fn a_quality_change_keeps_the_lines() {
    let s = open_at_10_min();
    let before = shown(&s);
    let (s, cmds) = press(&s, &[Intent::Menu, Intent::Right]);
    assert!(
        cmds.iter().any(|c| matches!(c, Command::PlayVideo { .. })),
        "the quality reloads the video: {cmds:?}"
    );
    assert!(requests(&cmds).is_empty());
    assert_eq!(shown(&s), before);
}

// ---------------------------------------------------------------------------------------------
// Stale answers
// ---------------------------------------------------------------------------------------------

#[test]
fn an_answer_for_an_old_offset_is_dropped() {
    let (s, old) = open_with_a_request_out();
    assert_eq!(old, 700);
    let (s, _) = press(&s, &[Intent::PageRight]);
    // The answer to the request from before the jump.
    let (late, cmds) = reduce(&s, answer(700, window(660, 760)));
    assert_eq!(late, s, "nothing changes");
    assert!(cmds.is_empty());
    // The one for the new spot counts.
    let (fresh, _) = reduce(&s, answer(1_260, window(1_220, 1_300)));
    assert!(!fresh.chat.is_empty());
}

#[test]
fn an_answer_for_another_video_or_after_leaving_is_dropped() {
    let (s, _) = asked_at(10 * MIN);
    let other = Event::ChatReplay {
        id: "2".into(),
        offset_s: 600,
        lines: window(560, 700),
    };
    let (next, cmds) = reduce(&s, other);
    assert_eq!(next, s);
    assert!(cmds.is_empty());
    // Left the video (B): the late answer finds no video, and live chat state is clean.
    let (gone, _) = reduce(&s, Intent::Back);
    assert!(gone.video.is_none());
    let (next, _) = reduce(&gone, answer(600, window(560, 700)));
    assert_eq!(next, gone);
    assert!(next.chat.is_empty());
}

#[test]
fn an_answer_after_closing_the_panel_is_dropped() {
    let (s, _) = asked_at(10 * MIN);
    let (closed, _) = reduce(&s, Intent::ToggleChat);
    let (next, _) = reduce(&closed, answer(600, window(560, 700)));
    assert_eq!(next, closed);
    assert!(next.chat.is_empty());
}

#[test]
fn an_answer_nobody_asked_for_is_dropped() {
    let s = playing_at(10 * MIN);
    let (next, _) = reduce(&s, answer(600, window(560, 700)));
    assert_eq!(next, s);
}

// ---------------------------------------------------------------------------------------------
// Failing soft
// ---------------------------------------------------------------------------------------------

#[test]
fn a_refused_first_request_says_there_is_no_replay_once() {
    let (s, _) = asked_at(10 * MIN);
    let (s, _) = reduce(&s, failure(600));
    assert!(!s.chat_open, "the panel closes");
    assert!(replay(&s).unavailable());
    assert_eq!(s.toasts.len(), 1);
    assert_eq!(s.toasts[0].kind, ToastKind::NoChatReplay);
    // Y does nothing more for this video: no panel, no request, no second notice.
    let (again, cmds) = reduce(&s, Intent::ToggleChat);
    assert_eq!(again, s);
    assert!(cmds.is_empty());
    // Not even after a jump.
    let (jumped, cmds) = press(&s, &[Intent::PageRight]);
    assert!(requests(&cmds).is_empty());
    assert_eq!(jumped.toasts.len(), 1);
}

#[test]
fn a_first_window_with_no_lines_says_the_same() {
    let (s, _) = asked_at(10 * MIN);
    let (s, _) = reduce(&s, answer(600, Vec::new()));
    assert!(!s.chat_open);
    assert_eq!(s.toasts[0].kind, ToastKind::NoChatReplay);
}

#[test]
fn a_failure_after_lines_came_keeps_the_panel_and_a_jump_tries_again() {
    let s = open_at_10_min();
    let (s, _) = reduce(&s, health(11 * MIN + 15_000));
    let (s, cmds) = reduce(&s, failure(700));
    assert!(s.chat_open);
    assert!(s.toasts.is_empty(), "no notice once the replay has worked");
    assert!(!replay(&s).loading());
    // No hammering: nothing more until a jump.
    let (s, cmds2) = reduce(&s, health(11 * MIN + 40_000));
    assert!(requests(&cmds).is_empty() && requests(&cmds2).is_empty());
    let (_, cmds) = press(&s, &[Intent::PageRight]);
    assert_eq!(requests(&cmds), [1_260]);
}

#[test]
fn a_jump_past_the_last_line_is_not_a_missing_replay() {
    // Lines came before. A window with nothing at a later spot is only the end of the chat.
    let s = open_at_10_min();
    let (s, _) = press(&s, &[Intent::PageRight]);
    let (s, _) = reduce(&s, failure(1_260));
    assert!(s.chat_open);
    assert!(s.toasts.is_empty());
    let (s, _) = press(&s, &[Intent::PageRight]);
    let (s, _) = reduce(&s, answer(7_800, Vec::new()));
    assert!(s.chat_open);
    assert!(s.toasts.is_empty());
    assert_eq!(shown(&s), Vec::<String>::new());
}

#[test]
fn a_failure_for_a_stale_request_is_ignored() {
    let (s, _) = asked_at(10 * MIN);
    let (s, _) = press(&s, &[Intent::PageRight]);
    let (next, _) = reduce(&s, failure(600));
    assert_eq!(next, s);
    assert!(next.toasts.is_empty() && next.chat_open);
}

// ---------------------------------------------------------------------------------------------
// Live chat stays what it was
// ---------------------------------------------------------------------------------------------

#[test]
fn leaving_the_video_leaves_no_lines_behind() {
    let s = open_at_10_min();
    assert!(!s.chat.is_empty());
    let (s, _) = reduce(&s, Intent::Back);
    assert!(s.chat.is_empty());
    assert!(!s.chat_open);
    assert_eq!(s.screen, Screen::Channel);
}

#[test]
fn leaving_a_video_with_the_panel_open_still_saves_continue_watching() {
    let s = open_at_10_min();
    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(s.chat.is_empty());
    assert!(!s.chat_open);
    let saved: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            Command::SaveContinue(list) => Some(list),
            _ => None,
        })
        .collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].len(), 1);
    assert!(cmds.contains(&Command::StopPlayback { slot: MAIN_SLOT }));
}

#[test]
fn a_video_opened_next_starts_with_the_panel_closed_and_empty() {
    let s = open_at_10_min();
    let (s, _) = reduce(&s, Intent::Back);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(s.video.is_some());
    assert!(!s.chat_open);
    assert!(s.chat.is_empty());
    assert!(!replay(&s).loading());
}

#[test]
fn live_chat_lines_never_reach_a_video() {
    let s = open_at_10_min();
    let before = shown(&s);
    let (s, cmds) = reduce(
        &s,
        Event::ChatMessages {
            channel: ChannelRef::twitch("sleepy"),
            messages: vec![ChatMessage::plain("live", "hello")],
        },
    );
    assert_eq!(shown(&s), before);
    assert!(cmds.is_empty());
}

#[test]
fn the_live_player_keeps_y_as_the_chat_toggle() {
    // A live stream: Y shows and hides its chat, sends no replay request and no compose box.
    let (s, _) = init();
    let stream = Stream {
        channel: ChannelRef::twitch("sleepy"),
        user_id: "77".into(),
        title: "live".into(),
        category: "Hollow Knight".into(),
        viewers: 10,
        tint: Rgb::default(),
        thumbnail: None,
    };
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![Tile::Stream(stream)])],
        },
    )
    .0;
    let (s, cmds) = press(&s, &[Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert!(s.video.is_none());
    assert!(cmds.iter().any(|c| matches!(c, Command::JoinChat { .. })));
    let (shown_chat, cmds) = reduce(&s, Intent::ToggleChat);
    assert!(shown_chat.chat_open);
    assert!(requests(&cmds).is_empty());
    let (hidden, _) = reduce(&shown_chat, Intent::ToggleChat);
    assert!(!hidden.chat_open);
}
