//! The video player (vod-plan.md, phase 4): every button of section 2's table, chapter jumps at
//! and near a chapter start, speed, start over, the player menu's rows for a video, the end card,
//! errors, and the hazards of phase 2's notes (a video's events never reach a live stream, its
//! health stays out of the slow-network rule, a seek while loading goes into `start_ms`).
//! Seeking with ◀ ▶ has its own file (`seeking.rs`).

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
        // 4:12:08
        length_s: 15_128,
        created_at: 1_794_335_302,
        views: 18_000,
        tint: Rgb::default(),
        thumbnail: None,
        muted: vec![Span {
            start_s: 5_400,
            length_s: 360,
        }],
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

/// Sleepy's channel page, loaded, focus on the first broadcast: broadcasts `1` (muted part), `2`
/// (subscriber-only), `3`; a highlight; a clip.
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
    assert_eq!(s.screen, Screen::Channel);
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
        Row::new(
            RowKind::PastBroadcasts,
            vec![
                Tile::Video(video("1")),
                Tile::Video(Video {
                    sub_only: true,
                    ..video("2")
                }),
                Tile::Video(video("3")),
            ],
        ),
        Row::new(RowKind::Highlights, vec![Tile::Video(video("4"))]),
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

fn play_cmd(id: &str, start_ms: u64) -> Command {
    Command::PlayVideo {
        slot: MAIN_SLOT,
        target: VideoTarget::Video(id.into()),
        start_ms,
        stream: StreamKind::Video(Quality::Best),
    }
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

/// Chapters at 0, 21:00, 2:10:00, 3:10:00 and 3:51:40.
fn details(id: &str) -> Event {
    let chapter = |start_ms: u64, name: &str| Chapter {
        start_ms,
        length_ms: 0,
        name: name.into(),
    };
    Event::VideoDetails(VideoDetails {
        id: id.into(),
        length_s: 15_128,
        chapters: vec![
            chapter(0, "Just Chatting"),
            chapter(21 * MIN, "Hollow Depths"),
            chapter(130 * MIN, "Night Circuit"),
            chapter(190 * MIN, "Hollow Depths"),
            chapter(231 * MIN + 40_000, "Just Chatting"),
        ],
        muted: vec![
            Span {
                start_s: 5_400,
                length_s: 360,
            },
            Span {
                start_s: 11_020,
                length_s: 360,
            },
        ],
        previews: Some("https://cdn/storyboard-info.json".into()),
    })
}

/// Broadcast `1` playing from the start, with its chapters, at `pos_ms`.
fn playing_at(pos_ms: u64) -> AppState {
    let (s, _) = reduce(&page(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, details("1")).0;
    reduce(&s, health(pos_ms)).0
}

fn seeks(cmds: &[Command]) -> Vec<u64> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Seek { to_ms, .. } => Some(*to_ms),
            _ => None,
        })
        .collect()
}

fn v(s: &AppState) -> &VideoSession {
    s.video.as_ref().expect("a video is on screen")
}

// ---------------------------------------------------------------------------------------------
// Opening a video
// ---------------------------------------------------------------------------------------------

#[test]
fn a_on_a_video_card_plays_it_from_the_start_over_the_page() {
    let (s, cmds) = reduce(&page(), Intent::Select);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.back_stack, vec![Screen::Home, Screen::Channel]);
    assert_eq!(s.playback, Playback::Loading);
    assert_eq!(s.now_playing, None, "a video is not now_playing");
    assert!(s.channel_page.is_some(), "the page stays under the player");
    assert_eq!(
        cmds,
        vec![
            play_cmd("1", 0),
            Command::LoadVideoDetails { id: "1".into() }
        ]
    );
    let v = v(&s);
    assert_eq!(v.item, VideoItem::Video(video("1")));
    assert_eq!(
        v.length_ms, 15_128_000,
        "the card's length until the player's"
    );
    assert_eq!(v.speed, 1_000);
    // Nothing live is joined or watched.
    assert!(!cmds.iter().any(|c| matches!(
        c,
        Command::JoinChat { .. } | Command::WatchHypeTrain { .. } | Command::WatchRaids { .. }
    )));
}

#[test]
fn the_first_frame_shows_the_video() {
    let (s, _) = reduce(&page(), Intent::Select);
    let (s, cmds) = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT });
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(v(&s).load, VideoLoad::Shown);
    assert!(cmds.is_empty());
}

#[test]
fn a_saved_place_resumes_with_a_toast_and_x_starts_over() {
    let mut s = page();
    s.continue_watching = vec![Progress {
        video: video("1"),
        pos_s: 4_360,
        saved_at: 1,
    }];
    let (resumed, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds[0], play_cmd("1", 4_360_000));
    assert_eq!(v(&resumed).pos_ms, 4_360_000);
    assert_eq!(
        resumed.toasts[0].kind,
        ToastKind::Resumed { pos_s: 4_360 },
        "Resumed at 1:12:40 · Start over in the menu"
    );
    assert!(cmds.contains(&Command::ExpireToast {
        id: resumed.toasts[0].id
    }));

    let (over, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(over.screen, Screen::Player);
    assert_eq!(cmds[0], play_cmd("1", 0));
    assert!(over.toasts.is_empty(), "no toast from the start");

    // Without a saved place X does nothing on a video.
    let (same, cmds) = reduce(&page(), Intent::Retry);
    assert_eq!(same.screen, Screen::Channel);
    assert!(cmds.is_empty());
}

#[test]
fn a_subscriber_only_video_opens_the_subscribe_card_and_never_plays() {
    let (s, cmds) = press(&page(), &[Intent::Right, Intent::Select]);
    assert!(s.subscribe_card);
    assert_eq!(s.screen, Screen::Channel);
    assert!(s.video.is_none());
    assert!(cmds.is_empty());
    // X (start over) on it, with a saved place: the card too.
    let mut saved = page();
    saved.continue_watching = vec![Progress {
        video: Video {
            sub_only: true,
            ..video("2")
        },
        pos_s: 60,
        saved_at: 1,
    }];
    let (s, cmds) = press(&saved, &[Intent::Right, Intent::Retry]);
    assert!(s.subscribe_card);
    assert!(s.video.is_none());
    assert!(!cmds.iter().any(|c| matches!(c, Command::PlayVideo { .. })));
}

#[test]
fn a_clip_plays_as_a_clip_without_details() {
    // The clip is on the Clips tab.
    let (s, cmds) = press(&page(), &[Intent::PageRight, Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(
        cmds,
        vec![Command::PlayVideo {
            slot: MAIN_SLOT,
            target: VideoTarget::Clip("a".into()),
            start_ms: 0,
            stream: StreamKind::Video(Quality::Best),
        }]
    );
    assert!(v(&s).item.is_clip());
    // Its menu has no chapters.
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let (s, _) = reduce(&s, Intent::Menu);
    assert_eq!(s.player_menu_rows(), &PlayerMenuRow::CLIP);
    assert!(!s.player_menu_rows().contains(&PlayerMenuRow::Chapters));
}

#[test]
fn details_fill_in_chapters_and_muted_parts_of_this_video_only() {
    let (s, _) = reduce(&page(), Intent::Select);
    let other = reduce(&s, details("3")).0;
    assert!(v(&other).chapters.is_empty(), "another video's details");
    let s = reduce(&s, details("1")).0;
    assert_eq!(v(&s).chapters.len(), 5);
    assert_eq!(v(&s).muted.len(), 2);
    assert_eq!(
        v(&s).previews.as_deref(),
        Some("https://cdn/storyboard-info.json")
    );
}

// ---------------------------------------------------------------------------------------------
// The buttons
// ---------------------------------------------------------------------------------------------

#[test]
fn a_pauses_and_plays_and_does_nothing_while_loading() {
    let s = playing_at(10 * MIN);
    let (paused, cmds) = reduce(&s, Intent::Select);
    assert!(paused.paused);
    assert_eq!(
        cmds,
        vec![Command::SetPaused {
            slot: MAIN_SLOT,
            paused: true
        }]
    );
    let (again, cmds) = reduce(&paused, Intent::Select);
    assert!(!again.paused);
    assert_eq!(
        cmds,
        vec![Command::SetPaused {
            slot: MAIN_SLOT,
            paused: false
        }]
    );
    let (loading, _) = reduce(&page(), Intent::Select);
    let (loading2, cmds) = reduce(&loading, Intent::Select);
    assert!(!loading2.paused);
    assert!(cmds.is_empty());
}

#[test]
fn b_stops_the_video_and_returns_to_the_page_where_it_was() {
    let start = press(&page(), &[Intent::Right, Intent::Right]).0;
    let (s, _) = reduce(&start, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(cmds, vec![Command::StopPlayback { slot: MAIN_SLOT }]);
    assert!(s.video.is_none());
    assert_eq!(s.playback, Playback::Idle);
    assert_eq!(s.channel_page.as_ref().unwrap().videos.focus.col, 2);
}

#[test]
fn y_and_view_do_nothing_on_a_video() {
    let s = playing_at(MIN);
    for intent in [Intent::ToggleChat, Intent::View] {
        let (next, cmds) = reduce(&s, intent);
        assert!(!next.chat_open);
        assert!(next.popup.is_none());
        assert!(cmds.is_empty(), "{intent:?}: {cmds:?}");
    }
}

#[test]
fn rb_goes_to_the_next_chapter_and_stops_at_the_last() {
    let (s, cmds) = reduce(&playing_at(25 * MIN), Intent::PageRight);
    assert_eq!(seeks(&cmds), [130 * MIN]);
    assert_eq!(v(&s).pos_ms, 130 * MIN, "drawn there at once");
    let (_, cmds) = reduce(&playing_at(232 * MIN), Intent::PageRight);
    assert!(seeks(&cmds).is_empty(), "already in the last chapter");
}

#[test]
fn lb_goes_to_the_start_of_this_chapter_or_the_one_before_near_its_start() {
    // 4 min into "Night Circuit": its start.
    let (_, cmds) = reduce(&playing_at(134 * MIN), Intent::PageLeft);
    assert_eq!(seeks(&cmds), [130 * MIN]);
    // 3 s in: the chapter before.
    let (_, cmds) = reduce(&playing_at(130 * MIN + 3_000), Intent::PageLeft);
    assert_eq!(seeks(&cmds), [21 * MIN]);
    // Exactly 5 s in counts as "in" the chapter.
    let (_, cmds) = reduce(&playing_at(130 * MIN + 5_000), Intent::PageLeft);
    assert_eq!(seeks(&cmds), [130 * MIN]);
    // In the first chapter, near its start: the start.
    let (_, cmds) = reduce(&playing_at(2_000), Intent::PageLeft);
    assert_eq!(seeks(&cmds), [0]);
}

#[test]
fn a_video_without_chapters_is_one_chapter() {
    let (s, _) = reduce(&page(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, health(40 * MIN)).0;
    let (_, cmds) = reduce(&s, Intent::PageLeft);
    assert_eq!(seeks(&cmds), [0]);
    let (_, cmds) = reduce(&s, Intent::PageRight);
    assert!(seeks(&cmds).is_empty());
}

// ---------------------------------------------------------------------------------------------
// The player menu
// ---------------------------------------------------------------------------------------------

#[test]
fn the_menu_shows_a_videos_rows_and_hides_latency_subscribe_and_chat() {
    let (s, _) = reduce(&playing_at(MIN), Intent::Menu);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Quality));
    assert_eq!(s.player_menu_rows(), &PlayerMenuRow::VIDEO);
    for hidden in [
        PlayerMenuRow::Latency,
        PlayerMenuRow::Subscribe,
        PlayerMenuRow::WriteChat,
    ] {
        assert!(!s.player_menu_rows().contains(&hidden), "{hidden:?}");
    }
    let mut visited = vec![s.player_menu.unwrap()];
    let mut at = s;
    for _ in 0..12 {
        at = reduce(&at, Intent::Down).0;
        if visited.last() != at.player_menu.as_ref() {
            visited.push(at.player_menu.unwrap());
        }
    }
    assert_eq!(visited, PlayerMenuRow::VIDEO.to_vec());
    // A live stream's menu is unchanged.
    assert_eq!(AppState::default().player_menu_rows(), &PlayerMenuRow::ALL);
}

#[test]
fn speed_steps_through_three_quarters_to_double_for_this_video() {
    let (s, _) = press(&playing_at(MIN), &[Intent::Menu, Intent::Down]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Speed));
    let (fast, cmds) = reduce(&s, Intent::Right);
    assert_eq!(v(&fast).speed, 1_250);
    assert_eq!(
        cmds,
        vec![Command::SetSpeed {
            slot: MAIN_SLOT,
            milli: 1_250
        }]
    );
    let (slow, cmds) = reduce(&s, Intent::Left);
    assert_eq!(v(&slow).speed, 750);
    assert_eq!(
        cmds,
        vec![Command::SetSpeed {
            slot: MAIN_SLOT,
            milli: 750
        }]
    );
    let (double, _) = press(&s, &[Intent::Select, Intent::Select, Intent::Select]);
    assert_eq!(v(&double).speed, 2_000);
    // The next video starts at 1x.
    let (back, _) = press(&double, &[Intent::Back, Intent::Back]);
    assert_eq!(back.screen, Screen::Channel);
    let (next, _) = reduce(&back, Intent::Select);
    assert_eq!(v(&next).speed, 1_000);
}

#[test]
fn chapters_in_the_menu_point_and_jump() {
    let s = playing_at(25 * MIN);
    let (s, _) = press(&s, &[Intent::Menu, Intent::Down, Intent::Down]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Chapters));
    assert_eq!(v(&s).menu_chapter, 1, "it points at the chapter on screen");
    let (s, cmds) = press(&s, &[Intent::Right, Intent::Right]);
    assert!(cmds.is_empty(), "Left and Right only point");
    assert_eq!(v(&s).menu_chapter, 3);
    let (s, _) = press(&s, &[Intent::Right, Intent::Right, Intent::Right]);
    assert_eq!(v(&s).menu_chapter, 4, "stops at the last");
    let (s, _) = reduce(&s, Intent::Left);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(seeks(&cmds), [190 * MIN]);
    assert_eq!(s.player_menu, None, "the jump closes the menu");
}

#[test]
fn start_over_in_the_menu_goes_to_the_start_and_plays() {
    let s = playing_at(70 * MIN);
    let (paused, _) = reduce(&s, Intent::Select);
    let (s, _) = press(
        &paused,
        &[Intent::Menu, Intent::Down, Intent::Down, Intent::Down],
    );
    assert_eq!(s.player_menu, Some(PlayerMenuRow::StartOver));
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(
        cmds,
        vec![
            Command::Seek {
                slot: MAIN_SLOT,
                to_ms: 0
            },
            Command::SetPaused {
                slot: MAIN_SLOT,
                paused: false
            }
        ]
    );
    assert_eq!(s.player_menu, None);
    assert!(!s.paused);
    assert_eq!(v(&s).pos_ms, 0);
}

#[test]
fn a_quality_change_reloads_the_same_video_where_it_is_behind_the_picture() {
    let s = playing_at(70 * MIN);
    let (s, cmds) = press(&s, &[Intent::Menu, Intent::Right]);
    assert_eq!(s.quality(), Quality::P1080);
    assert_eq!(
        cmds,
        vec![Command::PlayVideo {
            slot: MAIN_SLOT,
            target: VideoTarget::Video("1".into()),
            start_ms: 70 * MIN,
            stream: StreamKind::Video(Quality::P1080),
        }]
    );
    assert_eq!(s.playback, Playback::Playing, "the old quality plays on");
    assert_eq!(v(&s).load, VideoLoad::Reloading { reports: 0 });
    // The new quality's first frame: seeks go to the player again.
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    assert_eq!(v(&s).load, VideoLoad::Shown);
}

#[test]
fn audio_only_reloads_the_video_as_sound() {
    let s = playing_at(5 * MIN);
    let rows = &[Intent::Menu, Intent::Down, Intent::Down, Intent::Down];
    let (s, _) = press(&s, rows);
    let (s, _) = press(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Down],
    );
    assert_eq!(s.player_menu, Some(PlayerMenuRow::AudioOnly));
    let (s, cmds) = reduce(&s, Intent::Right);
    assert!(s.audio_only);
    assert_eq!(
        cmds,
        vec![Command::PlayVideo {
            slot: MAIN_SLOT,
            target: VideoTarget::Video("1".into()),
            start_ms: 5 * MIN,
            stream: StreamKind::AudioOnly,
        }]
    );
}

#[test]
fn channel_videos_in_the_menu_goes_back_to_the_page_below() {
    let s = playing_at(5 * MIN);
    let (s, _) = reduce(&s, Intent::Menu);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::ChannelVideos));
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    assert_eq!(cmds, vec![Command::StopPlayback { slot: MAIN_SLOT }]);
    assert!(s.video.is_none());
    assert!(s.player_menu.is_none());
}

#[test]
fn channel_videos_without_a_page_below_opens_the_videos_channel() {
    // As if the video were opened somewhere else (Continue watching, phase 5).
    let mut s = playing_at(5 * MIN);
    s.back_stack = vec![Screen::Home];
    s.channel_page = None;
    let (s, _) = reduce(&s, Intent::Menu);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(
        s.back_stack,
        vec![Screen::Home],
        "the page takes the player's place"
    );
    assert_eq!(
        cmds,
        vec![
            Command::StopPlayback { slot: MAIN_SLOT },
            Command::LoadChannelPage {
                channel: ChannelRef::twitch("sleepy")
            }
        ]
    );
    assert_eq!(s.channel_page.as_ref().unwrap().name, "Sleepy Fox");
}

// ---------------------------------------------------------------------------------------------
// The end, and errors
// ---------------------------------------------------------------------------------------------

#[test]
fn the_end_shows_the_end_card_with_no_autoplay() {
    let s = playing_at(15_120_000);
    let (s, cmds) = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT });
    assert!(cmds.is_empty(), "nothing plays next");
    assert_eq!(v(&s).end_card, Some(EndRow::WatchAgain));
    assert_eq!(v(&s).pos_ms, 15_128_000);
    assert_eq!(s.screen, Screen::Player);
    // The D-pad moves on the card, it does not seek.
    let (down, cmds) = press(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Right],
    );
    assert_eq!(v(&down).end_card, Some(EndRow::Back));
    assert!(cmds.is_empty());
    // Watch again: from the start, loading.
    let (again, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds, vec![play_cmd("1", 0)]);
    assert_eq!(again.playback, Playback::Loading);
    assert_eq!(v(&again).end_card, None);
    // Channel videos: the page below.
    let (page, cmds) = press(&s, &[Intent::Down, Intent::Select]);
    assert_eq!(page.screen, Screen::Channel);
    assert_eq!(cmds, vec![Command::StopPlayback { slot: MAIN_SLOT }]);
    // Back, and B.
    let (b, _) = reduce(&s, Intent::Back);
    assert_eq!(b.screen, Screen::Channel);
    let (back_row, _) = press(&s, &[Intent::Down, Intent::Down, Intent::Select]);
    assert_eq!(back_row.screen, Screen::Channel);
}

#[test]
fn a_clip_ends_on_the_end_card_too() {
    let (s, _) = press(&page(), &[Intent::PageRight, Intent::Select]);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT }).0;
    assert_eq!(v(&s).end_card, Some(EndRow::WatchAgain));
}

#[test]
fn a_failed_or_deleted_video_shows_an_error_with_retry_and_back() {
    let (s, _) = reduce(&page(), Intent::Select);
    let (s, _) = reduce(
        &s,
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "This video is no longer available.".into(),
        },
    );
    assert_eq!(
        s.playback,
        Playback::Failed("This video is no longer available.".into())
    );
    // A tries again from where it was.
    let (retry, cmds) = reduce(&s, Intent::Select);
    assert_eq!(retry.playback, Playback::Loading);
    assert_eq!(cmds, vec![play_cmd("1", 0)]);
    let (retry_x, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(retry_x.playback, Playback::Loading);
    assert_eq!(cmds, vec![play_cmd("1", 0)]);
    // ◀ ▶ do nothing on the error screen.
    let (_, cmds) = reduce(&s, Intent::Right);
    assert!(cmds.is_empty());
    // B goes back.
    let (b, cmds) = reduce(&s, Intent::Back);
    assert_eq!(b.screen, Screen::Channel);
    assert_eq!(cmds, vec![Command::StopPlayback { slot: MAIN_SLOT }]);
}

#[test]
fn a_failure_mid_video_keeps_the_place_for_the_retry() {
    let s = playing_at(42 * MIN);
    let (s, _) = reduce(
        &s,
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "x".into(),
        },
    );
    let (_, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds, vec![play_cmd("1", 42 * MIN)]);
}

// ---------------------------------------------------------------------------------------------
// Phase 2's hazards
// ---------------------------------------------------------------------------------------------

#[test]
fn a_live_streams_answers_never_touch_a_video() {
    let s = playing_at(MIN);
    for event in [
        Event::StreamOffline {
            channel: ChannelRef::twitch("sleepy"),
        },
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("sleepy"),
            message: "x".into(),
        },
        Event::OfflineRecheckDue(s.offline_recheck),
        Event::ChatMessages {
            channel: ChannelRef::twitch("sleepy"),
            messages: vec![ChatMessage::plain("a", "hi")],
        },
    ] {
        let (next, cmds) = reduce(&s, event.clone());
        assert_eq!(next.playback, Playback::Playing, "{event:?}");
        assert!(next.chat.is_empty(), "{event:?}");
        assert!(cmds.is_empty(), "{event:?}: {cmds:?}");
    }
}

#[test]
fn a_videos_late_events_find_nothing_once_it_is_left() {
    let s = playing_at(MIN);
    let (page, _) = reduce(&s, Intent::Back);
    for event in [
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "late".into(),
        },
        Event::PlaybackEnded { slot: MAIN_SLOT },
        Event::PlaybackStarted { slot: MAIN_SLOT },
        health(2 * MIN),
        details("1"),
    ] {
        let (next, cmds) = reduce(&page, event.clone());
        assert_eq!(next, page, "{event:?}");
        assert!(cmds.is_empty(), "{event:?}");
    }
    // And a live stream opened afterwards starts on its own.
    let (home, _) = reduce(&page, Intent::Back);
    assert_eq!(home.screen, Screen::Home);
    assert!(home.video.is_none());
}

#[test]
fn a_videos_health_stays_out_of_the_slow_network_rule_and_moves_the_playhead() {
    let s = playing_at(MIN);
    let stalled = |at_ms: u64, pos_ms: u64| Event::PlayerHealth {
        slot: MAIN_SLOT,
        at_ms,
        health: Health {
            stalled: true,
            buffer_ms: 0,
            pos_ms: Some(pos_ms),
            length_ms: Some(15_200_000),
        },
    };
    let mut state = s;
    for i in 0..40u64 {
        let (next, cmds) = reduce(&state, stalled(i * 1_000, MIN + i * 1_000));
        assert!(cmds.is_empty(), "{cmds:?}");
        state = next;
    }
    assert_eq!(state.net, NetLimit::default(), "the limit never moved");
    assert!(state.toasts.is_empty());
    assert_eq!(v(&state).pos_ms, MIN + 39_000);
    assert_eq!(v(&state).length_ms, 15_200_000, "the player's length wins");
}

#[test]
fn entering_a_muted_part_brings_the_hud_back() {
    let s = playing_at(5_398_000);
    let pulse = s.hud_pulse;
    let s = reduce(&s, health(5_399_000)).0;
    assert_eq!(s.hud_pulse, pulse, "not muted yet");
    let s = reduce(&s, health(5_400_000)).0;
    assert_ne!(s.hud_pulse, pulse, "muted from 1:30:00");
    let again = s.hud_pulse;
    let s = reduce(&s, health(5_401_000)).0;
    assert_eq!(s.hud_pulse, again, "once, on the way in");
}

#[test]
fn losing_focus_pauses_a_video_and_coming_back_resumes_it() {
    let s = playing_at(MIN);
    let (away, cmds) = reduce(&s, Event::WindowFocus(false));
    assert!(away.paused);
    assert_eq!(
        cmds,
        vec![Command::SetPaused {
            slot: MAIN_SLOT,
            paused: true
        }]
    );
    let (back, cmds) = reduce(&away, Event::WindowFocus(true));
    assert!(!back.paused);
    assert_eq!(
        cmds,
        vec![Command::SetPaused {
            slot: MAIN_SLOT,
            paused: false
        }]
    );
}

#[test]
fn the_sleep_timer_stops_the_video_and_suspends() {
    let s = playing_at(MIN);
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 8]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::SleepTimer));
    let (s, _) = reduce(&s, Intent::Right);
    let id = s.sleep.unwrap().id;
    let (s, cmds) = reduce(&s, Event::SleepTimerFired { id });
    assert_eq!(s.screen, Screen::Channel);
    assert!(s.video.is_none());
    assert_eq!(
        cmds,
        vec![Command::StopPlayback { slot: MAIN_SLOT }, Command::Suspend]
    );
}

#[test]
fn stats_follow_a_video_like_a_stream() {
    let mut start = page();
    start.session.stats = true;
    let (s, cmds) = reduce(&start, Intent::Select);
    assert!(cmds.contains(&Command::SetStats {
        slot: MAIN_SLOT,
        on: true
    }));
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, Event::PlayerStats(PlayerStats::default())).0;
    assert!(s.stats.is_some());
    let (_, cmds) = reduce(&s, Intent::Back);
    assert_eq!(
        cmds,
        vec![
            Command::StopPlayback { slot: MAIN_SLOT },
            Command::SetStats {
                slot: MAIN_SLOT,
                on: false
            }
        ]
    );
}
