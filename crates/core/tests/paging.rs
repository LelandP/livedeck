//! Browse feature: pagination triggers, error / retry, stale answers, grids and the Category
//! screen.

use htpc_core::*;

fn stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::twitch(channel),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 10,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn stream_with_image(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::twitch(channel),
        user_id: String::new(),
        title: String::new(),
        category: String::new(),
        viewers: 10,
        tint: Rgb::default(),
        thumbnail: Some(format!("https://cdn/{channel}.jpg")),
    })
}

fn category(id: &str) -> Tile {
    Tile::Category(Category {
        id: id.into(),
        name: format!("name-{id}"),
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn tiles(prefix: &str, n: usize) -> Vec<Tile> {
    (0..n).map(|i| stream(&format!("{prefix}{i}"))).collect()
}

fn run(mut s: AppState, intents: &[Intent]) -> (AppState, Vec<Command>) {
    let mut all = Vec::new();
    for &i in intents {
        let (next, cmds) = reduce(&s, i);
        s = next;
        all.extend(cmds);
    }
    (s, all)
}

fn load_more_cmds(cmds: &[Command]) -> Vec<&Command> {
    cmds.iter()
        .filter(|c| matches!(c, Command::LoadMore { .. }))
        .collect()
}

fn loaded(s: &AppState, feed: FeedId, rows: Vec<Row>) -> AppState {
    reduce(s, Event::FeedLoaded { feed, rows }).0
}

/// Home with one Top Live row of 10 tiles that has another page behind cursor "c1".
fn home_with_more() -> AppState {
    let (s, _) = init();
    let row = Row::new(RowKind::TopLive, tiles("t", 10)).with_paging(Paging::More("c1".into()));
    loaded(&s, FeedId::Home, vec![row])
}

fn more_loaded(cursor: &str, outcome: PageOutcome) -> Event {
    Event::MoreLoaded {
        feed: FeedId::Home,
        row: RowId::twitch(RowKind::TopLive),
        cursor: cursor.into(),
        outcome,
    }
}

fn page(prefix: &str, n: usize, next: Option<&str>) -> PageOutcome {
    PageOutcome::Page {
        tiles: tiles(prefix, n),
        next: next.map(Into::into),
    }
}

// ---- first load ----------------------------------------------------------------------------

#[test]
fn startup_is_loading_until_rows_arrive() {
    let (s, cmds) = init();
    assert!(s.home.loading);
    assert_eq!(cmds, vec![Command::LoadFeed { feed: FeedId::Home }]);

    let s = loaded(
        &s,
        FeedId::Home,
        vec![Row::new(RowKind::TopLive, tiles("t", 2))],
    );
    assert!(!s.home.loading);
}

#[test]
fn loaded_rows_ask_for_their_thumbnails_once_each() {
    let (s, _) = init();
    let row = Row::new(
        RowKind::TopLive,
        vec![
            stream_with_image("a"),
            stream_with_image("a"),
            stream_with_image("b"),
            stream("no-image"),
        ],
    );
    let ev = Event::FeedLoaded {
        feed: FeedId::Home,
        rows: vec![row],
    };
    let (_, cmds) = reduce(&s, ev);
    assert_eq!(
        cmds,
        vec![Command::FetchThumbnails {
            urls: vec!["https://cdn/a.jpg".into(), "https://cdn/b.jpg".into()]
        }]
    );
}

#[test]
fn a_failed_first_load_shows_an_error_and_x_retries_it() {
    let (s, _) = init();
    let failed = Event::FeedFailed {
        feed: FeedId::Home,
        message: "offline".into(),
    };
    let (s, _) = reduce(&s, failed);
    assert!(!s.home.loading);
    assert_eq!(s.home.error.as_deref(), Some("offline"));

    let (s, cmds) = reduce(&s, Intent::Retry);
    assert!(s.home.loading && s.home.error.is_none());
    assert_eq!(cmds, vec![Command::LoadFeed { feed: FeedId::Home }]);

    // A second X while loading does nothing: no duplicate request.
    let (_, cmds) = reduce(&s, Intent::Retry);
    assert!(cmds.is_empty());
}

#[test]
fn retry_with_nothing_failed_does_nothing() {
    let s = home_with_more();
    let (after, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

// ---- horizontal pagination -----------------------------------------------------------------

#[test]
fn next_page_loads_when_focus_is_within_three_of_the_row_end() {
    // 10 tiles: the trigger is col + 1 + 3 >= 10, so col 6.
    let (s, cmds) = run(home_with_more(), &[Intent::Right; 5]);
    assert_eq!(s.home.focus.col, 5);
    assert!(load_more_cmds(&cmds).is_empty(), "col 5 is too early");

    let (s, cmds) = reduce(&s, Intent::Right);
    assert_eq!(s.home.focus.col, 6);
    assert_eq!(
        cmds,
        vec![Command::LoadMore {
            feed: FeedId::Home,
            row: RowId::twitch(RowKind::TopLive),
            cursor: "c1".into()
        }]
    );
    assert_eq!(s.home.rows[0].paging, Paging::Loading("c1".into()));
}

#[test]
fn moving_on_while_loading_does_not_request_the_page_twice() {
    let (_, cmds) = run(home_with_more(), &[Intent::Right; 9]);
    assert_eq!(load_more_cmds(&cmds).len(), 1);
}

#[test]
fn a_row_without_more_pages_never_loads() {
    let (s, _) = init();
    let s = loaded(
        &s,
        FeedId::Home,
        vec![Row::new(RowKind::TopLive, tiles("t", 10))],
    );
    let (_, cmds) = run(s, &[Intent::Right; 9]);
    assert!(cmds.is_empty());
}

#[test]
fn only_the_focused_row_pages() {
    let (s, _) = init();
    let rows = vec![
        Row::new(RowKind::FollowedLive, tiles("f", 2)),
        Row::new(RowKind::TopLive, tiles("t", 2)).with_paging(Paging::More("c".into())),
    ];
    let s = loaded(&s, FeedId::Home, rows);
    // Right on row 0 (no paging) requests nothing, even though row 1 has a page waiting.
    let (_, cmds) = reduce(&s, Intent::Right);
    assert!(cmds.is_empty());
    // Moving down onto row 1 does.
    let (_, cmds) = reduce(&s, Intent::Down);
    assert_eq!(load_more_cmds(&cmds).len(), 1);
}

// ---- applying a page -----------------------------------------------------------------------

/// Home after focus reached col 6 and the page behind "c1" was requested.
fn loading_page() -> AppState {
    run(home_with_more(), &[Intent::Right; 6]).0
}

#[test]
fn a_loaded_page_is_appended_and_a_new_cursor_kept() {
    let (s, cmds) = reduce(&loading_page(), more_loaded("c1", page("n", 5, Some("c2"))));
    assert_eq!(s.home.rows[0].tiles.len(), 15);
    assert_eq!(s.home.rows[0].tiles[10].key().1, "n0");
    assert_eq!(s.home.rows[0].paging, Paging::More("c2".into()));
    assert_eq!(s.home.focus.col, 6, "focus does not jump");
    assert!(cmds.is_empty());
}

#[test]
fn a_page_with_no_next_cursor_ends_the_row() {
    let (s, _) = reduce(&loading_page(), more_loaded("c1", page("n", 2, None)));
    assert_eq!(s.home.rows[0].paging, Paging::End);
}

#[test]
fn a_source_that_repeats_the_cursor_cannot_loop_forever() {
    let (s, _) = reduce(&loading_page(), more_loaded("c1", page("n", 2, Some("c1"))));
    assert_eq!(s.home.rows[0].paging, Paging::End);
}

#[test]
fn repeated_items_are_dropped_from_a_new_page() {
    // The ranking shifted: t9 shows up again, and n0 twice in the same page.
    let outcome = PageOutcome::Page {
        tiles: vec![stream("t9"), stream("n0"), stream("n0"), stream("n1")],
        next: None,
    };
    let (s, _) = reduce(&loading_page(), more_loaded("c1", outcome));
    let keys: Vec<_> = s.home.rows[0].tiles.iter().map(Tile::key).collect();
    assert_eq!(keys.len(), 12);
    assert_eq!(keys.iter().filter(|k| k.1 == "t9").count(), 1);
    assert_eq!(keys.iter().filter(|k| k.1 == "n0").count(), 1);
}

fn kick_stream(channel: &str) -> Tile {
    let Tile::Stream(mut s) = stream(channel) else {
        unreachable!()
    };
    s.channel = ChannelRef::kick(channel);
    Tile::Stream(s)
}

#[test]
fn the_same_login_on_another_platform_is_another_channel() {
    // "t9" is already on the row as a Twitch channel; a Kick channel of that name is not a repeat.
    let outcome = PageOutcome::Page {
        tiles: vec![kick_stream("t9"), stream("t9")],
        next: None,
    };
    let (s, _) = reduce(&loading_page(), more_loaded("c1", outcome));
    let keys: Vec<_> = s.home.rows[0].tiles.iter().map(Tile::key).collect();
    assert_eq!(keys.len(), 11);
    assert_eq!(keys[10], (Platform::Kick, "t9"));
}

#[test]
fn a_page_for_the_same_kind_of_row_on_another_platform_is_ignored() {
    let s = loading_page();
    let kick_page = Event::MoreLoaded {
        feed: FeedId::Home,
        row: RowId {
            kind: RowKind::TopLive,
            platform: Platform::Kick,
        },
        cursor: "c1".into(),
        outcome: page("k", 5, None),
    };
    let (after, _) = reduce(&s, kick_page);
    assert_eq!(after.home.rows[0].tiles.len(), 10);
    assert_eq!(after.home.rows[0].paging, Paging::Loading("c1".into()));
}

#[test]
fn rows_of_two_platforms_page_on_their_own_cursors() {
    let (s, _) = init();
    let rows = vec![
        Row::new(RowKind::TopLive, tiles("t", 2)).with_paging(Paging::More("tw".into())),
        Row::new(RowKind::TopLive, tiles("k", 2))
            .on(Platform::Kick)
            .with_paging(Paging::More("ki".into())),
    ];
    let s = loaded(&s, FeedId::Home, rows);
    let (s, cmds) = reduce(&s, Intent::Down);
    assert_eq!(
        load_more_cmds(&cmds),
        vec![&Command::LoadMore {
            feed: FeedId::Home,
            row: RowId {
                kind: RowKind::TopLive,
                platform: Platform::Kick,
            },
            cursor: "ki".into(),
        }]
    );
    let answer = Event::MoreLoaded {
        feed: FeedId::Home,
        row: RowId {
            kind: RowKind::TopLive,
            platform: Platform::Kick,
        },
        cursor: "ki".into(),
        outcome: page("k2_", 3, None),
    };
    let (s, _) = reduce(&s, answer);
    assert_eq!(s.home.rows[0].tiles.len(), 2, "the Twitch row is untouched");
    assert_eq!(s.home.rows[1].tiles.len(), 5);
}

#[test]
fn twitch_has_every_feature_and_kick_only_watching() {
    let all = Features {
        raids: true,
        hype_train: true,
        subscribe: true,
        chat_send: true,
        videos: true,
    };
    assert_eq!(Platform::Twitch.features(), all);
    assert_eq!(
        Platform::Kick.features(),
        Features {
            raids: false,
            hype_train: false,
            subscribe: false,
            chat_send: false,
            videos: false,
        }
    );
}

#[test]
fn a_loaded_page_asks_for_only_its_new_thumbnails() {
    let outcome = PageOutcome::Page {
        tiles: vec![stream_with_image("n0"), stream_with_image("n1")],
        next: None,
    };
    let (_, cmds) = reduce(&loading_page(), more_loaded("c1", outcome));
    assert_eq!(
        cmds,
        vec![Command::FetchThumbnails {
            urls: vec!["https://cdn/n0.jpg".into(), "https://cdn/n1.jpg".into()]
        }]
    );
}

#[test]
fn anonymous_end_of_row_stops_paging_without_an_error() {
    // The provider maps `IntegrityRequired` to `End`.
    let (s, cmds) = reduce(&loading_page(), more_loaded("c1", PageOutcome::End));
    assert_eq!(s.home.rows[0].paging, Paging::End);
    assert!(cmds.is_empty());
    assert!(s.home.error.is_none());
    // Moving on never asks again.
    let (_, cmds) = run(s, &[Intent::Right; 3]);
    assert!(cmds.is_empty());
}

#[test]
fn a_stale_answer_is_ignored() {
    // A reload replaced the rows, so the row now waits for a different cursor.
    let reloaded = loaded(
        &loading_page(),
        FeedId::Home,
        vec![Row::new(RowKind::TopLive, tiles("x", 4)).with_paging(Paging::More("z1".into()))],
    );
    let (after, cmds) = reduce(&reloaded, more_loaded("c1", page("n", 5, None)));
    assert_eq!(after, reloaded);
    assert!(cmds.is_empty());
}

// ---- failed pages --------------------------------------------------------------------------

#[test]
fn a_failed_page_is_not_retried_automatically() {
    let (s, _) = reduce(&loading_page(), more_loaded("c1", PageOutcome::Failed));
    assert_eq!(s.home.rows[0].paging, Paging::Failed("c1".into()));
    // Moving around the row end does not start a request storm.
    let (_, cmds) = run(s, &[Intent::Right, Intent::Left, Intent::Right]);
    assert!(cmds.is_empty());
}

#[test]
fn x_retries_a_failed_page_with_the_same_cursor() {
    let (s, _) = reduce(&loading_page(), more_loaded("c1", PageOutcome::Failed));
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(s.home.rows[0].paging, Paging::Loading("c1".into()));
    assert_eq!(
        cmds,
        vec![Command::LoadMore {
            feed: FeedId::Home,
            row: RowId::twitch(RowKind::TopLive),
            cursor: "c1".into()
        }]
    );
    // And it recovers when the retry works.
    let (s, _) = reduce(&s, more_loaded("c1", page("n", 3, None)));
    assert_eq!(s.home.rows[0].tiles.len(), 13);
    assert_eq!(s.home.rows[0].paging, Paging::End);
}

// ---- grids: Browse and Category ------------------------------------------------------------

fn categories(n: usize) -> Vec<Tile> {
    (0..n).map(|i| category(&format!("g{i}"))).collect()
}

/// The Browse screen with 30 categories (6 rows of 5) and a page behind "next".
fn browse() -> AppState {
    let (s, _) = init();
    let rows = grid_rows(
        RowKind::Categories,
        categories(30),
        Paging::More("next".into()),
    );
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Browse));
    assert_eq!(s.screen, Screen::Browse);
    assert_eq!(
        cmds,
        vec![Command::LoadFeed {
            feed: FeedId::Browse
        }]
    );
    assert!(s.browse.loading);
    loaded(&s, FeedId::Browse, rows)
}

#[test]
fn grid_rows_split_by_card_shape_and_keep_paging_on_the_last_row() {
    let rows = grid_rows(
        RowKind::Categories,
        categories(12),
        Paging::More("c".into()),
    );
    let lens: Vec<_> = rows.iter().map(|r| r.tiles.len()).collect();
    assert_eq!(lens, [5, 5, 2]);
    assert_eq!(rows[0].paging, Paging::End);
    assert_eq!(rows[2].paging, Paging::More("c".into()));

    let rows = grid_rows(RowKind::CategoryStreams, tiles("s", 7), Paging::End);
    let lens: Vec<_> = rows.iter().map(|r| r.tiles.len()).collect();
    assert_eq!(lens, [3, 3, 1]);
    assert!(grid_rows(RowKind::Categories, Vec::new(), Paging::End).is_empty());
}

#[test]
fn browse_loads_on_first_open_only() {
    let s = browse();
    // Back to Home, then Browse again: the directory is already there.
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Home));
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Browse));
    assert_eq!(s.screen, Screen::Browse);
    assert!(cmds.is_empty());
    assert_eq!(s.browse.rows.len(), 6);
}

#[test]
fn browse_keeps_its_own_focus() {
    let s = browse();
    let (s, _) = run(s, &[Intent::Down, Intent::Right]);
    assert_eq!(s.browse.focus, Focus { row: 1, col: 1 });
    assert_eq!(s.home.focus, Focus::default());
}

#[test]
fn a_grid_pages_when_focus_nears_the_last_row() {
    // 6 rows: the trigger is row + 1 >= 5, so row 4.
    let (s, cmds) = run(browse(), &[Intent::Down; 3]);
    assert_eq!(s.browse.focus.row, 3);
    assert!(load_more_cmds(&cmds).is_empty());

    let (s, cmds) = reduce(&s, Intent::Down);
    assert_eq!(s.browse.focus.row, 4);
    assert_eq!(
        cmds,
        vec![Command::LoadMore {
            feed: FeedId::Browse,
            row: RowId::twitch(RowKind::Categories),
            cursor: "next".into()
        }]
    );
}

#[test]
fn a_grid_page_continues_the_grid_and_keeps_focus() {
    let (s, _) = run(browse(), &[Intent::Down; 4]);
    let ev = Event::MoreLoaded {
        feed: FeedId::Browse,
        row: RowId::twitch(RowKind::Categories),
        cursor: "next".into(),
        outcome: PageOutcome::Page {
            tiles: categories(40)[30..].to_vec(),
            next: None,
        },
    };
    let (s, _) = reduce(&s, ev);
    // 40 tiles in rows of 5, the paging state moved to the new last row.
    assert_eq!(s.browse.rows.len(), 8);
    assert_eq!(s.browse.rows[7].tiles[4].key().1, "g39");
    assert_eq!(s.browse.rows[7].paging, Paging::End);
    assert_eq!(s.browse.focus.row, 4);
}

#[test]
fn a_failed_grid_page_waits_for_x() {
    let (s, _) = run(browse(), &[Intent::Down; 4]);
    let failed = Event::MoreLoaded {
        feed: FeedId::Browse,
        row: RowId::twitch(RowKind::Categories),
        cursor: "next".into(),
        outcome: PageOutcome::Failed,
    };
    let (s, cmds) = reduce(&s, failed);
    assert!(cmds.is_empty());
    let (s, cmds) = run(s, &[Intent::Down, Intent::Up]);
    assert!(cmds.is_empty());
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(load_more_cmds(&cmds).len(), 1);
    assert_eq!(s.browse.rows[5].paging, Paging::Loading("next".into()));
}

// ---- the Category screen -------------------------------------------------------------------

fn game_feed(id: &str) -> FeedId {
    FeedId::Category(id.into())
}

/// Browse, with category "g2" opened.
fn category_open() -> AppState {
    let (s, cmds) = run(browse(), &[Intent::Right, Intent::Right, Intent::Select]);
    assert_eq!(s.screen, Screen::Category);
    assert_eq!(
        cmds,
        vec![Command::LoadFeed {
            feed: game_feed("g2")
        }]
    );
    s
}

#[test]
fn opening_a_category_pushes_it_above_browse() {
    let s = category_open();
    assert_eq!(s.back_stack, vec![Screen::Browse]);
    let open = s.category.as_ref().unwrap();
    assert_eq!((open.id.as_str(), open.name.as_str()), ("g2", "name-g2"));
    assert!(open.feed.loading);
    assert_eq!(s.current_feed_id(), Some(game_feed("g2")));
}

#[test]
fn category_streams_load_as_a_grid_and_play_on_select() {
    let rows = grid_rows(
        RowKind::CategoryStreams,
        tiles("s", 7),
        Paging::More("p2".into()),
    );
    let s = loaded(&category_open(), game_feed("g2"), rows);
    let feed = &s.category.as_ref().unwrap().feed;
    assert!(!feed.loading);
    assert_eq!(feed.rows.len(), 3);

    let (s, cmds) = run(s, &[Intent::Right, Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "s1");
    assert_eq!(s.back_stack, vec![Screen::Browse, Screen::Category]);
    assert_eq!(
        cmds,
        vec![
            Command::JoinChat {
                channel: ChannelRef::twitch("s1")
            },
            Command::WatchHypeTrain {
                channel: "s1".into()
            },
            Command::Play {
                slot: 0,
                channel: ChannelRef::twitch("s1"),
                stream: StreamKind::Video(Quality::Best),
                latency: Latency::Low
            }
        ]
    );

    // Back from the player returns to the same card in the category.
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Category);
    assert_eq!(
        s.category.as_ref().unwrap().feed.focus,
        Focus { row: 0, col: 1 }
    );
    assert_eq!(s.back_stack, vec![Screen::Browse]);
}

#[test]
fn category_pages_through_its_own_cursor() {
    let rows = grid_rows(
        RowKind::CategoryStreams,
        tiles("s", 9),
        Paging::More("p2".into()),
    );
    let s = loaded(&category_open(), game_feed("g2"), rows);
    // 3 rows of 3: the trigger is row + 1 >= 2, so row 1.
    let (_, cmds) = reduce(&s, Intent::Down);
    assert_eq!(
        cmds,
        vec![Command::LoadMore {
            feed: game_feed("g2"),
            row: RowId::twitch(RowKind::CategoryStreams),
            cursor: "p2".into()
        }]
    );
}

#[test]
fn back_leaves_the_category_without_the_rail() {
    let (s, cmds) = reduce(&category_open(), Intent::Back);
    assert_eq!(s.screen, Screen::Browse);
    assert!(s.category.is_none());
    assert!(s.back_stack.is_empty());
    assert!(cmds.is_empty());
    // The directory kept its place.
    assert_eq!(s.browse.focus, Focus { row: 0, col: 2 });
}

#[test]
fn an_answer_for_a_category_that_was_left_is_dropped() {
    let (s, _) = reduce(&category_open(), Intent::Back);
    let late = Event::FeedLoaded {
        feed: game_feed("g2"),
        rows: grid_rows(RowKind::CategoryStreams, tiles("s", 3), Paging::End),
    };
    let (after, cmds) = reduce(&s, late);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

#[test]
fn an_answer_for_a_previous_category_does_not_land_in_the_new_one() {
    let (s, _) = reduce(&category_open(), Intent::Back);
    // Open g0 instead.
    let (s, _) = run(s, &[Intent::Left, Intent::Left, Intent::Select]);
    assert_eq!(s.category.as_ref().unwrap().id, "g0");
    let late = Event::FeedLoaded {
        feed: game_feed("g2"),
        rows: grid_rows(RowKind::CategoryStreams, tiles("s", 3), Paging::End),
    };
    let (after, _) = reduce(&s, late);
    assert!(after.category.as_ref().unwrap().feed.loading);
    assert!(after.category.as_ref().unwrap().feed.rows.is_empty());
}

#[test]
fn a_failed_category_shows_its_error_and_retries() {
    let failed = Event::FeedFailed {
        feed: game_feed("g2"),
        message: "offline".into(),
    };
    let (s, _) = reduce(&category_open(), failed);
    assert_eq!(
        s.category.as_ref().unwrap().feed.error.as_deref(),
        Some("offline")
    );
    let (_, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(
        cmds,
        vec![Command::LoadFeed {
            feed: game_feed("g2")
        }]
    );
}

#[test]
fn rail_navigation_closes_the_category() {
    let (s, _) = reduce(&category_open(), Intent::RailPick(RailEntry::Home));
    assert_eq!(s.screen, Screen::Home);
    assert!(s.category.is_none());
    assert!(s.back_stack.is_empty());
}

#[test]
fn the_cards_under_the_player_stay_the_visible_feed() {
    let s = category_open();
    assert_eq!(s.visible_feed_id(), game_feed("g2"));
    let rows = grid_rows(RowKind::CategoryStreams, tiles("s", 3), Paging::End);
    let (s, _) = run(loaded(&s, game_feed("g2"), rows), &[Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.visible_feed_id(), game_feed("g2"));
}

#[test]
fn retry_is_ignored_on_the_player_and_login_screens() {
    let (s, _) = run(home_with_more(), &[Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    let (after, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}
