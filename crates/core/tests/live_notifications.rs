//! Live notifications: the watch follows the account and the setting, toasts are added, capped
//! and expire, and the channel on screen is never announced.

use htpc_core::*;

fn stream(channel: &str) -> Stream {
    Stream {
        channel: channel.into(),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 10,
        tint: Rgb::default(),
        thumbnail: Some(format!("https://cdn/{channel}.jpg")),
    }
}

fn home() -> AppState {
    let (s, _) = init();
    let rows = vec![Row::new(RowKind::TopLive, vec![Tile::Stream(stream("t0"))])];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

fn sign_in(s: &AppState) -> (AppState, Vec<Command>) {
    let user = User {
        login: "nightowl_dev".into(),
        can_chat: true,
    };
    reduce(s, Event::SignedIn(user))
}

fn signed_in() -> AppState {
    sign_in(&home()).0
}

fn went_live(s: &AppState, channels: &[&str]) -> (AppState, Vec<Command>) {
    let streams = channels.iter().map(|c| stream(c)).collect();
    reduce(s, Event::WentLive(streams))
}

fn toast_channels(s: &AppState) -> Vec<&str> {
    s.toasts.iter().map(|t| t.stream.channel.as_str()).collect()
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

/// The Settings screen with focus on the live notifications row.
fn on_the_setting(s: &AppState) -> AppState {
    let (s, _) = reduce(s, Intent::RailPick(RailEntry::Settings));
    run(s, &[Intent::Down; 3]).0
}

// ---- the watch -------------------------------------------------------------------------------

#[test]
fn signing_in_starts_the_watch_and_signing_out_stops_it() {
    let (s, cmds) = sign_in(&home());
    assert!(cmds.contains(&Command::StartLiveWatch));
    assert!(s.wants_live_watch());
    let (s, _) = went_live(&s, &["a"]);
    let (s, cmds) = reduce(&s, Event::SignedOut);
    assert!(cmds.contains(&Command::StopLiveWatch));
    assert!(s.toasts.is_empty());
    assert!(!s.wants_live_watch());
}

#[test]
fn with_the_setting_off_signing_in_starts_nothing() {
    let mut s = home();
    s.settings.live_notifications = false;
    let (_, cmds) = sign_in(&s);
    assert!(!cmds.contains(&Command::StartLiveWatch));
}

#[test]
fn the_setting_turns_the_watch_off_and_on_and_is_saved() {
    let s = on_the_setting(&signed_in());
    assert_eq!(s.settings_focus, SettingsRow::LiveNotifications);
    let (s, _) = went_live(&s, &["a"]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(!s.settings.live_notifications);
    assert!(s.toasts.is_empty(), "turning it off clears the toasts");
    assert_eq!(
        cmds,
        [Command::StopLiveWatch, Command::SaveSettings(s.settings)]
    );
    // Left and Right toggle as well.
    let (s, cmds) = reduce(&s, Intent::Left);
    assert!(s.settings.live_notifications);
    assert_eq!(
        cmds,
        [Command::StartLiveWatch, Command::SaveSettings(s.settings)]
    );
}

#[test]
fn signed_out_the_setting_only_saves() {
    let s = on_the_setting(&home());
    let (s, cmds) = reduce(&s, Intent::Right);
    assert!(!s.settings.live_notifications);
    assert_eq!(cmds, [Command::SaveSettings(s.settings)]);
}

// ---- toasts ----------------------------------------------------------------------------------

#[test]
fn each_channel_gets_a_toast_newest_first_that_expires() {
    let (s, cmds) = went_live(&signed_in(), &["a", "b"]);
    assert_eq!(toast_channels(&s), ["b", "a"]);
    let ids: Vec<u32> = s.toasts.iter().map(|t| t.id).collect();
    assert_eq!(ids, [2, 1]);
    assert!(cmds.contains(&Command::ExpireToast { id: 1 }));
    assert!(cmds.contains(&Command::ExpireToast { id: 2 }));
    assert!(cmds.contains(&Command::FetchThumbnails {
        urls: vec!["https://cdn/a.jpg".into(), "https://cdn/b.jpg".into()]
    }));

    let (s, _) = went_live(&s, &["c"]);
    assert_eq!(toast_channels(&s), ["c", "b", "a"]);
    let (s, cmds) = reduce(&s, Event::ToastExpired(1));
    assert_eq!(toast_channels(&s), ["c", "b"]);
    assert!(cmds.is_empty());
    // An unknown or repeated expiry changes nothing.
    let (again, _) = reduce(&s, Event::ToastExpired(1));
    assert_eq!(again, s);
}

#[test]
fn a_channel_already_on_a_toast_is_not_announced_twice() {
    let (s, _) = went_live(&signed_in(), &["a"]);
    let (s, cmds) = went_live(&s, &["A", "a"]);
    assert_eq!(toast_channels(&s), ["a"]);
    assert!(cmds.is_empty());
}

#[test]
fn the_channel_being_watched_is_never_announced() {
    let s = signed_in();
    // Home's only card is t0.
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Player);
    let (s, _) = went_live(&s, &["t0", "other"]);
    assert_eq!(toast_channels(&s), ["other"]);
}

#[test]
fn toasts_show_on_every_screen_and_take_no_buttons() {
    let (s, _) = went_live(&signed_in(), &["a"]);
    // A still opens the focused card, B still asks to quit.
    let (watching, _) = reduce(&s, Intent::Select);
    assert_eq!(watching.screen, Screen::Player);
    assert_eq!(toast_channels(&watching), ["a"]);
    let (asking, _) = reduce(&s, Intent::Back);
    assert!(asking.exit_prompt);
}

#[test]
fn a_burst_keeps_only_the_newest() {
    let names: Vec<String> = (0..TOASTS_KEPT + 5).map(|i| format!("c{i}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let (s, _) = went_live(&signed_in(), &names);
    assert_eq!(s.toasts.len(), TOASTS_KEPT);
    assert_eq!(s.toasts[0].stream.channel, format!("c{}", TOASTS_KEPT + 4));
}

#[test]
fn late_reports_are_dropped_when_the_watch_should_not_run() {
    let (s, cmds) = went_live(&home(), &["a"]);
    assert!(s.toasts.is_empty() && cmds.is_empty(), "signed out");
    let mut off = signed_in();
    off.settings.live_notifications = false;
    let (s, _) = went_live(&off, &["a"]);
    assert!(s.toasts.is_empty(), "setting off");
}
