//! Reducer tests: screen transitions, back stack, focus clamping and rail entry/exit.

use htpc_core::*;

fn stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::twitch(channel),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn category(name: &str) -> Tile {
    Tile::Category(Category {
        id: format!("id-{name}"),
        name: name.into(),
        tint: Rgb::default(),
        thumbnail: None,
    })
}

/// Rows of different lengths so clamping is observable: 5, 2 and 3 tiles.
fn rows() -> Vec<Row> {
    vec![
        Row::new(
            RowKind::FollowedLive,
            (0..5).map(|i| stream(&format!("f{i}"))).collect(),
        ),
        Row::new(RowKind::TopLive, vec![stream("t0"), stream("t1")]),
        Row::new(
            RowKind::Categories,
            vec![category("c0"), category("c1"), category("c2")],
        ),
    ]
}

/// Home with rows loaded.
fn home() -> AppState {
    let (s, _) = init();
    let (s, cmds) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: rows(),
        },
    );
    assert!(cmds.is_empty());
    s
}

/// Applies intents in order and returns the final state plus every command emitted.
fn run(mut s: AppState, intents: &[Intent]) -> (AppState, Vec<Command>) {
    let mut all = Vec::new();
    for &i in intents {
        let (next, cmds) = reduce(&s, i);
        s = next;
        all.extend(cmds);
    }
    (s, all)
}

fn signed_in(s: AppState) -> (AppState, Vec<Command>) {
    reduce(
        &s,
        Event::SignedIn(User {
            login: "nightowl_dev".into(),
            can_chat: true,
        }),
    )
}

// ---- init / events -------------------------------------------------------------------------

#[test]
fn init_starts_on_home_and_loads_rows() {
    let (s, cmds) = init();
    assert_eq!(s.screen, Screen::Home);
    assert!(s.back_stack.is_empty());
    assert_eq!(cmds, vec![Command::LoadFeed { feed: FeedId::Home }]);
}

#[test]
fn reduce_does_not_mutate_input() {
    let s = home();
    let before = s.clone();
    let _ = reduce(&s, Intent::Down);
    assert_eq!(s, before);
}

#[test]
fn rows_loaded_clamps_existing_focus() {
    let mut s = home();
    s.home.focus = Focus { row: 2, col: 2 };
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: rows()[..2].to_vec(),
        },
    );
    assert_eq!(s.home.focus, Focus { row: 1, col: 1 });
}

#[test]
fn intents_on_empty_rows_are_safe() {
    let (s, _) = init();
    let (s, cmds) = run(
        s,
        &[
            Intent::Down,
            Intent::Right,
            Intent::Up,
            Intent::Select,
            Intent::Pick { row: 3, col: 9 },
        ],
    );
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.home.focus, Focus::default());
    assert!(cmds.is_empty());
}

// ---- focus ---------------------------------------------------------------------------------

#[test]
fn home_rows_each_keep_their_own_column() {
    // Scroll row 0 to col 3, go down: row 1 starts at its own column (0), not row 0's.
    let (s, _) = run(home(), &[Intent::Right; 3]);
    let (s, _) = reduce(&s, Intent::Down);
    assert_eq!(s.home.focus, Focus { row: 1, col: 0 });
    // Scroll row 1, go down and back up: each row is where it was left.
    let (s, _) = run(s, &[Intent::Right, Intent::Down]);
    assert_eq!(s.home.focus, Focus { row: 2, col: 0 });
    let (s, _) = reduce(&s, Intent::Up);
    assert_eq!(s.home.focus, Focus { row: 1, col: 1 });
    let (s, _) = reduce(&s, Intent::Up);
    assert_eq!(s.home.focus, Focus { row: 0, col: 3 });
}

#[test]
fn right_clamps_at_row_end() {
    let (s, _) = run(home(), &[Intent::Right; 10]);
    assert_eq!(s.home.focus, Focus { row: 0, col: 4 });
}

#[test]
fn left_moves_back_before_opening_rail() {
    let (s, _) = run(home(), &[Intent::Right, Intent::Right, Intent::Left]);
    assert_eq!(s.home.focus.col, 1);
    assert!(!s.rail.focused);
}

#[test]
fn down_and_up_clamp_at_edges() {
    let (s, _) = run(home(), &[Intent::Down; 10]);
    assert_eq!(s.home.focus.row, 2);
    let (s, _) = run(s, &[Intent::Up; 10]);
    assert_eq!(s.home.focus.row, 0);
}

// ---- rail ----------------------------------------------------------------------------------

#[test]
fn left_from_first_column_opens_rail() {
    let (s, cmds) = reduce(&home(), Intent::Left);
    assert!(s.rail.focused);
    assert_eq!(s.home.focus.col, 0);
    assert!(cmds.is_empty());
}

#[test]
fn rail_cursor_clamps_and_reaches_account() {
    let (s, _) = run(home(), &[Intent::Left, Intent::Up]);
    assert_eq!(s.rail.index, RailEntry::Home);
    let (s, _) = run(s, &[Intent::Down; 10]);
    assert_eq!(s.rail.index, RailEntry::Account);
}

#[test]
fn rail_owns_focus_while_open() {
    let (s, _) = run(home(), &[Intent::Left, Intent::Down]);
    assert_eq!(s.home.focus, Focus::default(), "card focus must not move");
    assert_eq!(s.rail.index, RailEntry::Following);
}

#[test]
fn right_closes_rail_and_keeps_cursor() {
    let (s, cmds) = run(
        home(),
        &[Intent::Left, Intent::Down, Intent::Down, Intent::Right],
    );
    assert!(!s.rail.focused, "Right should close the rail");
    assert_eq!(s.rail.index, RailEntry::Favorites);
    assert_eq!(s.screen, Screen::Home);
    assert!(cmds.is_empty());
}

#[test]
fn menu_toggles_rail() {
    let (s, _) = reduce(&home(), Intent::Menu);
    assert!(s.rail.focused);
    let (s, _) = reduce(&s, Intent::Menu);
    assert!(!s.rail.focused);
}

#[test]
fn rail_home_closes_rail_and_stays_home() {
    let (s, cmds) = run(home(), &[Intent::Left, Intent::Select]);
    assert_eq!(s.screen, Screen::Home);
    assert!(!s.rail.focused);
    assert!(cmds.is_empty());
}

#[test]
fn rail_settings_opens_the_settings_screen() {
    let (s, cmds) = reduce(&home(), Intent::RailPick(RailEntry::Settings));
    assert_eq!(s.screen, Screen::Settings);
    assert!(s.back_stack.is_empty());
    assert!(cmds.is_empty());
}

// ---- login ---------------------------------------------------------------------------------

#[test]
fn account_entry_opens_login_on_top_of_home() {
    let mut keys = vec![Intent::Left];
    // Past Following, Favorites, Browse, Search and Settings.
    keys.extend([Intent::Down; 6]);
    keys.push(Intent::Select);
    let (s, cmds) = run(home(), &keys);
    assert_eq!(s.rail.index, RailEntry::Account);
    assert_eq!(s.screen, Screen::Login);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    assert!(!s.rail.focused);
    assert_eq!(cmds, vec![Command::StartLogin]);
}

#[test]
fn following_while_signed_out_opens_login() {
    let (s, cmds) = reduce(&home(), Intent::RailPick(RailEntry::Following));
    assert_eq!(s.screen, Screen::Login);
    assert_eq!(cmds, vec![Command::StartLogin]);
}

#[test]
fn following_while_signed_in_opens_and_loads_the_following_tab() {
    let (s, _) = signed_in(home());
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Following));
    assert_eq!(s.screen, Screen::Following);
    assert!(s.back_stack.is_empty());
    assert!(s.following.loading);
    assert_eq!(s.rail.index, RailEntry::Following);
    assert_eq!(
        cmds,
        vec![Command::LoadFeed {
            feed: FeedId::Following
        }]
    );
}

#[test]
fn device_code_is_stored_only_while_on_login() {
    let code = DeviceCode {
        user_code: "KQRT-7ZMP".into(),
        verification_uri: "twitch.tv/activate".into(),
    };
    let (s, _) = reduce(&home(), Event::DeviceCodeIssued(code.clone()));
    assert_eq!(s.login, None, "stale code after leaving login");

    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (s, _) = reduce(&s, Event::DeviceCodeIssued(code.clone()));
    assert_eq!(s.login, Some(code));
}

#[test]
fn select_on_a_waiting_login_does_nothing() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (after, cmds) = run(s.clone(), &[Intent::Select, Intent::Retry]);
    assert_eq!(after, s, "the login ends by itself, A has nothing to do");
    assert!(cmds.is_empty());
}

#[test]
fn signed_in_closes_the_login_and_reloads_home_for_the_account() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (s, cmds) = signed_in(s);
    assert_eq!(s.screen, Screen::Home);
    assert!(s.back_stack.is_empty());
    assert_eq!(s.login, None);
    assert_eq!(
        s.user.as_ref().map(|u| u.login.as_str()),
        Some("nightowl_dev")
    );
    // The Followed row appears, so Home loads again. Its rows stay up until the answer arrives.
    // Live notifications start watching the account's follows.
    assert_eq!(
        cmds,
        vec![
            Command::LoadFeed { feed: FeedId::Home },
            Command::StartLiveWatch
        ]
    );
    assert!(s.home.loading);
    assert!(!s.home.rows.is_empty());
}

#[test]
fn following_while_signed_out_continues_to_following_after_the_login() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Following));
    assert_eq!(s.screen, Screen::Login);
    let (s, cmds) = signed_in(s);
    assert_eq!(s.screen, Screen::Following);
    assert!(s.back_stack.is_empty());
    assert_eq!(s.rail.index, RailEntry::Following);
    assert_eq!(s.after_login, None);
    assert!(cmds.contains(&Command::LoadFeed {
        feed: FeedId::Following
    }));
    assert!(cmds.contains(&Command::LoadFeed { feed: FeedId::Home }));
}

#[test]
fn a_failed_login_is_shown_and_a_retries_with_a_fresh_code() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (s, _) = reduce(&s, Event::LoginFailed(LoginProblem::Expired));
    assert_eq!(s.login_problem, Some(LoginProblem::Expired));
    assert_eq!(s.login, None);
    assert_eq!(s.screen, Screen::Login);

    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds, vec![Command::StartLogin]);
    assert_eq!(s.login_problem, None);
    assert_eq!(s.screen, Screen::Login);

    // X does the same.
    let (s, _) = reduce(&s, Event::LoginFailed(LoginProblem::Denied));
    let (_, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(cmds, vec![Command::StartLogin]);
}

#[test]
fn a_login_failure_after_leaving_the_screen_is_ignored() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (s, _) = reduce(&s, Intent::Back);
    let (after, cmds) = reduce(&s, Event::LoginFailed(LoginProblem::Denied));
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

#[test]
fn a_new_code_clears_the_old_problem() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let failed = Event::LoginFailed(LoginProblem::Failed("offline".into()));
    let (s, _) = reduce(&s, failed);
    let code = DeviceCode {
        user_code: "NEW".into(),
        verification_uri: "u".into(),
    };
    let (s, _) = reduce(&s, Event::DeviceCodeIssued(code.clone()));
    assert_eq!(s.login_problem, None);
    assert_eq!(s.login, Some(code));
}

#[test]
fn leaving_the_login_forgets_the_problem_and_the_destination() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Following));
    assert_eq!(s.after_login, Some(Screen::Following));
    let (s, _) = reduce(&s, Event::LoginFailed(LoginProblem::Denied));
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(cmds, vec![Command::CancelLogin]);
    assert_eq!((s.login_problem, s.after_login), (None, None));
}

#[test]
fn back_from_login_cancels_and_returns() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (s, _) = reduce(
        &s,
        Event::DeviceCodeIssued(DeviceCode {
            user_code: "X".into(),
            verification_uri: "u".into(),
        }),
    );
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(s.back_stack.is_empty());
    assert_eq!(s.login, None);
    assert_eq!(cmds, vec![Command::CancelLogin]);
}

#[test]
fn login_ignores_navigation() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let keys = [
        Intent::Up,
        Intent::Down,
        Intent::Left,
        Intent::Right,
        Intent::ToggleChat,
        Intent::Menu,
    ];
    let (after, cmds) = run(s.clone(), &keys);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

#[test]
fn rail_click_on_login_cancels_and_goes_home() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Home));
    assert_eq!(s.screen, Screen::Home);
    assert!(s.back_stack.is_empty());
    assert_eq!(cmds, vec![Command::CancelLogin]);
}

#[test]
fn rail_click_on_account_while_on_login_is_a_no_op() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Account));
    let (after, cmds) = reduce(&s, Intent::RailPick(RailEntry::Account));
    assert_eq!(after.screen, Screen::Login);
    assert_eq!(after.back_stack, vec![Screen::Home]);
    assert!(cmds.is_empty());
}

#[test]
fn signed_in_elsewhere_does_not_change_screen() {
    let (s, _) = signed_in(home());
    assert_eq!(s.screen, Screen::Home);
    assert!(s.is_signed_in());
}

// ---- player --------------------------------------------------------------------------------

#[test]
fn select_stream_opens_player_and_joins_chat() {
    let (s, cmds) = run(home(), &[Intent::Down, Intent::Right, Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    assert_eq!(
        s.now_playing.as_ref().map(|p| p.channel.login.as_str()),
        Some("t1")
    );
    assert!(!s.chat_open);
    assert_eq!(
        cmds,
        vec![
            Command::JoinChat {
                channel: ChannelRef::twitch("t1")
            },
            Command::WatchHypeTrain {
                channel: "t1".into()
            },
            Command::Play {
                slot: 0,
                channel: ChannelRef::twitch("t1"),
                stream: StreamKind::Video(Quality::Best),
                latency: Latency::Low
            }
        ]
    );
}

#[test]
fn select_category_opens_its_streams_screen() {
    let (s, cmds) = run(home(), &[Intent::Down, Intent::Down, Intent::Select]);
    assert_eq!(s.screen, Screen::Category);
    assert_eq!(s.back_stack, vec![Screen::Home]);
    let open = s.category.as_ref().expect("category is open");
    assert_eq!((open.id.as_str(), open.name.as_str()), ("id-c0", "c0"));
    assert!(
        open.feed.loading,
        "skeletons show while the first page loads"
    );
    assert_eq!(
        cmds,
        vec![Command::LoadFeed {
            feed: FeedId::Category("id-c0".into())
        }]
    );
}

#[test]
fn pick_moves_focus_and_plays() {
    let (s, _) = run(home(), &[Intent::Left]);
    let (s, cmds) = reduce(&s, Intent::Pick { row: 0, col: 3 });
    assert_eq!(s.home.focus, Focus { row: 0, col: 3 });
    assert_eq!(s.screen, Screen::Player);
    assert!(!s.rail.focused);
    assert_eq!(cmds.len(), 3, "join chat, watch for hype trains and play");
}

#[test]
fn y_toggles_chat_only_on_player() {
    let (s, _) = reduce(&home(), Intent::ToggleChat);
    assert!(!s.chat_open);

    let (s, _) = run(home(), &[Intent::Select, Intent::ToggleChat]);
    assert!(s.chat_open);
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert!(!s.chat_open);
}

#[test]
fn back_from_player_closes_chat_and_returns_home() {
    let (s, _) = run(home(), &[Intent::Right, Intent::Select, Intent::ToggleChat]);
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(!s.chat_open);
    assert!(s.now_playing.is_none());
    assert!(s.chat.is_empty());
    assert!(s.back_stack.is_empty());
    assert_eq!(
        cmds,
        vec![
            Command::StopPlayback { slot: 0 },
            Command::LeaveChat,
            Command::StopHypeTrain
        ]
    );
    // Focus is kept, so the user lands on the card they watched.
    assert_eq!(s.home.focus, Focus { row: 0, col: 1 });
}

#[test]
fn player_ignores_navigation_and_menu() {
    let (s, _) = run(home(), &[Intent::Select]);
    let keys = [Intent::Up, Intent::Left, Intent::Select, Intent::Menu];
    let (after, cmds) = run(s.clone(), &keys);
    // Only the HUD pulse changes, and nothing leaves the player screen.
    assert_eq!(after.screen, Screen::Player);
    assert_eq!(after.now_playing, s.now_playing);
    assert_eq!(after.home, s.home);
    assert!(cmds.is_empty());
}

fn msg(user: &str) -> ChatMessage {
    ChatMessage::plain(user, "hi")
}

fn chat(channel: &str, messages: Vec<ChatMessage>) -> Event {
    Event::ChatMessages {
        channel: ChannelRef::twitch(channel),
        messages,
    }
}

#[test]
fn chat_messages_append_for_current_channel_only() {
    let (s, _) = run(home(), &[Intent::Select]);
    let (s, _) = reduce(&s, chat("f0", vec![msg("a"), msg("b")]));
    let (s, _) = reduce(&s, chat("other", vec![msg("x")]));
    assert_eq!(s.chat.len(), 2);

    // Late messages after leaving are dropped.
    let (s, _) = reduce(&s, Intent::Back);
    let (s, _) = reduce(&s, chat("f0", vec![msg("late")]));
    assert!(s.chat.is_empty());
}

#[test]
fn chat_is_capped_and_drops_oldest() {
    let (s, _) = run(home(), &[Intent::Select]);
    let many = (0..CHAT_CAPACITY + 25)
        .map(|i| msg(&format!("u{i}")))
        .collect();
    let (s, _) = reduce(&s, chat("f0", many));
    assert_eq!(s.chat.len(), CHAT_CAPACITY);
    assert_eq!(s.chat[0].user, "u25");
}

// ---- back stack ----------------------------------------------------------------------------

#[test]
fn back_on_a_root_screen_opens_the_rail() {
    for entry in [RailEntry::Home, RailEntry::Browse, RailEntry::Settings] {
        let (s, _) = reduce(&home(), Intent::RailPick(entry));
        let (s, cmds) = reduce(&s, Intent::Back);
        assert!(s.rail.focused, "{entry:?}");
        assert!(!s.exit_prompt, "{entry:?}");
        assert!(cmds.is_empty());
    }
}

#[test]
fn back_on_the_rail_asks_to_quit() {
    let (s, cmds) = run(home(), &[Intent::Back, Intent::Back]);
    assert!(s.exit_prompt);
    assert!(
        s.rail.focused,
        "the sidebar is still open behind the prompt"
    );
    assert!(cmds.is_empty(), "asking is not quitting yet");
}

#[test]
fn menu_then_back_asks_to_quit() {
    let (s, _) = run(home(), &[Intent::Menu, Intent::Back]);
    assert!(s.exit_prompt);
}

#[test]
fn a_on_the_quit_prompt_quits() {
    let (s, _) = run(home(), &[Intent::Back, Intent::Back]);
    let (_, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds, vec![Command::Quit]);
}

#[test]
fn b_on_the_quit_prompt_cancels_and_leaves_home_as_it_was() {
    let (open, _) = run(home(), &[Intent::Back, Intent::Back]);
    let (s, cmds) = reduce(&open, Intent::Back);
    assert!(!s.exit_prompt);
    assert!(
        s.rail.focused,
        "B closes the prompt and the sidebar stays open"
    );
    assert!(cmds.is_empty());
    // Start closes it too.
    let (s, _) = reduce(&open, Intent::Menu);
    assert!(!s.exit_prompt);
}

#[test]
fn the_quit_prompt_takes_all_other_input() {
    let (open, _) = run(home(), &[Intent::Back, Intent::Back]);
    for intent in [
        Intent::Up,
        Intent::Down,
        Intent::Left,
        Intent::Right,
        Intent::Retry,
        Intent::ToggleChat,
        Intent::Pick { row: 0, col: 0 },
        Intent::RailPick(RailEntry::Browse),
    ] {
        let (s, cmds) = reduce(&open, intent);
        assert_eq!(s, open, "{intent:?} changed something behind the prompt");
        assert!(cmds.is_empty(), "{intent:?}");
    }
}

#[test]
fn back_on_a_category_still_goes_back_instead_of_asking() {
    let mut s = home();
    s.back_stack = vec![Screen::Browse];
    s.screen = Screen::Category;
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Browse);
    assert!(!s.exit_prompt);
}

#[test]
fn back_with_empty_stack_falls_back_to_home() {
    let mut s = home();
    s.screen = Screen::Player;
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
}

#[test]
fn root_screen_resets_back_stack() {
    let mut s = home();
    s.back_stack = vec![Screen::Home, Screen::Home];
    s.screen = Screen::Settings;
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Home));
    assert_eq!(s.screen, Screen::Home);
    assert!(s.back_stack.is_empty());
}

#[test]
fn rail_entry_index_round_trips() {
    for (i, e) in RailEntry::ALL.iter().enumerate() {
        assert_eq!(e.index(), i);
        assert_eq!(RailEntry::from_index(i), Some(*e));
    }
    assert_eq!(RailEntry::from_index(RailEntry::ALL.len()), None);
    assert!(Screen::Home.is_root());
    assert!(!Screen::Player.is_root());
    assert!(!Screen::Login.is_root());
}

#[test]
fn home_load_failure_keeps_rows_and_clears_on_success() {
    let mut s = home();
    s.home.focus = Focus { row: 1, col: 1 };
    let (s, cmds) = reduce(
        &s,
        Event::FeedFailed {
            feed: FeedId::Home,
            message: "offline".into(),
        },
    );
    assert!(cmds.is_empty());
    assert_eq!(s.home.error.as_deref(), Some("offline"));
    assert_eq!(s.home.rows, rows(), "stale rows stay visible");
    assert_eq!(s.home.focus, Focus { row: 1, col: 1 });

    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: rows(),
        },
    );
    assert_eq!(s.home.error, None);
}
