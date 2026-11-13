//! *Continue watching* (vod-plan.md, phase 5): a video's place is saved when it is left (B, the end
//! card's *Back*, the window closing), at most ten, newest first; it is dropped when it ends, when
//! it is left within two minutes of its end and when it turns out to be gone; clips are never
//! saved. Home shows the list as a row after Followed, A resumes and X removes.

use htpc_core::*;

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

fn saved(id: &str, pos_s: u32) -> Progress {
    Progress {
        video: video(id),
        pos_s,
        saved_at: 1_794_400_000,
    }
}

fn stream(login: &str) -> Stream {
    Stream {
        channel: ChannelRef::twitch(login),
        user_id: "1".into(),
        title: "live".into(),
        category: "Chatting".into(),
        viewers: 10,
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

/// Home with a hero, Followed and Top Live, loaded, and `list` as the saved places.
fn home(list: Vec<Progress>) -> AppState {
    let (mut s, _) = init();
    s.continue_watching = list;
    let rows = vec![
        Row::new(RowKind::Featured, vec![Tile::Stream(stream("hero"))]),
        Row::new(RowKind::FollowedLive, vec![Tile::Stream(stream("fol"))]),
        Row::new(RowKind::TopLive, vec![Tile::Stream(stream("top"))]),
    ];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

/// Sleepy's channel page, opened from Home, with broadcasts `1`, `2`, `3` and a clip.
fn page(list: Vec<Progress>) -> AppState {
    let mut s = home(Vec::new());
    s.continue_watching = list;
    // A live card's Y: Featured is the hero. Open the page by its channel directly.
    let offline = Tile::Channel(Channel {
        channel: ChannelRef::twitch("sleepy"),
        name: "sleepy".into(),
        tint: Rgb::default(),
    });
    s.home.rows[2].tiles = vec![offline];
    s.home.focus = Focus { row: 2, col: 0 };
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
                Tile::Video(video("2")),
                Tile::Video(video("3")),
            ],
        ),
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

/// The first video of the page playing, at `pos_s`.
fn playing_at(s: &AppState, pos_s: u64) -> AppState {
    let (s, _) = reduce(s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    reduce(&s, health(pos_s * 1_000)).0
}

fn saves(cmds: &[Command]) -> Vec<&Vec<Progress>> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::SaveContinue(list) => Some(list),
            _ => None,
        })
        .collect()
}

fn ids(list: &[Progress]) -> Vec<&str> {
    list.iter().map(|p| p.video.id.as_str()).collect()
}

fn home_kinds(s: &AppState) -> Vec<RowKind> {
    s.home.rows.iter().map(|r| r.kind).collect()
}

// ---------------------------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------------------------

#[test]
fn b_out_of_a_video_saves_where_it_was_with_the_whole_card() {
    let (s, cmds) = press(&playing_at(&page(Vec::new()), 4_360), &[Intent::Back]);
    assert_eq!(s.screen, Screen::Channel);
    let list = saves(&cmds);
    assert_eq!(list.len(), 1);
    assert_eq!(ids(list[0]), ["1"]);
    assert_eq!(list[0][0].pos_s, 4_360);
    assert_eq!(list[0][0].video, video("1"));
    assert_eq!(&s.continue_watching, list[0]);
}

#[test]
fn the_saved_place_is_what_the_channel_page_resumes_from() {
    let (s, _) = press(&playing_at(&page(Vec::new()), 4_360), &[Intent::Back]);
    assert_eq!(s.focused_progress().map(|p| p.pos_s), Some(4_360));
    let (resumed, cmds) = reduce(&s, Intent::Select);
    assert!(cmds.iter().any(|c| matches!(
        c,
        Command::PlayVideo {
            start_ms: 4_360_000,
            ..
        }
    )));
    assert_eq!(
        resumed.toasts[0].kind,
        ToastKind::Resumed { pos_s: 4_360 },
        "Resumed at 1:12:40"
    );
}

#[test]
fn the_newest_goes_first_and_the_same_video_is_never_listed_twice() {
    let s = page(vec![saved("9", 100), saved("1", 50), saved("8", 100)]);
    let (s, cmds) = press(&playing_at(&s, 900), &[Intent::Back]);
    assert_eq!(ids(&s.continue_watching), ["1", "9", "8"]);
    assert_eq!(s.continue_watching[0].pos_s, 900);
    assert_eq!(saves(&cmds).len(), 1);
}

#[test]
fn at_most_ten_the_oldest_falls_off() {
    let list: Vec<Progress> = (10..20).map(|i| saved(&i.to_string(), 60)).collect();
    assert_eq!(list.len(), CONTINUE_MAX);
    let (s, cmds) = press(&playing_at(&page(list), 900), &[Intent::Back]);
    assert_eq!(s.continue_watching.len(), CONTINUE_MAX);
    assert_eq!(
        ids(&s.continue_watching),
        ["1", "10", "11", "12", "13", "14", "15", "16", "17", "18"]
    );
    assert_eq!(saves(&cmds)[0].len(), CONTINUE_MAX);
}

#[test]
fn a_video_left_within_two_minutes_of_its_end_is_dropped() {
    let s = page(vec![saved("1", 900), saved("2", 60)]);
    // 119 s from the end: watched. 121 s: not.
    let (s_near, cmds) = press(&playing_at(&s, 15_128 - 119), &[Intent::Back]);
    assert_eq!(ids(&s_near.continue_watching), ["2"]);
    assert_eq!(saves(&cmds).len(), 1);
    let (s_far, _) = press(&playing_at(&s, 15_128 - 121), &[Intent::Back]);
    assert_eq!(ids(&s_far.continue_watching), ["1", "2"]);
}

#[test]
fn a_video_left_at_the_very_start_changes_nothing() {
    let s = page(vec![saved("1", 900)]);
    let (left, cmds) = press(&playing_at(&s, 4), &[Intent::Back]);
    assert_eq!(left.continue_watching, vec![saved("1", 900)]);
    assert!(saves(&cmds).is_empty(), "nothing to write");
    let (left, cmds) = press(&playing_at(&page(Vec::new()), 4), &[Intent::Back]);
    assert!(left.continue_watching.is_empty() && saves(&cmds).is_empty());
}

#[test]
fn a_video_that_ends_leaves_the_list() {
    let s = playing_at(&page(vec![saved("1", 900), saved("2", 60)]), 15_000);
    let (ended, cmds) = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT });
    assert_eq!(ids(&ended.continue_watching), ["2"]);
    assert_eq!(saves(&cmds), [&ended.continue_watching]);
    assert!(ended.video.as_ref().unwrap().end_card.is_some());
    // Back on the end card does not bring it back.
    let (back, _) = reduce(&ended, Intent::Back);
    assert_eq!(ids(&back.continue_watching), ["2"]);
}

#[test]
fn watch_again_and_leaving_early_saves_the_new_place() {
    let s = playing_at(&page(Vec::new()), 15_000);
    let (s, _) = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT });
    let (s, _) = press(&s, &[Intent::Select]); // Watch again
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(&s, health(600_000)).0;
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.continue_watching.len(), 1);
    assert_eq!(s.continue_watching[0].pos_s, 600);
}

#[test]
fn clips_are_never_saved() {
    let s = page(Vec::new());
    let (s, _) = press(&s, &[Intent::PageRight, Intent::Select]);
    assert!(s.video.as_ref().is_some_and(|v| v.item.is_clip()));
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let s = reduce(
        &s,
        Event::PlayerHealth {
            slot: MAIN_SLOT,
            at_ms: 0,
            health: Health {
                stalled: false,
                buffer_ms: 1,
                pos_ms: Some(15_000),
                length_ms: Some(28_000),
            },
        },
    )
    .0;
    let (left, cmds) = reduce(&s, Intent::Back);
    assert!(left.continue_watching.is_empty());
    assert!(saves(&cmds).is_empty());
    // Ending one changes nothing either.
    let (ended, cmds) = reduce(&s, Event::PlaybackEnded { slot: MAIN_SLOT });
    assert!(ended.continue_watching.is_empty() && saves(&cmds).is_empty());
}

#[test]
fn the_end_cards_channel_videos_saves_too() {
    let s = playing_at(&page(Vec::new()), 900);
    let (s, cmds) = press(&s, &[Intent::Menu]);
    assert!(s.player_menu.is_some());
    // The menu's last row, *Channel videos*.
    let rows = s.player_menu_rows();
    let last = rows.len() - 1;
    let mut s = s;
    for _ in 0..last {
        s = reduce(&s, Intent::Down).0;
    }
    let (s, cmds2) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.continue_watching.len(), 1);
    assert_eq!(saves(&cmds2).len(), 1);
    assert!(saves(&cmds).is_empty());
}

#[test]
fn the_window_closing_saves_a_video_on_screen_and_nothing_else() {
    let s = playing_at(&page(Vec::new()), 900);
    let (closing, cmds) = reduce(&s, Event::Quitting);
    assert_eq!(closing.continue_watching.len(), 1);
    assert_eq!(saves(&cmds).len(), 1);
    // Still on screen: nothing breaks if more events follow.
    assert!(closing.video.is_some());

    let (idle, cmds) = reduce(&home(Vec::new()), Event::Quitting);
    assert!(idle.continue_watching.is_empty() && cmds.is_empty());
}

#[test]
fn quitting_from_the_prompt_saves_first() {
    let mut s = playing_at(&page(Vec::new()), 900);
    s.exit_prompt = true;
    let (_, cmds) = reduce(&s, Intent::Select);
    let at = |f: fn(&Command) -> bool| cmds.iter().position(f).unwrap();
    assert!(
        at(|c| matches!(c, Command::SaveContinue(_))) < at(|c| *c == Command::Quit),
        "{cmds:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// A video that is gone
// ---------------------------------------------------------------------------------------------

#[test]
fn a_video_that_is_gone_leaves_the_list_and_stays_out_after_back() {
    let gone = || Event::VideoGone { id: "1".into() };
    let error = || Event::PlaybackError {
        slot: MAIN_SLOT,
        message: "This video could not be played.".into(),
    };
    let s = page(vec![saved("1", 900), saved("2", 60)]);
    let (s, _) = reduce(&s, Intent::Select);
    // Twitch's answer and the player's error, in either order.
    for events in [vec![gone(), error()], vec![error(), gone()]] {
        let (failed, cmds) = events
            .into_iter()
            .fold((s.clone(), Vec::new()), |(s, mut all), e| {
                let (next, cmds) = reduce(&s, e);
                all.extend(cmds);
                (next, all)
            });
        assert_eq!(ids(&failed.continue_watching), ["2"]);
        assert_eq!(saves(&cmds).len(), 1);
        assert_eq!(
            failed.playback,
            Playback::Failed(VIDEO_GONE_MESSAGE.into()),
            "the screen says it is gone"
        );
        let (back, cmds) = reduce(&failed, Intent::Back);
        assert_eq!(ids(&back.continue_watching), ["2"], "not saved again");
        assert!(saves(&cmds).is_empty());
    }
}

#[test]
fn a_video_that_cannot_be_played_keeps_its_place_unless_twitch_says_it_is_gone() {
    // Streamlink's "no playable streams" is not proof: it also comes for a restriction or a
    // bad moment. The error shows and the saved place stays, also after B.
    let s = page(vec![saved("1", 900)]);
    let (s, _) = reduce(&s, Intent::Select);
    let (failed, cmds) = reduce(
        &s,
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "This video could not be played.".into(),
        },
    );
    assert_eq!(ids(&failed.continue_watching), ["1"]);
    assert!(saves(&cmds).is_empty());
    let (back, _) = reduce(&failed, Intent::Back);
    assert_eq!(ids(&back.continue_watching), ["1"]);
    // Another video's answer changes nothing.
    let (other, cmds) = reduce(&failed, Event::VideoGone { id: "2".into() });
    assert_eq!(other, failed);
    assert!(cmds.is_empty());
}

#[test]
fn any_other_failure_keeps_the_place() {
    let s = page(vec![saved("1", 900)]);
    let (s, _) = reduce(&s, Intent::Select);
    let (failed, cmds) = reduce(
        &s,
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "Twitch took too long to answer.".into(),
        },
    );
    assert_eq!(ids(&failed.continue_watching), ["1"]);
    assert!(saves(&cmds).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Home's row
// ---------------------------------------------------------------------------------------------

#[test]
fn home_shows_the_row_after_followed() {
    let s = home(vec![saved("1", 900), saved("2", 60)]);
    assert_eq!(
        home_kinds(&s),
        [
            RowKind::Featured,
            RowKind::FollowedLive,
            RowKind::ContinueWatching,
            RowKind::TopLive
        ]
    );
    let row = &s.home.rows[2];
    assert_eq!(row.tiles.len(), 2);
    assert!(matches!(&row.tiles[0], Tile::Video(v) if v.id == "1"));
    // No list, no row.
    assert!(!home_kinds(&home(Vec::new())).contains(&RowKind::ContinueWatching));
}

#[test]
fn without_a_followed_row_it_goes_after_the_hero() {
    let (mut s, _) = init();
    s.continue_watching = vec![saved("1", 900)];
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![
                Row::new(RowKind::Featured, vec![Tile::Stream(stream("hero"))]),
                Row::new(RowKind::TopLive, vec![Tile::Stream(stream("top"))]),
            ],
        },
    )
    .0;
    assert_eq!(
        home_kinds(&s),
        [
            RowKind::Featured,
            RowKind::ContinueWatching,
            RowKind::TopLive
        ]
    );
}

#[test]
fn a_reload_keeps_the_row() {
    let s = home(vec![saved("1", 900)]);
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(
                RowKind::TopLive,
                vec![Tile::Stream(stream("top"))],
            )],
        },
    )
    .0;
    assert_eq!(
        home_kinds(&s),
        [RowKind::ContinueWatching, RowKind::TopLive]
    );
}

#[test]
fn a_video_left_shows_on_home_without_a_reload() {
    // Home, then the channel page over it, then a video.
    let s = page(Vec::new());
    assert!(!home_kinds(&s).contains(&RowKind::ContinueWatching));
    let (left, cmds) = press(&playing_at(&s, 900), &[Intent::Back, Intent::Back]);
    assert_eq!(left.screen, Screen::Home);
    assert_eq!(
        home_kinds(&left),
        [
            RowKind::Featured,
            RowKind::FollowedLive,
            RowKind::ContinueWatching,
            RowKind::TopLive
        ]
    );
    assert!(!saves(&cmds).is_empty());
    // The focus stayed on the card it was on.
    assert_eq!(left.home.focus, Focus { row: 3, col: 0 });
}

#[test]
fn a_on_a_card_resumes_over_home_and_b_comes_back() {
    let s = home(vec![saved("1", 900), saved("2", 4_360)]);
    let (s, _) = press(&s, &[Intent::Down, Intent::Down, Intent::Right]);
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.id == "2"));
    let (playing, cmds) = reduce(&s, Intent::Select);
    assert_eq!(playing.screen, Screen::Player);
    assert_eq!(playing.now_playing, None);
    assert!(cmds.iter().any(|c| matches!(
        c,
        Command::PlayVideo {
            target: VideoTarget::Video(id),
            start_ms: 4_360_000,
            ..
        } if id == "2"
    )));
    assert_eq!(playing.toasts[0].kind, ToastKind::Resumed { pos_s: 4_360 });
    // The year of *Streamed Nov 10* has a clock to count from.
    assert!(playing.video.as_ref().unwrap().now > 0);
    let (back, _) = reduce(&playing, Intent::Back);
    assert_eq!(back.screen, Screen::Home);
    assert_eq!(back.home.focus, Focus { row: 2, col: 1 });
}

#[test]
fn x_removes_the_card_and_the_row_goes_with_the_last() {
    let s = home(vec![saved("1", 900), saved("2", 60)]);
    let (s, _) = press(&s, &[Intent::Down, Intent::Down]);
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(ids(&s.continue_watching), ["2"]);
    assert_eq!(saves(&cmds), [&s.continue_watching]);
    assert_eq!(s.home.rows[2].tiles.len(), 1);
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.id == "2"));
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert!(s.continue_watching.is_empty());
    assert_eq!(saves(&cmds).len(), 1);
    assert_eq!(
        home_kinds(&s),
        [RowKind::Featured, RowKind::FollowedLive, RowKind::TopLive]
    );
    // The focus lands on a card that exists.
    assert!(s.focused_tile().is_some());
}

#[test]
fn x_elsewhere_on_home_does_nothing_to_the_list() {
    let s = home(vec![saved("1", 900)]);
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(s.continue_watching.len(), 1);
    assert!(saves(&cmds).is_empty());
}

#[test]
fn x_on_a_saved_video_of_the_channel_page_starts_it_over() {
    let s = page(vec![saved("1", 900)]);
    assert!(s.focused_progress().is_some());
    let (s, cmds) = press(&s, &[Intent::Retry]);
    assert_eq!(s.screen, Screen::Player);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::PlayVideo { start_ms: 0, .. })));
}
