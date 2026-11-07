//! Account and Following tests: the account rail entry, sign-out, Settings, and the Following
//! tab.

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

fn offline_channel(login: &str) -> Tile {
    Tile::Channel(Channel {
        channel: ChannelRef::twitch(login),
        name: login.into(),
        tint: Rgb::default(),
    })
}

/// Home with one loaded row.
fn home() -> AppState {
    let (s, _) = init();
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![stream("t0")])],
        },
    )
    .0
}

fn signed_in(s: &AppState) -> AppState {
    let user = User {
        login: "nightowl_dev".into(),
        can_chat: true,
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

fn load_following(s: &AppState, rows: Vec<Row>) -> AppState {
    let feed = FeedId::Following;
    reduce(s, Event::FeedLoaded { feed, rows }).0
}

/// Signed in, on the Following tab with two live and one offline channel.
fn following() -> AppState {
    let s = signed_in(&home());
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Following));
    load_following(
        &s,
        vec![
            Row::new(
                RowKind::FollowedLive,
                vec![stream("live0"), stream("live1")],
            ),
            Row::new(RowKind::FollowedOffline, vec![offline_channel("off0")]),
        ],
    )
}

// ---- account: the rail entry, sign-out and Settings ------------------------------------------

#[test]
fn the_account_entry_opens_settings_once_signed_in() {
    let s = signed_in(&home());
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Account));
    assert_eq!(s.screen, Screen::Settings);
    assert!(cmds.is_empty());
}

#[test]
fn a_on_settings_signs_out_and_the_event_reloads_home() {
    let s = signed_in(&home());
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Settings));
    assert_eq!(s.screen, Screen::Settings);

    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds, vec![Command::SignOut]);
    assert!(s.is_signed_in(), "the account stays until the event");

    let (s, cmds) = reduce(&s, Event::SignedOut);
    assert!(!s.is_signed_in());
    assert_eq!(s.screen, Screen::Settings);
    assert_eq!(
        cmds,
        vec![
            Command::StopLiveWatch,
            Command::DisconnectRaids,
            Command::LoadFeed { feed: FeedId::Home }
        ]
    );
}

#[test]
fn a_on_settings_while_signed_out_opens_the_login_and_returns_to_settings() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Settings));
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Login);
    assert_eq!(s.back_stack, vec![Screen::Settings]);
    assert_eq!(cmds, vec![Command::StartLogin]);

    let s = signed_in(&s);
    assert_eq!(s.screen, Screen::Settings);
}

#[test]
fn signing_out_on_the_following_tab_goes_home() {
    let s = following();
    let (s, cmds) = reduce(&s, Event::SignedOut);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.rail.index, RailEntry::Home);
    assert_eq!(s.following, Feed::default(), "follows are not kept");
    assert_eq!(
        cmds,
        vec![
            Command::StopLiveWatch,
            Command::DisconnectRaids,
            Command::LoadFeed { feed: FeedId::Home }
        ]
    );
}

#[test]
fn signing_out_while_watching_keeps_the_stream_and_fixes_the_way_back() {
    let (s, _) = run(following(), &[Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.back_stack, vec![Screen::Following]);

    // Twitch refuses to refresh the tokens mid-stream. The video keeps playing.
    let (s, _) = reduce(&s, Event::SignedOut);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.back_stack, vec![Screen::Home]);
}

#[test]
fn signed_out_following_asks_to_sign_in_again() {
    let (s, _) = reduce(&following(), Event::SignedOut);
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Following));
    assert_eq!(s.screen, Screen::Login);
    assert_eq!(cmds, vec![Command::StartLogin]);
}

#[test]
fn settings_has_no_cards_and_the_account_row_ignores_up_and_right() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Settings));
    let (after, cmds) = run(s.clone(), &[Intent::Up, Intent::Right]);
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

// ---- Settings: choices and saving ------------------------------------------------------------

fn on_settings() -> htpc_core::AppState {
    reduce(&home(), Intent::RailPick(RailEntry::Settings)).0
}

#[test]
fn down_and_up_move_between_the_settings_rows_and_stop_at_the_ends() {
    let (s, _) = run(on_settings(), &[Intent::Down]);
    assert_eq!(s.settings_focus, SettingsRow::ChatLayout);
    let (s, _) = run(s, &[Intent::Down; 5]);
    assert_eq!(s.settings_focus, SettingsRow::ShowKick);
    let (s, _) = run(s, &[Intent::Up; 6]);
    assert_eq!(s.settings_focus, SettingsRow::Account);
}

#[test]
fn show_kick_starts_off_and_toggling_it_reloads_home_with_kick() {
    let s = on_settings();
    assert!(!s.settings.show_kick);
    let (s, cmds) = run(s, &[Intent::Down; 5]);
    assert!(cmds.is_empty());
    let (on, cmds) = reduce(&s, Intent::Right);
    assert!(on.settings.show_kick);
    // The provider hears first, so the Home load after it already has the Kick row.
    assert_eq!(
        cmds,
        vec![
            Command::SetKick(true),
            Command::LoadFeed { feed: FeedId::Home },
            Command::SaveSettings(on.settings),
        ]
    );
    assert!(on.home.loading);
    let (off, cmds) = reduce(&on, Intent::Left);
    assert!(!off.settings.show_kick);
    assert_eq!(cmds[0], Command::SetKick(false));
}

#[test]
fn a_changes_a_choice_and_asks_for_it_to_be_saved() {
    let (s, cmds) = run(on_settings(), &[Intent::Down, Intent::Select]);
    assert_eq!(s.settings.chat_layout, ChatLayout::Overlay);
    assert_eq!(cmds, vec![Command::SaveSettings(s.settings)]);
}

#[test]
fn left_and_right_step_a_choice_both_ways() {
    let (s, _) = run(on_settings(), &[Intent::Down, Intent::Down]);
    let (s, cmds) = reduce(&s, Intent::Right);
    assert_eq!(s.settings.quality, Quality::P1080);
    assert_eq!(cmds, vec![Command::SaveSettings(s.settings)]);
    let (s, _) = reduce(&s, Intent::Left);
    assert_eq!(s.settings.quality, Quality::Best);
    let (s, _) = reduce(&s, Intent::Left);
    assert_eq!(s.settings.quality, Quality::Worst, "wraps around");
}

#[test]
fn the_new_quality_is_used_by_the_next_stream() {
    let (s, _) = run(on_settings(), &[Intent::Down, Intent::Down, Intent::Select]);
    assert_eq!(s.settings.quality, Quality::P1080);
    let (_, cmds) = reduce(
        &reduce(&s, Intent::RailPick(RailEntry::Home)).0,
        Intent::Select,
    );
    let play = cmds.iter().find_map(|c| match c {
        Command::Play {
            stream: StreamKind::Video(quality),
            ..
        } => Some(*quality),
        _ => None,
    });
    assert_eq!(play, Some(Quality::P1080));
}

#[test]
fn left_on_the_account_row_opens_the_rail_and_b_asks_to_quit() {
    let (s, _) = reduce(&on_settings(), Intent::Back);
    assert!(s.exit_prompt && !s.rail.focused);
    assert_eq!(s.screen, Screen::Settings);
    let (s, _) = reduce(&on_settings(), Intent::Left);
    assert!(s.rail.focused);
    // On a choice, Left changes the value instead.
    let (s, _) = run(on_settings(), &[Intent::Down, Intent::Left]);
    assert!(!s.rail.focused);
}

#[test]
fn a_on_a_choice_never_signs_out() {
    let s = signed_in(&home());
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Settings));
    let (_, cmds) = run(s, &[Intent::Down, Intent::Select]);
    assert!(!cmds.contains(&Command::SignOut));
}

// ---- the Following tab -----------------------------------------------------------------------

#[test]
fn following_while_signed_in_opens_and_loads_the_tab() {
    let s = signed_in(&home());
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Following));
    assert_eq!(s.screen, Screen::Following);
    assert!(s.back_stack.is_empty());
    assert!(s.following.loading);
    assert_eq!(s.rail.index, RailEntry::Following);
    let feed = FeedId::Following;
    assert_eq!(cmds, vec![Command::LoadFeed { feed }]);
}

#[test]
fn following_rows_navigate_on_their_own_focus() {
    let (s, _) = run(following(), &[Intent::Right, Intent::Down]);
    assert_eq!(s.following.focus, Focus { row: 1, col: 0 });
    assert_eq!(s.home.focus, Focus::default(), "Home keeps its own focus");
}

#[test]
fn a_live_followed_channel_plays_and_back_returns_to_following() {
    let (s, cmds) = run(following(), &[Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::Play { slot: 0, channel, .. } if channel.login == "live0")));

    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Following);
    assert_eq!(s.following.focus, Focus::default());
}

#[test]
fn an_offline_followed_channel_still_tries_to_play() {
    let (s, cmds) = run(following(), &[Intent::Down, Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.playback, Playback::Loading);
    assert_eq!(
        s.now_playing.as_ref().map(|p| p.channel.login.as_str()),
        Some("off0")
    );
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::Play { slot: 0, channel, .. } if channel.login == "off0")));
    let join = Command::JoinChat {
        channel: ChannelRef::twitch("off0"),
    };
    assert!(cmds.contains(&join));

    // The resolver says it is still offline: the end screen, like any other offline channel.
    let offline = Event::StreamOffline {
        channel: ChannelRef::twitch("off0"),
    };
    let (s, _) = reduce(&s, offline);
    assert_eq!(s.playback, Playback::Offline);
}

#[test]
fn a_failed_following_load_shows_the_error_and_x_retries() {
    let s = signed_in(&home());
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Following));
    let failed = Event::FeedFailed {
        feed: FeedId::Following,
        message: "offline".into(),
    };
    let (s, _) = reduce(&s, failed);
    assert_eq!(s.following.error.as_deref(), Some("offline"));

    let (s, cmds) = reduce(&s, Intent::Retry);
    assert!(s.following.loading);
    let feed = FeedId::Following;
    assert_eq!(cmds, vec![Command::LoadFeed { feed }]);
}

#[test]
fn reopening_the_tab_refreshes_it_and_keeps_the_old_rows_meanwhile() {
    let s = following();
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Home));
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Following));
    assert!(s.following.loading);
    assert_eq!(
        s.following.rows.len(),
        2,
        "stale rows stay until the answer"
    );
    assert_eq!(cmds.len(), 1);
}

#[test]
fn followed_tiles_are_keyed_by_login_so_a_live_channel_is_not_listed_twice() {
    assert_eq!(stream("abc").key(), offline_channel("abc").key());
    assert_eq!(offline_channel("abc").thumbnail(), None);
}
