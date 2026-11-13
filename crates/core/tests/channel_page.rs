//! The channel page (vod-plan.md, phase 3): every way in (Y on a card of any browse screen, a
//! *Continue watching* card included, A on an offline channel, the live player's menu), Kick left
//! out, signed out, Back, the tabs and the subscribe card. A on a video or clip plays it (the player itself: `video_player.rs`).
//! View adds the channel to Favorites or removes it, signed out too.

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

/// `fox` playing on the Player screen with the menu on its *Channel videos* row, opened from Home.
fn menu_on_channel_videos(s: &AppState) -> AppState {
    let (s, _) = reduce(s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::ChannelVideos));
    s
}

/// The channel page of `fox`, opened from the player's menu over the stream (still playing),
/// signed in or not, and loaded (a broadcast, a sub-only one and a clip of `sleepy`'s sort).
fn page_over_the_stream(signed_in: bool) -> AppState {
    let home = if signed_in { sign_in(&home()) } else { home() };
    let s = menu_on_channel_videos(&home);
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Channel);
    reduce(&s, loaded("fox")).0
}

#[test]
fn the_live_players_menu_opens_the_page_over_the_stream_which_keeps_playing() {
    let s = menu_on_channel_videos(&home());
    // Left and Right do nothing on an action row.
    let (still, cmds) = press(&s, &[Intent::Left, Intent::Right]);
    assert_eq!(still.screen, Screen::Player);
    assert!(cmds.is_empty(), "{cmds:?}");

    let (page, cmds) = reduce(&s, Intent::Select);
    assert_eq!(page.screen, Screen::Channel);
    // The Player stays on the stack under the page.
    assert_eq!(page.back_stack, vec![Screen::Home, Screen::Player]);
    assert_eq!(page.now_playing.as_ref().unwrap().channel.login, "fox");
    assert_eq!(page.playback, Playback::Playing);
    assert_eq!(page.player_menu, None);
    // Only the page is asked for: the player, chat and the rest are left alone.
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("fox"))]);
    assert!(page.live_covered());
    assert!(
        page.channel_page.as_ref().unwrap().live.is_some(),
        "it was playing"
    );
}

#[test]
fn b_on_the_page_goes_straight_back_to_the_playing_stream_with_no_commands() {
    let (s, _) = reduce(&home(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    // Chat shown, a pause, the stats box: all as they were.
    let (s, _) = press(&s, &[Intent::ToggleChat, Intent::Select]);
    assert!(s.chat_open && s.paused);
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    let (page, _) = reduce(&s, Intent::Select);
    let page = reduce(&page, loaded("fox")).0;

    let (back, cmds) = reduce(&page, Intent::Back);
    assert!(cmds.is_empty(), "nothing reloads: {cmds:?}");
    assert_eq!(back.screen, Screen::Player);
    assert_eq!(back.back_stack, vec![Screen::Home]);
    assert_eq!(back.channel_page, None);
    assert_eq!(back.now_playing, page.now_playing);
    assert_eq!(back.playback, Playback::Playing);
    assert!(back.chat_open && back.paused, "the HUD state is as it was");
    assert_ne!(back.hud_pulse, page.hud_pulse, "the HUD shows again");

    // And B on the Player is the usual one: the stream stops, Home is back.
    let (home, cmds) = reduce(&back, Intent::Back);
    assert_eq!(home.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::LeaveChat));
}

#[test]
fn the_stream_behind_the_page_is_still_followed() {
    let s = page_over_the_stream(false);
    assert!(s.stream_on_screen());
    // The player's own reports still count: the end of the stream is the offline screen.
    let (s, cmds) = reduce(&s, Event::PlaybackEnded { slot: 0 });
    assert_eq!(s.playback, Playback::Offline);
    assert_eq!(cmds, vec![Command::ScheduleOfflineRecheck { id: 1 }]);
    assert_eq!(s.screen, Screen::Channel, "the page stays up");
    // The quiet recheck runs behind the page, and finds the stream back.
    let (s, cmds) = reduce(&s, Event::OfflineRecheckDue(1));
    assert!(cmds.iter().any(|c| matches!(c, Command::Play { .. })));
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    assert_eq!(s.playback, Playback::Playing);
    // A hype train for the channel shows when the Player is back.
    let train = HypeTrain {
        level: 2,
        progress: 400,
        goal: 1800,
        seconds_left: 297,
    };
    let (s, _) = reduce(
        &s,
        Event::HypeTrain {
            channel: "fox".into(),
            train: Some(train),
        },
    );
    assert_eq!(s.hype_train, Some(train));
    let (back, cmds) = reduce(&s, Intent::Back);
    assert!(cmds.is_empty());
    assert_eq!(back.screen, Screen::Player);
    assert!(back.hype_train.is_some());
}

#[test]
fn a_stream_that_went_offline_behind_the_page_shows_the_offline_screen_on_return() {
    let s = page_over_the_stream(false);
    let (s, _) = reduce(&s, Event::PlaybackEnded { slot: 0 });
    let (back, cmds) = reduce(&s, Intent::Back);
    assert!(cmds.is_empty());
    assert_eq!(back.screen, Screen::Player);
    assert_eq!(back.playback, Playback::Offline);
    // A on it tries again, as on any offline stream.
    let (_, cmds) = reduce(&back, Intent::Select);
    assert!(cmds.iter().any(|c| matches!(c, Command::Play { .. })));
}

#[test]
fn a_raid_behind_the_page_waits_and_its_card_counts_down_on_return() {
    let s = page_over_the_stream(true);
    let (s, cmds) = reduce(
        &s,
        Event::Raided {
            from_login: "fox".into(),
            to_login: "next_up".into(),
            to_name: "Next Up".into(),
            viewers: 50,
        },
    );
    let id = s.raid.as_ref().expect("the offer is kept").id;
    assert!(cmds.is_empty(), "no countdown behind the page: {cmds:?}");
    assert_eq!(s.screen, Screen::Channel);
    // The page takes its buttons as usual: A on the focused card does not follow the raid.
    let (still, _) = press(&s, &[Intent::PageRight]);
    assert!(still.raid.is_some());
    assert_eq!(still.now_playing.as_ref().unwrap().channel.login, "fox");

    let (back, cmds) = reduce(&s, Intent::Back);
    assert_eq!(back.screen, Screen::Player);
    assert_eq!(cmds, vec![Command::StartRaidCountdown { id, seconds: 10 }]);
    assert_eq!(back.raid.as_ref().unwrap().seconds_left, 10);
    // From there it is the usual card: A follows.
    let (followed, cmds) = reduce(&back, Intent::Select);
    assert_eq!(followed.now_playing.unwrap().channel.login, "next_up");
    assert!(cmds.iter().any(
        |c| matches!(c, Command::Play { channel, .. } if *channel == ChannelRef::twitch("next_up"))
    ));
}

#[test]
fn the_slow_network_rule_and_the_background_pause_still_apply_behind_the_page() {
    let s = page_over_the_stream(false);
    let stall = Event::PlayerHealth {
        slot: 0,
        at_ms: 0,
        health: Health {
            stalled: true,
            buffer_ms: 0,
            pos_ms: None,
            length_ms: None,
        },
    };
    let (s, cmds) = reduce(&s, stall);
    assert!(
        cmds.iter().any(|c| matches!(c, Command::Play { .. })),
        "the stream reloads lower: {cmds:?}"
    );
    assert!(s.net.is_limited());
    // Switching to another app pauses it (no background play), coming back resumes it.
    let (away, cmds) = reduce(&s, Event::WindowFocus(false));
    assert!(away.paused);
    assert_eq!(
        cmds,
        vec![Command::SetPaused {
            slot: 0,
            paused: true
        }]
    );
    let (back, _) = reduce(&away, Event::WindowFocus(true));
    assert!(!back.paused);
}

#[test]
fn the_sleep_timer_ends_the_stream_behind_the_page_and_leaves_the_page() {
    let mut s = page_over_the_stream(false);
    s.sleep = Some(SleepTimer {
        id: 4,
        minutes: 15,
        left_min: 0,
        warning: false,
    });
    let (s, cmds) = reduce(&s, Event::SleepTimerFired { id: 4 });
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::LeaveChat));
    assert!(cmds.contains(&Command::Suspend));
    assert_eq!(s.now_playing, None);
    // The page is still up, and B leaves it for where the stream was opened from.
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    let (home, _) = reduce(&s, Intent::Back);
    assert_eq!(home.screen, Screen::Home);
}

#[test]
fn a_rail_entry_from_the_page_ends_the_stream_behind_it() {
    let s = page_over_the_stream(false);
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Settings));
    assert_eq!(s.screen, Screen::Settings);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::LeaveChat));
    assert_eq!(s.now_playing, None);
    assert!(s.back_stack.is_empty());
}

#[test]
fn a_video_from_the_page_stops_the_stream_and_b_twice_loads_it_again() {
    let s = page_over_the_stream(true);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Player);
    assert!(s.video.is_some());
    assert_eq!(s.now_playing, None, "a video and a stream never share it");
    assert_eq!(s.parked_live.as_ref().unwrap().channel.login, "fox");
    assert_eq!(
        s.back_stack,
        vec![Screen::Home, Screen::Player, Screen::Channel]
    );
    // The stream stops, with its chat and raid watch, before the video starts.
    let stopped = cmds
        .iter()
        .position(|c| *c == Command::StopPlayback { slot: 0 })
        .expect("the stream stops");
    let playing = cmds
        .iter()
        .position(|c| matches!(c, Command::PlayVideo { .. }))
        .expect("the video plays");
    assert!(stopped < playing, "{cmds:?}");
    assert!(cmds.contains(&Command::LeaveChat));
    assert!(cmds.contains(&Command::StopRaids));

    // B from the video: the page, as usual, with the stream still parked.
    let (page, _) = reduce(&s, Intent::Back);
    assert_eq!(page.screen, Screen::Channel);
    assert!(page.video.is_none());
    assert_eq!(page.now_playing, None);
    assert!(!page.live_covered());
    // The Player entry below the page is still the stream's.
    assert_eq!(page.back_stack, vec![Screen::Home, Screen::Player]);

    // B from the page: the stream is loaded again (it cannot be instant).
    let (live, cmds) = reduce(&page, Intent::Back);
    assert_eq!(live.screen, Screen::Player);
    assert_eq!(live.back_stack, vec![Screen::Home]);
    assert_eq!(live.channel_page, None);
    assert_eq!(live.parked_live, None);
    assert_eq!(live.now_playing.as_ref().unwrap().channel.login, "fox");
    assert_eq!(live.playback, Playback::Loading);
    assert!(cmds.contains(&Command::JoinChat {
        channel: ChannelRef::twitch("fox")
    }));
    assert!(cmds.iter().any(
        |c| matches!(c, Command::Play { channel, .. } if *channel == ChannelRef::twitch("fox"))
    ));

    // From there B is the usual one.
    let (home, cmds) = reduce(&live, Intent::Back);
    assert_eq!(home.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
}

#[test]
fn a_subscriber_only_video_leaves_the_stream_behind_the_page_playing() {
    let s = page_over_the_stream(false);
    let (s, _) = press(&s, &[Intent::Right]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.subscribe_card, "the card opens instead");
    assert!(cmds.is_empty(), "{cmds:?}");
    assert!(s.live_covered());
    assert_eq!(s.parked_live, None);
}

#[test]
fn a_on_the_live_card_of_the_stream_behind_the_page_is_just_b() {
    let (s, _) = reduce(&home(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = press(&s, &[Intent::Menu]);
    let (s, _) = press(&s, &[Intent::Down; 9]);
    let (s, _) = reduce(&s, Intent::Select);
    let (s, _) = reduce(
        &s,
        Event::ChannelPageLoaded {
            info: info("fox", Some(live("fox"))),
            rows: vec![Row::new(RowKind::LiveNow, vec![Tile::Stream(live("fox"))])],
        },
    );
    let (back, cmds) = reduce(&s, Intent::Select);
    assert!(cmds.is_empty(), "{cmds:?}");
    assert_eq!(back.screen, Screen::Player);
    assert_eq!(back.back_stack, vec![Screen::Home]);
    assert_eq!(back.playback, Playback::Playing);
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

// ---------------------------------------------------------------------------------------------
// View: Favorites (Twitch has no follow API for apps)
// ---------------------------------------------------------------------------------------------

/// The live channel `fox`'s page, opened with Y on its card and loaded.
fn live_page(signed_in: bool) -> AppState {
    let home = if signed_in { sign_in(&home()) } else { home() };
    let (s, _) = press(&home, &[Intent::ToggleChat]);
    let now_live = Event::ChannelPageLoaded {
        info: info("fox", Some(live("fox"))),
        rows: vec![Row::new(RowKind::LiveNow, vec![Tile::Stream(live("fox"))])],
    };
    reduce(&s, now_live).0
}

fn favorites_rows(feed: &Feed) -> Vec<(RowKind, Vec<String>)> {
    feed.rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::FavoritesLive | RowKind::FavoritesOffline))
        .map(|r| {
            let logins = r.tiles.iter().filter_map(|t| t.channel());
            (r.kind, logins.map(|c| c.login.clone()).collect())
        })
        .collect()
}

#[test]
fn view_on_the_page_adds_the_channel_to_favorites_and_again_removes_it_signed_out() {
    let s = page();
    assert!(!s.is_signed_in());
    let (starred, cmds) = reduce(&s, Intent::View);
    assert_eq!(starred.favorites, [ChannelRef::twitch("sleepy")]);
    assert_eq!(
        cmds,
        [Command::SaveFavorites(vec![ChannelRef::twitch("sleepy")])]
    );
    // No popup, the page stays as it was, and signed out no favorites row appears anywhere.
    assert_eq!(starred.popup, None);
    assert_eq!(starred.screen, Screen::Channel);
    assert_eq!(starred.channel_page, s.channel_page);
    assert_eq!(starred.home, s.home);
    assert_eq!(starred.favorites_feed, s.favorites_feed);
    let (unstarred, cmds) = reduce(&starred, Intent::View);
    assert!(unstarred.favorites.is_empty());
    assert_eq!(cmds, [Command::SaveFavorites(Vec::new())]);
    assert_eq!(unstarred.channel_page, s.channel_page);
}

#[test]
fn view_on_a_live_page_signed_in_adds_the_stream_to_the_favorites_rows() {
    let s = live_page(true);
    let (starred, cmds) = reduce(&s, Intent::View);
    // Not the multiview popup: View on the page is Favorites only.
    assert_eq!(starred.popup, None);
    assert_eq!(starred.favorites, [ChannelRef::twitch("fox")]);
    assert_eq!(
        cmds,
        [Command::SaveFavorites(vec![ChannelRef::twitch("fox")])]
    );
    let fox = vec!["fox".to_owned()];
    assert_eq!(
        favorites_rows(&starred.favorites_feed),
        [(RowKind::FavoritesLive, fox.clone())]
    );
    assert_eq!(
        favorites_rows(&starred.home),
        [(RowKind::FavoritesLive, fox)]
    );
    // Again: gone from the list and the rows.
    let (unstarred, cmds) = reduce(&starred, Intent::View);
    assert!(unstarred.favorites.is_empty());
    assert_eq!(cmds, [Command::SaveFavorites(Vec::new())]);
    assert!(favorites_rows(&unstarred.favorites_feed).is_empty());
    assert!(favorites_rows(&unstarred.home).is_empty());
}

#[test]
fn view_on_an_offline_page_signed_in_adds_the_channel_to_the_offline_row() {
    let s = sign_in(&page());
    let (starred, _) = reduce(&s, Intent::View);
    let rows = &starred.favorites_feed.rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, RowKind::FavoritesOffline);
    match &rows[0].tiles[..] {
        [Tile::Channel(c)] => {
            assert_eq!(c.channel, ChannelRef::twitch("sleepy"));
            assert_eq!(c.name, "Sleepy Fox", "the page's name");
        }
        other => panic!("{other:?}"),
    }
    // Home lists only live favorites.
    assert!(favorites_rows(&starred.home).is_empty());
}

#[test]
fn view_removes_a_channel_starred_elsewhere() {
    // Starred from the View popup on another screen, as the login in other letters.
    let mut s = live_page(false);
    s.favorites = vec![ChannelRef::twitch("Fox"), ChannelRef::kick("dozer")];
    let (after, cmds) = reduce(&s, Intent::View);
    assert_eq!(after.favorites, [ChannelRef::kick("dozer")]);
    assert_eq!(
        cmds,
        [Command::SaveFavorites(vec![ChannelRef::kick("dozer")])]
    );
}

#[test]
fn view_does_nothing_with_the_rail_or_the_subscribe_card_open() {
    let (rail, _) = press(&page(), &[Intent::Left]);
    assert!(rail.rail.focused);
    let (after, cmds) = reduce(&rail, Intent::View);
    assert_eq!(after, rail);
    assert!(cmds.is_empty());
    let (card, _) = reduce(&on_the_sub_only_video(true), Intent::Select);
    assert!(card.subscribe_card);
    let (after, cmds) = reduce(&card, Intent::View);
    assert!(after.favorites.is_empty());
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
    // The page that was underneath is reused (and reloaded): no second page, but it is pushed
    // again so B goes back to the stream, which keeps playing.
    assert_eq!(s.screen, Screen::Channel);
    assert_eq!(
        s.back_stack,
        vec![Screen::Home, Screen::Channel, Screen::Player]
    );
    assert_eq!(cmds, vec![load_command(ChannelRef::twitch("fox"))]);
    assert_eq!(
        s.channel_page.as_ref().unwrap().channel,
        ChannelRef::twitch("fox")
    );
    assert!(s.live_covered());

    // B: the stream, with the page it was opened from still below it.
    let (live, cmds) = reduce(&s, Intent::Back);
    assert!(cmds.is_empty());
    assert_eq!(live.screen, Screen::Player);
    assert_eq!(live.back_stack, vec![Screen::Home, Screen::Channel]);
    assert!(live.channel_page.is_some(), "the lower page is kept");
    // B again stops the stream and shows that page, B once more leaves it.
    let (page, cmds) = reduce(&live, Intent::Back);
    assert_eq!(page.screen, Screen::Channel);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    let (home, _) = reduce(&page, Intent::Back);
    assert_eq!(home.screen, Screen::Home);
    assert_eq!(home.channel_page, None);
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
