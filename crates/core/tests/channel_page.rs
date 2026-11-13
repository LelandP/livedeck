//! The channel page (vod-plan.md, phase 3): every way in (Y on a card of any browse screen, a
//! *Continue watching* card included, A on an offline channel, the live player's menu), Kick left
//! out, signed out, Back, the tabs and the subscribe card. A on a video or clip plays it (the player itself: `video_player.rs`).

use htpc_core::*;

fn live(login: &str) -> Stream {
    Stream {
        channel: ChannelRef::twitch(login),
        user_id: format!("id_{login}"),
        title: format!("{login} title"),
        category: "Cat".into(),
        viewers: 100,
        tint: Rgb::default(),
        thumbnail: None,
    }
}

fn kick_live(slug: &str) -> Stream {
    Stream {
        channel: ChannelRef::kick(slug),
        ..live(slug)
    }
}

fn offline(channel: ChannelRef) -> Tile {
    Tile::Channel(Channel {
        name: format!("{} (name)", channel.login),
        channel,
        tint: Rgb::default(),
    })
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

fn sign_in(s: &AppState) -> AppState {
    let user = User {
        login: "viewer".into(),
        can_chat: true,
    };
    reduce(s, Event::SignedIn(user)).0
}

/// Home, signed out: a Twitch stream, an offline Twitch channel, a Kick stream, an offline Kick
/// channel and a category, in that order, all in one row (focus starts on the first).
fn home() -> AppState {
    let (s, _) = init();
    let rows = vec![Row::new(
        RowKind::TopLive,
        vec![
            Tile::Stream(live("fox")),
            offline(ChannelRef::twitch("sleepy")),
            Tile::Stream(kick_live("napper")),
            offline(ChannelRef::kick("dozer")),
            Tile::Category(Category {
                id: "g1".into(),
                name: "Cat".into(),
                tint: Rgb::default(),
                thumbnail: None,
            }),
        ],
    )];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

fn rights(n: usize) -> Vec<Intent> {
    vec![Intent::Right; n]
}

fn video(id: &str, sub_only: bool) -> Video {
    Video {
        id: id.into(),
        channel: ChannelRef::twitch("sleepy"),
        channel_name: "Sleepy".into(),
        kind: VideoKind::Archive,
        title: format!("video {id}"),
        category: "Cat".into(),
        length_s: 3_600,
        created_at: 1_794_335_302,
        views: 10,
        tint: Rgb::default(),
        thumbnail: None,
        muted: Vec::new(),
        sub_only,
        recording: false,
    }
}

fn clip(slug: &str) -> Clip {
    Clip {
        slug: slug.into(),
        channel: ChannelRef::twitch("sleepy"),
        title: format!("clip {slug}"),
        category: "Cat".into(),
        length_s: 30,
        views: 5,
        clipped_by: "someone".into(),
        created_at: 1_794_335_302,
        tint: Rgb::default(),
        thumbnail: Some(format!("https://cdn/{slug}.jpg")),
    }
}

fn info(channel: &str, live_now: Option<Stream>) -> Box<ChannelInfo> {
    Box::new(ChannelInfo {
        channel: ChannelRef::twitch(channel),
        user_id: "77".into(),
        name: "Sleepy Fox".into(),
        avatar: Some("https://cdn/avatar.jpg".into()),
        live: live_now,
        last_live_at: Some(1_794_335_302),
        loaded_at: 1_794_346_102,
    })
}

/// What `LoadChannelPage` answers for an offline channel with all four rows.
fn loaded(channel: &str) -> Event {
    Event::ChannelPageLoaded {
        info: info(channel, None),
        rows: vec![
            Row::new(
                RowKind::PastBroadcasts,
                vec![
                    Tile::Video(video("1", false)),
                    Tile::Video(video("2", true)),
                    Tile::Video(video("3", false)),
                ],
            ),
            Row::new(RowKind::Highlights, vec![Tile::Video(video("4", false))]),
            Row::new(RowKind::ClipsWeek, vec![Tile::Clip(clip("a"))]),
            Row::new(
                RowKind::ClipsAllTime,
                vec![Tile::Clip(clip("b")), Tile::Clip(clip("c"))],
            ),
        ],
    }
}

/// The offline channel's page, opened from Home and loaded.
fn page() -> AppState {
    let (s, _) = press(&home(), &[Intent::Right, Intent::Select]);
    assert_eq!(s.screen, Screen::Channel);
    reduce(&s, loaded("sleepy")).0
}

fn load_command(channel: ChannelRef) -> Command {
    Command::LoadChannelPage { channel }
}

// ---------------------------------------------------------------------------------------------
// Every way in
// ---------------------------------------------------------------------------------------------

#[test]
fn y_on_a_live_twitch_card_opens_its_page_which_starts_out_live() {
    let (s, cmds) = reduce(&home(), Intent::ToggleChat);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("fox"))]);
    let page = s.channel_page.as_ref().expect("the page is open");
    assert_eq!(page.channel, ChannelRef::twitch("fox"));
    assert_eq!(page.name, "fox");
    assert!(page.live.is_some(), "the card is a live stream");
    assert_eq!(page.tab, ChannelTab::Videos);
    assert!(page.loading());
    // The stream is not played, and nothing live is joined.
    assert_eq!(s.now_playing, None);
}

#[test]
fn y_on_an_offline_twitch_card_opens_its_page() {
    let (s, cmds) = press(&home(), &[Intent::Right, Intent::ToggleChat]);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("sleepy"))]);
    let page = s.channel_page.as_ref().unwrap();
    assert_eq!(page.name, "sleepy (name)", "the card's name shows at once");
    assert!(page.live.is_none());
}

#[test]
fn a_on_an_offline_twitch_card_opens_its_page_instead_of_the_player() {
    let (s, cmds) = press(&home(), &[Intent::Right, Intent::Select]);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.now_playing, None);
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("sleepy"))]);
}

#[test]
fn a_on_an_offline_kick_card_still_tries_to_play() {
    let (s, cmds) = press(&home(), &{
        let mut keys = rights(3);
        keys.push(Intent::Select);
        keys
    });
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.playback, Playback::Loading);
    assert!(cmds.iter().any(
        |c| matches!(c, Command::Play { channel, .. } if *channel == ChannelRef::kick("dozer"))
    ));
    assert_eq!(s.channel_page, None);
}

#[test]
fn a_on_a_live_card_still_plays() {
    let (s, _) = reduce(&home(), Intent::Select);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.channel_page, None);
}

#[test]
fn y_works_signed_out_and_signed_in() {
    for signed_in in [false, true] {
        let mut s = home();
        if signed_in {
            s = sign_in(&s);
        }
        let (s, cmds) = reduce(&s, Intent::ToggleChat);
        assert_eq!(s.screen, Screen::Channel, "signed_in={signed_in}");
        assert_eq!(cmds, vec![load_command(ChannelRef::twitch("fox"))]);
    }
}

/// Home with a Followed row and a *Continue watching* row holding two of sleepy's broadcasts,
/// focus on the second card of that row.
fn home_on_a_continue_card() -> AppState {
    let (mut s, _) = init();
    s.continue_watching = ["1", "2"]
        .iter()
        .map(|id| Progress {
            video: video(id, false),
            pos_s: 900,
            saved_at: 1_794_400_000,
        })
        .collect();
    let rows = vec![Row::new(
        RowKind::FollowedLive,
        vec![Tile::Stream(live("fox"))],
    )];
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0;
    let (s, _) = press(&s, &[Intent::Down, Intent::Right]);
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.id == "2"));
    s
}

#[test]
fn y_on_a_continue_watching_card_opens_that_videos_channel_page() {
    let s = home_on_a_continue_card();
    let focus = s.home.focus;
    let (s, cmds) = reduce(&s, Intent::ToggleChat);
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("sleepy"))]);
    let page = s.channel_page.as_ref().expect("the page is open");
    assert_eq!(page.channel, ChannelRef::twitch("sleepy"));
    assert_eq!(
        page.name, "Sleepy",
        "the video's channel name shows at once"
    );
    assert!(page.live.is_none());
    assert_eq!(page.tab, ChannelTab::Videos);
    assert!(page.loading());
    // The video is not played and the saved places stay as they were.
    assert_eq!(s.video, None);
    assert_eq!(s.continue_watching.len(), 2);

    // B returns to Home with the card still focused, and saves nothing.
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.channel_page, None);
    assert_eq!(s.home.focus, focus);
    assert!(!cmds.iter().any(|c| matches!(c, Command::SaveContinue(_))));
}

#[test]
fn a_continue_watching_card_still_resumes_on_a_and_y_does_nothing_with_the_rail_open() {
    let s = home_on_a_continue_card();
    let (playing, _) = reduce(&s, Intent::Select);
    assert_eq!(playing.screen, Screen::Player);
    assert!(playing.video.is_some());
    assert_eq!(playing.channel_page, None);

    let (s, _) = press(&s, &[Intent::Left, Intent::Left]);
    assert!(s.rail.focused);
    let (after, cmds) = reduce(&s, Intent::ToggleChat);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}
#[test]
fn the_page_loads_signed_out_and_shows_all_its_rows() {
    let s = page();
    assert!(!s.is_signed_in());
    let page = s.channel_page.as_ref().unwrap();
    assert_eq!(page.name, "Sleepy Fox");
    assert_eq!(page.avatar.as_deref(), Some("https://cdn/avatar.jpg"));
    assert_eq!(page.last_live_at, Some(1_794_335_302));
    assert_eq!(page.now, 1_794_346_102);
    assert!(!page.loading());
    assert_eq!(page.error(), None);
    let kinds = |feed: &Feed| feed.rows.iter().map(|r| r.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds(&page.videos),
        [RowKind::PastBroadcasts, RowKind::Highlights]
    );
    assert_eq!(
        kinds(&page.clips),
        [RowKind::ClipsWeek, RowKind::ClipsAllTime]
    );
}

/// The screens with cards of channels, each ready with a Twitch stream and an offline Twitch
/// channel (and signed in where the screen needs it).
fn screens() -> Vec<(&'static str, AppState, Screen)> {
    let stream_row = |kind| Row::new(kind, vec![Tile::Stream(live("fox"))]);
    let off_row = |kind| Row::new(kind, vec![offline(ChannelRef::twitch("sleepy"))]);
    let mut out = Vec::new();

    out.push(("Home", home(), Screen::Home));

    // A category.
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(
                RowKind::Categories,
                vec![Tile::Category(Category {
                    id: "g1".into(),
                    name: "Cat".into(),
                    tint: Rgb::default(),
                    thumbnail: None,
                })],
            )],
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Category);
    let rows = grid_rows(
        RowKind::CategoryStreams,
        vec![Tile::Stream(live("fox"))],
        Paging::End,
    );
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Category("g1".into()),
            rows,
        },
    )
    .0;
    out.push(("a category", s, Screen::Category));

    // Following.
    let s = sign_in(&home());
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Following));
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Following,
            rows: vec![
                stream_row(RowKind::FollowedLive),
                off_row(RowKind::FollowedOffline),
            ],
        },
    )
    .0;
    out.push(("Following", s, Screen::Following));

    // Favorites.
    let mut s = sign_in(&home());
    s.favorites = vec![ChannelRef::twitch("fox"), ChannelRef::twitch("sleepy")];
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Favorites));
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Favorites,
            rows: vec![
                stream_row(RowKind::FavoritesLive),
                off_row(RowKind::FavoritesOffline),
            ],
        },
    )
    .0;
    out.push(("Favorites", s, Screen::Favorites));

    // Search results.
    let mut s = sign_in(&home());
    s.search.query = "fox".into();
    s.search.asked = "fox".into();
    s.search.id = 1;
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Search));
    let s = reduce(
        &s,
        Event::SearchResults {
            id: 1,
            rows: vec![
                stream_row(RowKind::SearchLive),
                off_row(RowKind::SearchOffline),
            ],
        },
    )
    .0;
    let mut s = s;
    s.search.in_results = true;
    out.push(("Search", s, Screen::Search));
    out
}

#[test]
fn y_and_a_open_the_page_on_every_browse_screen_and_b_returns_there() {
    for (name, s, screen) in screens() {
        assert_eq!(s.screen, screen, "{name}");
        // Y on the live stream's card.
        let (page, cmds) = reduce(&s, Intent::ToggleChat);
        assert_eq!(page.screen, Screen::Channel, "{name}: Y on a stream");
        assert_eq!(
            cmds,
            vec![load_command(ChannelRef::twitch("fox"))],
            "{name}"
        );
        let (back, _) = reduce(&page, Intent::Back);
        assert_eq!(back.screen, screen, "{name}: B");
        assert_eq!(back.channel_page, None, "{name}: B forgets the page");
        // Y and A on the offline channel's card (the second row on Following, Favorites and
        // Search, the second card on the rest).
        let to_offline: Vec<Intent> = if matches!(
            screen,
            Screen::Following | Screen::Favorites | Screen::Search
        ) {
            vec![Intent::Down]
        } else {
            vec![]
        };
        if screen == Screen::Category {
            // The category's grid has the live card only.
            continue;
        }
        for key in [Intent::ToggleChat, Intent::Select] {
            let mut keys = to_offline.clone();
            if screen == Screen::Home {
                keys.push(Intent::Right);
            }
            keys.push(key);
            let (page, cmds) = press(&s, &keys);
            assert_eq!(page.screen, Screen::Channel, "{name}: {key:?} offline");
            assert_eq!(
                cmds,
                vec![load_command(ChannelRef::twitch("sleepy"))],
                "{name}"
            );
            let (back, _) = reduce(&page, Intent::Back);
            assert_eq!(back.screen, screen, "{name}: B");
        }
    }
}

#[test]
fn the_live_players_menu_opens_the_page_and_stops_the_stream() {
    let (s, _) = reduce(&home(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::ChannelVideos));
    // Left and Right do nothing on an action row.
    let (still, cmds) = press(&s, &[Intent::Left, Intent::Right]);
    assert_eq!(still.screen, Screen::Player);
    assert!(cmds.is_empty(), "{cmds:?}");

    let (page, cmds) = reduce(&s, Intent::Select);
    assert_eq!(page.screen, Screen::Channel);
    assert_eq!(page.back_stack, vec![Screen::Home]);
    assert_eq!(page.now_playing, None);
    assert_eq!(page.playback, Playback::Idle);
    assert_eq!(page.player_menu, None);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::LeaveChat));
    assert!(cmds.contains(&load_command(ChannelRef::twitch("fox"))));
    assert!(
        page.channel_page.as_ref().unwrap().live.is_some(),
        "it was playing"
    );

    // B returns to the screen the stream was opened from.
    let (back, _) = reduce(&page, Intent::Back);
    assert_eq!(back.screen, Screen::Home);
    assert_eq!(back.channel_page, None);
}

#[test]
fn the_menu_row_does_nothing_for_kick() {
    let (s, _) = press(&home(), &{
        let mut keys = rights(2);
        keys.push(Intent::Select);
        keys
    });
    assert_eq!(s.screen, Screen::Player);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::ChannelVideos));
    let (after, cmds) = reduce(&s, Intent::Select);
    assert_eq!(after, s);
    assert!(cmds.is_empty(), "{cmds:?}");
}

#[test]
fn the_page_opened_from_the_player_of_an_offline_stream_is_not_live() {
    let (s, _) = reduce(&home(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let s = reduce(&s, Event::PlaybackEnded { slot: 0 }).0;
    assert_eq!(s.playback, Playback::Offline);
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    let (page, _) = reduce(&s, Intent::Select);
    assert_eq!(page.screen, Screen::Channel);
    assert!(page.channel_page.unwrap().live.is_none());
}

// ---------------------------------------------------------------------------------------------
// Kick
// ---------------------------------------------------------------------------------------------

#[test]
fn y_on_a_kick_card_does_nothing_at_all() {
    // The Kick stream, then the offline Kick channel.
    for steps in [2, 3] {
        let (s, _) = press(&home(), &rights(steps));
        let (after, cmds) = reduce(&s, Intent::ToggleChat);
        assert_eq!(after, s, "Y on card {steps}");
        assert!(cmds.is_empty(), "{cmds:?}");
    }
    assert!(!Platform::Kick.features().videos);
    assert!(Platform::Twitch.features().videos);
}

#[test]
fn y_on_a_category_or_with_the_rail_open_does_nothing() {
    let (s, _) = press(&home(), &rights(4));
    assert!(matches!(s.focused_tile(), Some(Tile::Category(_))));
    let (after, cmds) = reduce(&s, Intent::ToggleChat);
    assert_eq!(after, s);
    assert!(cmds.is_empty());

    // Left from the first card opens the rail.
    let (s, _) = press(&home(), &[Intent::Left]);
    assert!(s.rail.focused);
    let (after, cmds) = reduce(&s, Intent::ToggleChat);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

#[test]
fn the_loaded_page_asks_for_its_images() {
    let (s, _) = reduce(&home(), Intent::ToggleChat);
    let (_, cmds) = reduce(&s, loaded("fox"));
    let urls: Vec<&String> = cmds
        .iter()
        .filter_map(|c| match c {
            Command::FetchThumbnails { urls } => Some(urls),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        urls[0].as_str(),
        "https://cdn/avatar.jpg",
        "the picture first"
    );
    assert!(urls.iter().any(|u| u.as_str() == "https://cdn/a.jpg"));
}

#[test]
fn the_live_stream_comes_with_the_page() {
    let (s, _) = press(&home(), &[Intent::Right, Intent::ToggleChat]);
    let now_live = Event::ChannelPageLoaded {
        info: info("sleepy", Some(live("sleepy"))),
        rows: vec![Row::new(
            RowKind::LiveNow,
            vec![Tile::Stream(live("sleepy"))],
        )],
    };
    let (s, _) = reduce(&s, now_live);
    let page = s.channel_page.as_ref().unwrap();
    assert!(page.live.is_some());
    assert_eq!(page.videos.rows[0].kind, RowKind::LiveNow);
    assert!(page.clips.rows.is_empty());
}

#[test]
fn a_late_answer_for_another_page_is_dropped() {
    let (s, _) = reduce(&home(), Intent::ToggleChat);
    let before = s.clone();
    let (after, cmds) = reduce(&s, loaded("someone_else"));
    assert_eq!(after, before);
    assert!(cmds.is_empty());
    let failed = Event::ChannelPageFailed {
        channel: ChannelRef::twitch("someone_else"),
        message: "no".into(),
    };
    assert_eq!(reduce(&s, failed).0, before);

    // And one that arrives after the page was left.
    let (left, _) = reduce(&s, Intent::Back);
    let (after, cmds) = reduce(&left, loaded("fox"));
    assert_eq!(after, left);
    assert!(cmds.is_empty());
}

#[test]
fn a_failed_load_shows_the_error_and_x_asks_again() {
    let (s, _) = press(&home(), &[Intent::Right, Intent::Select]);
    let failed = Event::ChannelPageFailed {
        channel: ChannelRef::twitch("sleepy"),
        message: "network error".into(),
    };
    let (s, _) = reduce(&s, failed);
    let page = s.channel_page.as_ref().unwrap();
    assert_eq!(page.error(), Some("network error"));
    assert!(!page.loading());

    let (s, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("sleepy"))]);
    let page = s.channel_page.as_ref().unwrap();
    assert_eq!(page.error(), None);
    assert!(page.loading());
    // X again while it loads asks for nothing more.
    let (_, cmds) = reduce(&s, Intent::Retry);
    assert!(cmds.is_empty());

    let (s, _) = reduce(&s, loaded("sleepy"));
    assert_eq!(s.channel_page.unwrap().videos.rows.len(), 2);
}

// ---------------------------------------------------------------------------------------------
// Tabs and focus
// ---------------------------------------------------------------------------------------------

#[test]
fn lb_and_rb_switch_tab_and_each_tab_keeps_its_focus() {
    let s = page();
    // On Videos: down to Highlights.
    let (s, _) = press(&s, &[Intent::Right, Intent::Down]);
    assert_eq!(
        s.channel_page.as_ref().unwrap().videos.focus,
        Focus { row: 1, col: 0 }
    );
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.id == "4"));

    // RB to Clips, down and right to the second all-time clip.
    let (s, _) = press(&s, &[Intent::PageRight]);
    assert_eq!(s.channel_page.as_ref().unwrap().tab, ChannelTab::Clips);
    let (s, _) = press(&s, &[Intent::Down, Intent::Right, Intent::Right]);
    assert!(matches!(s.focused_tile(), Some(Tile::Clip(c)) if c.slug == "c"));

    // LB back to Videos: the Highlights card is still focused. (The first row's column was
    // 1 when it was left.)
    let (s, _) = press(&s, &[Intent::PageLeft]);
    assert_eq!(s.channel_page.as_ref().unwrap().tab, ChannelTab::Videos);
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.id == "4"));
    let (s, _) = press(&s, &[Intent::Up]);
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.id == "2"));

    // And Clips remembers its own.
    let (s, _) = press(&s, &[Intent::PageRight]);
    assert!(matches!(s.focused_tile(), Some(Tile::Clip(c)) if c.slug == "c"));
}

#[test]
fn the_d_pad_moves_over_the_rows_and_left_opens_the_rail() {
    let s = page();
    let (s, _) = press(
        &s,
        &[Intent::Right, Intent::Right, Intent::Right, Intent::Right],
    );
    assert_eq!(
        s.channel_page.as_ref().unwrap().videos.focus.col,
        2,
        "stops at the end"
    );
    let (s, _) = press(&s, &[Intent::Down, Intent::Down, Intent::Down]);
    assert_eq!(
        s.channel_page.as_ref().unwrap().videos.focus.row,
        1,
        "stops at the last row"
    );
    // Each row keeps its column.
    let (s, _) = press(&s, &[Intent::Up]);
    assert_eq!(s.channel_page.as_ref().unwrap().videos.focus.col, 2);

    let (s, _) = press(&s, &[Intent::Left, Intent::Left, Intent::Left]);
    assert!(s.rail.focused, "Left from the first card opens the rail");
    assert_eq!(s.screen, Screen::Channel);
}

#[test]
fn a_card_click_selects_it() {
    let (s, _) = press(&page(), &[Intent::PageRight]);
    // A click moves the focus and plays the clip, as A does.
    let (s, cmds) = reduce(&s, Intent::Pick { row: 1, col: 1 });
    assert_eq!(
        s.channel_page.as_ref().unwrap().clips.focus,
        Focus { row: 1, col: 1 }
    );
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(
        cmds,
        vec![Command::PlayVideo {
            slot: MAIN_SLOT,
            target: VideoTarget::Clip("c".into()),
            start_ms: 0,
            stream: StreamKind::Video(Quality::Best),
        }]
    );
}

#[test]
fn view_does_not_open_the_multiview_popup_on_the_page() {
    let (s, _) = press(&home(), &[Intent::ToggleChat]);
    let now_live = Event::ChannelPageLoaded {
        info: info("fox", Some(live("fox"))),
        rows: vec![Row::new(RowKind::LiveNow, vec![Tile::Stream(live("fox"))])],
    };
    let (s, _) = reduce(&s, now_live);
    let s = sign_in(&s);
    let (after, cmds) = reduce(&s, Intent::View);
    assert_eq!(after.popup, None);
    assert!(cmds.is_empty());
}

// ---------------------------------------------------------------------------------------------
// A
// ---------------------------------------------------------------------------------------------

#[test]
fn a_on_the_live_card_plays_and_b_comes_back_to_the_page() {
    let (s, _) = reduce(&home(), Intent::ToggleChat);
    let now_live = Event::ChannelPageLoaded {
        info: info("fox", Some(live("fox"))),
        rows: vec![
            Row::new(RowKind::LiveNow, vec![Tile::Stream(live("fox"))]),
            Row::new(
                RowKind::PastBroadcasts,
                vec![Tile::Video(video("1", false))],
            ),
        ],
    };
    let (s, _) = reduce(&s, now_live);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.back_stack, vec![Screen::Home, Screen::Channel]);
    assert!(cmds.iter().any(
        |c| matches!(c, Command::Play { channel, .. } if *channel == ChannelRef::twitch("fox"))
    ));

    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Channel);
    assert!(s.channel_page.is_some(), "the page is still there");
    assert_eq!(s.now_playing, None);
}

#[test]
fn the_menu_row_from_a_stream_opened_on_the_page_returns_to_that_page() {
    let (s, _) = reduce(&home(), Intent::ToggleChat);
    let now_live = Event::ChannelPageLoaded {
        info: info("fox", Some(live("fox"))),
        rows: vec![Row::new(RowKind::LiveNow, vec![Tile::Stream(live("fox"))])],
    };
    let (s, _) = reduce(&s, now_live);
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    let (s, cmds) = reduce(&s, Intent::Select);
    // The page that was underneath is reused: no second one on the stack.
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    assert_eq!(cmds.last(), Some(&load_command(ChannelRef::twitch("fox"))));
    assert_eq!(
        s.channel_page.as_ref().unwrap().channel,
        ChannelRef::twitch("fox")
    );
}

#[test]
fn a_on_a_video_or_clip_plays_it_over_the_page() {
    // The video player itself is tested in `video_player.rs` and `seeking.rs`.
    let s = page();
    let (after, cmds) = reduce(&s, Intent::Select);
    assert_eq!(after.screen, Screen::Player);
    assert_eq!(after.back_stack, vec![Screen::Home, Screen::Channel]);
    assert!(matches!(
        after.video.as_ref().map(|v| &v.item),
        Some(VideoItem::Video(v)) if v.id == "1"
    ));
    assert_eq!(after.now_playing, None);
    assert!(
        matches!(cmds[0], Command::PlayVideo { start_ms: 0, .. }),
        "{cmds:?}"
    );
    let (s, _) = press(&s, &[Intent::PageRight]);
    let (after, cmds) = reduce(&s, Intent::Select);
    assert_eq!(after.screen, Screen::Player);
    assert!(matches!(
        &cmds[..],
        [Command::PlayVideo { target: VideoTarget::Clip(slug), .. }] if slug == "a"
    ));
    // X does nothing without a saved place.
    let (after, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Subscriber-only videos
// ---------------------------------------------------------------------------------------------

fn on_the_sub_only_video(can_open_browser: bool) -> AppState {
    let mut s = page();
    s.can_open_browser = can_open_browser;
    let (s, _) = press(&s, &[Intent::Right]);
    assert!(matches!(s.focused_tile(), Some(Tile::Video(v)) if v.sub_only));
    s
}

#[test]
fn a_on_a_subscriber_only_video_opens_the_subscribe_card() {
    let (s, cmds) = reduce(&on_the_sub_only_video(true), Intent::Select);
    assert!(s.subscribe_card);
    assert_eq!(s.screen, Screen::Channel);
    assert!(cmds.is_empty(), "{cmds:?}");
    assert_eq!(
        s.subscribe_channel(),
        Some(&ChannelRef::twitch("sleepy")),
        "the card is for the page's channel"
    );
}

#[test]
fn the_card_opens_the_browser_where_there_is_one_and_b_closes_it() {
    let (open, _) = reduce(&on_the_sub_only_video(true), Intent::Select);
    let (s, cmds) = reduce(&open, Intent::Select);
    assert_eq!(
        cmds,
        vec![Command::OpenUrl("https://www.twitch.tv/subs/sleepy".into())]
    );
    assert!(s.subscribe_card);

    let (s, _) = reduce(&s, Intent::Back);
    assert!(!s.subscribe_card);
    assert_eq!(s.screen, Screen::Channel, "B closes the card, not the page");

    // In Game Mode A does nothing.
    let (open, _) = reduce(&on_the_sub_only_video(false), Intent::Select);
    let (s, cmds) = reduce(&open, Intent::Select);
    assert!(cmds.is_empty());
    assert!(s.subscribe_card);
}

#[test]
fn the_card_swallows_the_other_buttons_and_goes_with_the_page() {
    let (open, _) = reduce(&on_the_sub_only_video(true), Intent::Select);
    let (s, cmds) = press(
        &open,
        &[
            Intent::Up,
            Intent::Down,
            Intent::Left,
            Intent::Right,
            Intent::PageRight,
            Intent::Retry,
            Intent::ToggleChat,
        ],
    );
    assert_eq!(s, open);
    assert!(cmds.is_empty());

    // A rail click while the card is up leaves the page and the card.
    let (s, _) = reduce(&open, Intent::RailPick(RailEntry::Browse));
    assert_eq!(s.screen, Screen::Browse);
    assert!(!s.subscribe_card);
    assert_eq!(s.channel_page, None);
}

// ---------------------------------------------------------------------------------------------
// Continue watching (empty until phase 5, but the page reads it)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_saved_place_is_read_for_resume_and_start_over() {
    let mut s = page();
    assert!(s.continue_watching.is_empty(), "nothing fills it yet");
    assert!(s.focused_progress().is_none());

    s.continue_watching = vec![Progress {
        video: video("1", false),
        pos_s: 1_800,
        saved_at: 1,
    }];
    let p = s
        .focused_progress()
        .expect("the first video was left half way");
    assert_eq!((p.pos_s, p.permille()), (1_800, 500));
    assert_eq!(s.progress_of("2"), None);

    // On another video there is no saved place.
    let (s, _) = press(&s, &[Intent::Right]);
    assert!(s.focused_progress().is_none());
}

// ---------------------------------------------------------------------------------------------
// Back and leaving
// ---------------------------------------------------------------------------------------------

#[test]
fn back_goes_to_where_it_was_opened_and_forgets_the_page() {
    let s = page();
    let (back, cmds) = reduce(&s, Intent::Back);
    assert_eq!(back.screen, Screen::Home);
    assert_eq!(back.channel_page, None);
    assert!(back.back_stack.is_empty());
    assert!(cmds.is_empty(), "{cmds:?}");
    // Home is as it was: focus on the offline card the page was opened from.
    assert_eq!(back.home.focus, Focus { row: 0, col: 1 });
}

#[test]
fn b_with_the_rail_open_asks_to_quit_as_on_a_category() {
    let (s, _) = press(&page(), &[Intent::Left]);
    assert!(s.rail.focused);
    let (s, _) = reduce(&s, Intent::Back);
    assert!(s.exit_prompt);
    assert_eq!(s.screen, Screen::Channel);
}

#[test]
fn a_rail_entry_leaves_the_page() {
    let (s, _) = press(&page(), &[Intent::Left, Intent::Left]);
    assert!(s.rail.focused);
    // The rail opens on Home's entry, one down is Following (signed out: the login).
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Browse));
    assert_eq!(s.screen, Screen::Browse);
    assert_eq!(s.channel_page, None);
    assert!(s.back_stack.is_empty());
}

#[test]
fn signing_out_on_the_page_keeps_it() {
    let (s, _) = reduce(&sign_in(&home()), Intent::ToggleChat);
    let (s, _) = reduce(&s, loaded("fox"));
    let (s, _) = reduce(&s, Event::SignedOut);
    assert_eq!(s.screen, Screen::Channel);
    assert!(s.channel_page.is_some());
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
}

#[test]
fn signing_out_fixes_a_way_back_that_needed_the_account() {
    let s = sign_in(&home());
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Following));
    let rows = vec![Row::new(
        RowKind::FollowedLive,
        vec![Tile::Stream(live("fox"))],
    )];
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Following,
            rows,
        },
    );
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert_eq!(s.back_stack, vec![Screen::Following]);
    let (s, _) = reduce(&s, Event::SignedOut);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    let (s, _) = press(&s, &[Intent::Back]);
    assert_eq!(s.screen, Screen::Home);
}

#[test]
fn a_feed_refresh_leaves_the_page_alone() {
    let s = page();
    let (after, cmds) = reduce(&s, Event::FeedRefreshDue);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}
