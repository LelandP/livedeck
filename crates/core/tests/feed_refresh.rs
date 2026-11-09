//! The feed refresh timer: the feed on screen reloads quietly once a minute, so a stream that
//! went live shows up without a restart (`Event::FeedRefreshDue`).

use htpc_core::*;

fn live(login: &str) -> Stream {
    Stream {
        channel: ChannelRef::twitch(login),
        user_id: format!("id_{login}"),
        title: format!("{login} title"),
        category: "Cat".into(),
        viewers: 10,
        tint: Rgb::default(),
        thumbnail: None,
    }
}

fn home_loaded(state: &AppState, login: &str) -> AppState {
    let rows = vec![Row::new(RowKind::TopLive, vec![Tile::Stream(live(login))])];
    reduce(
        state,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

#[test]
fn a_refresh_reloads_the_feed_on_screen_without_a_spinner() {
    let (state, _) = init();
    let state = home_loaded(&state, "first");
    let (next, cmds) = reduce(&state, Event::FeedRefreshDue);
    assert_eq!(cmds, vec![Command::LoadFeed { feed: FeedId::Home }]);
    // The rows stay, and no spinner is shown while the reload runs.
    assert!(!next.home.loading);
    assert_eq!(next.home.rows, state.home.rows);
}

#[test]
fn menu_twice_reloads_the_feed_and_closes_the_sidebar() {
    let (state, _) = init();
    let state = home_loaded(&state, "first");
    let (opened, _) = reduce(&state, Msg::from(Intent::Menu));
    assert!(opened.rail.focused);
    let (next, cmds) = reduce(&opened, Msg::from(Intent::Menu));
    assert_eq!(cmds, vec![Command::LoadFeed { feed: FeedId::Home }]);
    assert!(!next.rail.focused);
}

#[test]
fn menu_then_another_press_then_menu_does_not_reload() {
    let (state, _) = init();
    let state = home_loaded(&state, "first");
    let (opened, _) = reduce(&state, Msg::from(Intent::Menu));
    let (moved, _) = reduce(&opened, Msg::from(Intent::Down));
    let (next, cmds) = reduce(&moved, Msg::from(Intent::Menu));
    assert!(cmds.is_empty(), "{cmds:?}");
    assert!(!next.rail.focused);
}

#[test]
fn a_refresh_is_skipped_while_that_feed_is_already_loading() {
    // The start-up load has not answered yet.
    let (state, _) = init();
    assert!(state.home.loading);
    let (_, cmds) = reduce(&state, Event::FeedRefreshDue);
    assert!(cmds.is_empty(), "{cmds:?}");
}
