//! Search: the sign-in gate, the keyboard, typing, stale answers, and moving between the keyboard
//! and the results.

use htpc_core::*;

fn stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn category(id: &str) -> Tile {
    Tile::Category(Category {
        id: id.into(),
        name: format!("Game {id}"),
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn home() -> AppState {
    let (s, _) = init();
    let rows = vec![Row::new(RowKind::TopLive, vec![stream("t0")])];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

fn signed_in(s: &AppState) -> AppState {
    let user = User {
        login: "nightowl_dev".into(),
    };
    reduce(s, Event::SignedIn(user)).0
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

/// Signed in, on the Search screen with nothing typed.
fn search() -> AppState {
    let (s, _) = reduce(&signed_in(&home()), Intent::RailPick(RailEntry::Search));
    s
}

fn typed(s: &AppState, text: &str) -> (AppState, Vec<Command>) {
    let keys: Vec<Intent> = text.chars().map(Intent::Text).collect();
    run(s.clone(), &keys)
}

fn searches(cmds: &[Command]) -> Vec<(u32, String)> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Search { id, query } => Some((*id, query.clone())),
            _ => None,
        })
        .collect()
}

fn results() -> Vec<Row> {
    vec![
        Row::new(RowKind::SearchLive, vec![stream("live0"), stream("live1")]),
        Row::new(RowKind::SearchCategories, vec![category("1")]),
    ]
}

/// "ab" typed and its results in.
fn with_results() -> AppState {
    let (s, _) = typed(&search(), "ab");
    let id = s.search.id;
    reduce(
        &s,
        Event::SearchResults {
            id,
            rows: results(),
        },
    )
    .0
}

// ---- getting there ---------------------------------------------------------------------------

#[test]
fn search_sits_between_browse_and_settings_and_is_a_root_screen() {
    assert_eq!(RailEntry::Browse.next(), RailEntry::Search);
    assert_eq!(RailEntry::Search.next(), RailEntry::Settings);
    let s = search();
    assert_eq!(s.screen, Screen::Search);
    assert!(s.back_stack.is_empty());
    assert!(Screen::Search.is_root());
}

#[test]
fn signed_out_search_asks_to_sign_in_and_continues_there() {
    let (s, cmds) = reduce(&home(), Intent::RailPick(RailEntry::Search));
    assert_eq!(s.screen, Screen::Login);
    assert_eq!(s.after_login, Some(Screen::Search));
    assert_eq!(cmds, [Command::StartLogin]);

    let s = signed_in(&s);
    assert_eq!(s.screen, Screen::Search);
    assert!(s.back_stack.is_empty());
}

#[test]
fn signing_out_leaves_search_and_forgets_it() {
    let s = with_results();
    let id = s.search.id;
    let (s, _) = reduce(&s, Event::SignedOut);
    assert_eq!(s.screen, Screen::Home);
    assert!(s.search.query.is_empty() && s.search.feed.rows.is_empty());
    assert_eq!(s.search.id, id, "ids never repeat");
}

// ---- typing ----------------------------------------------------------------------------------

#[test]
fn every_change_sends_a_search_with_a_new_id_and_short_text_clears() {
    let (s, cmds) = typed(&search(), "abc");
    // One character is too short: the app is told (to drop a pending search), nothing loads.
    assert_eq!(
        searches(&cmds),
        [(1, "a".into()), (2, "ab".into()), (3, "abc".into())]
    );
    assert_eq!(s.search.query, "abc");
    assert!(s.search.feed.loading);

    let (s, cmds) = run(s, &[Intent::Erase, Intent::Erase]);
    assert_eq!(searches(&cmds), [(4, "ab".into()), (5, "a".into())]);
    assert!(!s.search.feed.loading && s.search.feed.rows.is_empty());
}

#[test]
fn a_trailing_space_does_not_search_again() {
    let (s, _) = typed(&search(), "ab");
    let (s, cmds) = reduce(&s, Intent::ToggleChat);
    assert_eq!(s.search.query, "ab ");
    assert!(searches(&cmds).is_empty());
    let (s, cmds) = typed(&s, "c");
    assert_eq!(searches(&cmds), [(3, "ab c".into())]);
    assert_eq!(s.search.asked, "ab c");
}

#[test]
fn the_physical_keyboard_types_lowercase_and_ignores_other_characters() {
    let (s, _) = typed(&search(), "Ab!Y r");
    assert_eq!(s.search.query, "aby r");
    // `y` types instead of opening chat, `r` instead of retrying.
    assert!(!s.chat_open);
}

#[test]
fn the_on_screen_keys_type_space_delete_and_clear() {
    // A on `a`, right to `b`, A.
    let (s, _) = run(search(), &[Intent::Select, Intent::Right, Intent::Select]);
    assert_eq!(s.search.query, "ab");
    // X deletes, Y types a space.
    let (s, _) = run(s, &[Intent::Retry, Intent::ToggleChat]);
    assert_eq!(s.search.query, "a ");
    // Down to the bottom row: space, then clear.
    let (s, _) = run(s, &[Intent::Down; 4]);
    assert_eq!(s.search.keys.key(), Key::Space);
    let (s, _) = run(s, &[Intent::Right, Intent::Select]);
    assert_eq!(s.search.keys.key(), Key::Clear);
    assert!(s.search.query.is_empty());
}

#[test]
fn b_deletes_then_asks_to_quit_once_empty() {
    let (s, _) = typed(&search(), "ab");
    let (s, _) = run(s, &[Intent::Back, Intent::Back]);
    assert!(s.search.query.is_empty());
    assert!(!s.exit_prompt);
    let (s, _) = reduce(&s, Intent::Back);
    assert!(s.exit_prompt);
}

#[test]
fn left_at_the_keyboard_edge_opens_the_rail_and_typing_there_does_nothing() {
    let (s, _) = reduce(&search(), Intent::Left);
    assert!(s.rail.focused);
    let (s, _) = typed(&s, "ab");
    assert!(s.search.query.is_empty(), "the rail has focus");
}

#[test]
fn elsewhere_y_r_and_backspace_keep_their_meaning() {
    let (s, _) = reduce(&home(), Intent::Text('a'));
    assert_eq!(s, home(), "other letters do nothing");
    let (s, _) = reduce(&home(), Intent::Erase);
    assert!(s.exit_prompt, "Backspace is B");
    let (s, _) = reduce(&home(), Intent::Select);
    let (s, _) = reduce(&s, Intent::Text('y'));
    assert!(s.chat_open, "y toggles chat on the player");
}

// ---- answers ---------------------------------------------------------------------------------

#[test]
fn stale_answers_are_dropped() {
    let (s, _) = typed(&search(), "ab");
    let old = s.search.id;
    let (s, _) = typed(&s, "c");
    let (s, _) = reduce(
        &s,
        Event::SearchResults {
            id: old,
            rows: results(),
        },
    );
    assert!(s.search.feed.rows.is_empty(), "an answer for \"ab\"");
    assert!(s.search.feed.loading);
    let (s, _) = reduce(
        &s,
        Event::SearchFailed {
            id: old,
            message: "x".into(),
        },
    );
    assert_eq!(s.search.feed.error, None);

    let id = s.search.id;
    let (s, cmds) = reduce(
        &s,
        Event::SearchResults {
            id,
            rows: results(),
        },
    );
    assert_eq!(s.search.feed.rows.len(), 2);
    assert_eq!(s.search.results_for, "abc");
    assert!(!s.search.feed.loading);
    assert!(cmds.is_empty(), "no thumbnails in these tiles");
}

#[test]
fn a_failed_search_shows_its_error_and_done_tries_again() {
    let (s, _) = typed(&search(), "ab");
    let id = s.search.id;
    let (s, _) = reduce(
        &s,
        Event::SearchFailed {
            id,
            message: "offline".into(),
        },
    );
    assert_eq!(s.search.feed.error.as_deref(), Some("offline"));
    let (s, cmds) = reduce(&s, Intent::Menu);
    assert_eq!(searches(&cmds), [(id + 1, "ab".into())]);
    assert!(s.search.feed.loading && s.search.feed.error.is_none());
}

// ---- keyboard and results --------------------------------------------------------------------

#[test]
fn down_from_the_bottom_row_enters_the_results_and_up_returns_to_the_same_key() {
    let s = with_results();
    let (s, _) = run(s, &[Intent::Right, Intent::Down, Intent::Down]);
    let key = s.search.keys;
    let (s, _) = run(s, &[Intent::Down, Intent::Down]);
    assert!(!s.search.in_results, "still on the bottom row");
    let (s, _) = reduce(&s, Intent::Down);
    assert!(s.search.in_results);
    assert_eq!(s.search.feed.focus, Focus::default());

    // Inside the rows it works like Home.
    let (s, _) = run(s, &[Intent::Right, Intent::Down]);
    assert_eq!(s.search.feed.focus, Focus { row: 1, col: 0 });
    let (s, _) = run(s, &[Intent::Up, Intent::Up]);
    assert!(!s.search.in_results);
    let (s, _) = run(s, &[Intent::Up, Intent::Up]);
    assert_eq!(s.search.keys, key, "the key focus is kept");
}

#[test]
fn start_on_the_keyboard_jumps_to_the_results_and_b_comes_back() {
    let (s, _) = reduce(&with_results(), Intent::Menu);
    assert!(s.search.in_results);
    assert!(!s.rail.focused, "Start is done here, not the rail");
    let (s, _) = reduce(&s, Intent::Back);
    assert!(!s.search.in_results);
    assert!(!s.exit_prompt);
    // In the results, Start is the rail as on other screens.
    let (s, _) = run(s, &[Intent::Menu, Intent::Menu]);
    assert!(s.rail.focused);
}

#[test]
fn without_results_down_and_start_stay_on_the_keyboard() {
    let (s, _) = run(search(), &[Intent::Down; 6]);
    assert!(!s.search.in_results);
    let (s, _) = reduce(&s, Intent::Menu);
    assert!(!s.search.in_results);
}

#[test]
fn typing_in_the_results_returns_to_the_keyboard() {
    let (s, _) = reduce(&with_results(), Intent::Menu);
    let (s, cmds) = reduce(&s, Intent::Text('x'));
    assert!(!s.search.in_results);
    assert_eq!(s.search.query, "abx");
    assert_eq!(searches(&cmds).len(), 1);
}

#[test]
fn a_stream_from_the_results_keeps_the_search_for_when_b_comes_back() {
    let (s, _) = reduce(&with_results(), Intent::Menu);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Player);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::Play { channel, .. } if channel == "live0")));
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Search);
    assert_eq!(s.search.query, "ab");
    assert!(s.search.in_results);
    assert_eq!(s.search.feed.rows.len(), 2);
}

#[test]
fn a_category_result_opens_the_category_screen_and_back_returns() {
    let (s, _) = run(with_results(), &[Intent::Menu, Intent::Down]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Category);
    assert_eq!(
        cmds,
        [Command::LoadFeed {
            feed: FeedId::Category("1".into())
        }]
    );
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Search);
}

#[test]
fn result_rows_page_for_the_text_they_answer() {
    let (s, _) = typed(&search(), "ab");
    let id = s.search.id;
    let tiles: Vec<Tile> = (0..6).map(|i| stream(&format!("s{i}"))).collect();
    let row = Row::new(RowKind::SearchLive, tiles).with_paging(Paging::More("c".into()));
    let (s, _) = reduce(
        &s,
        Event::SearchResults {
            id,
            rows: vec![row],
        },
    );
    let (s, cmds) = run(s, &[Intent::Menu, Intent::Right, Intent::Right]);
    assert_eq!(
        cmds,
        [Command::LoadMore {
            feed: FeedId::Search("ab".into()),
            kind: RowKind::SearchLive,
            cursor: "c".into(),
        }]
    );
    // A page for another text is ignored.
    let page = |text: &str| Event::MoreLoaded {
        feed: FeedId::Search(text.into()),
        kind: RowKind::SearchLive,
        cursor: "c".into(),
        outcome: PageOutcome::Page {
            tiles: vec![stream("more")],
            next: None,
        },
    };
    let (ignored, _) = reduce(&s, page("abc"));
    assert_eq!(ignored.search.feed.rows[0].tiles.len(), 6);
    let (s, _) = reduce(&s, page("ab"));
    assert_eq!(s.search.feed.rows[0].tiles.len(), 7);
}
