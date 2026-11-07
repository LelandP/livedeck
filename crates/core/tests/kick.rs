//! Kick streams: watched and read like Twitch's, without Twitch's own features (raids, hype
//! trains, subscribing, writing in chat).

use htpc_core::*;

fn stream(channel: ChannelRef) -> Tile {
    Tile::Stream(Stream {
        user_id: "77".into(),
        title: format!("{} title", channel.login),
        category: "Cat".into(),
        viewers: 500,
        tint: Rgb::default(),
        thumbnail: None,
        channel,
    })
}

/// Home with a Kick row (signed in to Twitch, so Twitch's features would apply to Twitch).
fn home() -> AppState {
    let (s, _) = init();
    let mut s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(
                RowKind::TopLive,
                vec![
                    stream(ChannelRef::kick("awa-xo")),
                    stream(ChannelRef::twitch("ender")),
                ],
            )
            .on(Platform::Kick)],
        },
    )
    .0;
    s = reduce(
        &s,
        Event::SignedIn(User {
            login: "viewer".into(),
            can_chat: true,
        }),
    )
    .0;
    s
}

fn watching_kick() -> (AppState, Vec<Command>) {
    let (s, cmds) = reduce(&home(), Intent::Select);
    (reduce(&s, Event::PlaybackStarted { slot: 0 }).0, cmds)
}

#[test]
fn a_kick_stream_plays_and_joins_kick_chat_without_twitch_watches() {
    let (s, cmds) = watching_kick();
    let kick = ChannelRef::kick("awa-xo");
    assert_eq!(s.now_playing.as_ref().unwrap().channel, kick);
    assert!(cmds.contains(&Command::JoinChat {
        channel: kick.clone()
    }));
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::Play { slot: 0, channel, .. } if *channel == kick)));
    // The hype-train watch of an earlier Twitch stream stops; nothing watches raids.
    assert!(cmds.contains(&Command::StopHypeTrain));
    assert!(!cmds.iter().any(|c| matches!(
        c,
        Command::WatchHypeTrain { .. }
            | Command::WatchRaids { .. }
            | Command::FetchStreamInfo { .. }
    )));
}

#[test]
fn a_twitch_stream_still_gets_its_watches() {
    let (s, _) = reduce(&home(), Intent::Right);
    let (_, cmds) = reduce(&s, Intent::Select);
    assert!(cmds.contains(&Command::WatchHypeTrain {
        channel: "ender".into()
    }));
    assert!(cmds.contains(&Command::WatchRaids {
        broadcaster_id: "77".into()
    }));
}

#[test]
fn chat_lines_of_the_kick_channel_are_shown_and_twitchs_namesake_ignored() {
    let (s, _) = watching_kick();
    let line = |channel| Event::ChatMessages {
        channel,
        messages: vec![ChatMessage::plain("a", "hi")],
    };
    let (s, _) = reduce(&s, line(ChannelRef::twitch("awa-xo")));
    assert!(s.chat.is_empty());
    let (s, _) = reduce(&s, line(ChannelRef::kick("awa-xo")));
    assert_eq!(s.chat.len(), 1);
}

#[test]
fn writing_in_chat_and_subscribing_stay_closed_on_kick() {
    let (s, _) = watching_kick();
    let (s, _) = reduce(&s, Intent::ToggleChat);
    // X with chat shown writes in chat on Twitch.
    let (after_x, _) = reduce(&s, Intent::Retry);
    assert!(!after_x.compose.open);
    // The player menu's Subscribe and Write in chat rows do nothing.
    let (menu, _) = reduce(&s, Intent::Menu);
    let mut at = menu;
    while at.player_menu != Some(PlayerMenuRow::Subscribe) {
        at = reduce(&at, Intent::Down).0;
    }
    let (sub, _) = reduce(&at, Intent::Select);
    assert!(!sub.subscribe_card);
    let (chat_row, _) = reduce(&at, Intent::Down);
    let (write, _) = reduce(&chat_row, Intent::Select);
    assert!(!write.compose.open);
}

#[test]
fn a_raid_report_for_a_twitch_namesake_does_not_move_a_kick_viewer() {
    let (s, _) = watching_kick();
    let (s, cmds) = reduce(
        &s,
        Event::Raided {
            from_login: "awa-xo".into(),
            to_login: "elsewhere".into(),
            to_name: "Elsewhere".into(),
            viewers: 10,
        },
    );
    assert!(s.raid.is_none());
    assert!(cmds.is_empty());
}

#[test]
fn twitchs_features_are_on_when_nothing_plays() {
    let (s, _) = init();
    assert_eq!(s.on_screen_features(), Platform::Twitch.features());
}
