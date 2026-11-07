//! `insta` snapshots of the view-model for each Browse state. The view-model is rendered as plain
//! text (one line per card) so a diff in review shows exactly which card changed.
//!
//! Review changes with `cargo insta review`, or accept them with `INSTA_UPDATE=always cargo test`.

use htpc_core::{
    grid_rows, init, reduce, AppState, Category, ChannelRef, ChatKind, ChatLayout, ChatMessage,
    ChatSpan, ChatStatus, Event, FeedId, Intent, Moderation, PageOutcome, Paging, PlayerMenuRow,
    PlayerStats, RailEntry, Rgb, Row, RowId, RowKind, Stream, Tile,
};
use htpc_ui::{view_model, CardVm, ViewModel};

fn stream(channel: &str, viewers: u64, image: bool) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::twitch(channel),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "VALORANT".into(),
        viewers,
        tint: Rgb::from_hex(0x7a2e2e),
        thumbnail: image.then(|| format!("https://cdn/{channel}.jpg")),
    })
}

fn category(id: &str, name: &str) -> Tile {
    Tile::Category(Category {
        id: id.into(),
        name: name.into(),
        tint: Rgb::from_hex(0x6b5a2d),
        thumbnail: Some(format!("https://cdn/{id}.jpg")),
    })
}

fn card(c: &CardVm) -> String {
    if c.loading {
        return "[skeleton]".into();
    }
    if c.retry {
        return "[retry]".into();
    }
    let mut out = format!("[{}", c.channel);
    if !c.viewers.is_empty() {
        out += &format!(" | {} | {} viewers", c.category, c.viewers);
    }
    if !c.thumb_url.is_empty() {
        out += " | img";
    }
    out + "]"
}

fn render(vm: &ViewModel) -> String {
    let mut out = format!(
        "screen={:?} heading={:?} focus=({}, {}) rail_current={}\n",
        vm.screen, vm.heading, vm.sel_row, vm.sel_col, vm.rail_current
    );
    out += &format!(
        "select={:?} retry={} back={} empty={} error={:?}\n",
        vm.select_label, vm.can_retry, vm.can_back, vm.feed_empty, vm.feed_error
    );
    for (i, row) in vm.rows.iter().enumerate() {
        let cards: Vec<_> = row.cards.iter().map(card).collect();
        out += &format!(
            "row {i} {:?} {:?}: {}\n",
            row.shape,
            row.title,
            cards.join(" ")
        );
    }
    out
}

fn send(s: &AppState, intents: &[Intent]) -> AppState {
    intents.iter().fold(s.clone(), |s, i| reduce(&s, *i).0)
}

fn home_loaded() -> AppState {
    let rows = vec![
        Row::new(
            RowKind::Featured,
            vec![stream("grandmaster_k", 48_100, true)],
        ),
        Row::new(
            RowKind::FollowedLive,
            vec![
                stream("nightowl_dev", 4_200, true),
                stream("marrow", 1_900, false),
            ],
        ),
        Row::new(
            RowKind::TopLive,
            vec![
                stream("tidewatch", 31_500, true),
                stream("brickby", 22_400, true),
            ],
        ),
        Row::new(
            RowKind::Categories,
            vec![
                category("509658", "Just Chatting"),
                category("516575", "VALORANT"),
            ],
        ),
    ];
    let (s, _) = init();
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

#[test]
fn home_while_loading() {
    let (s, _) = init();
    insta::assert_snapshot!(render(&view_model(&s)));
}

#[test]
fn home_loaded_with_the_hero_focused() {
    insta::assert_snapshot!(render(&view_model(&home_loaded())));
}

#[test]
fn home_with_a_next_page_loading() {
    let tiles: Vec<Tile> = (0..8)
        .map(|i| stream(&format!("s{i}"), 100, false))
        .collect();
    let row = Row::new(RowKind::TopLive, tiles).with_paging(Paging::More("c".into()));
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![row],
        },
    )
    .0;
    insta::assert_snapshot!(render(&view_model(&send(&s, &[Intent::Right; 4]))));
}

#[test]
fn home_with_a_failed_page() {
    let s = send(&home_loaded(), &[Intent::Down, Intent::Down]);
    // Row 2 (Top Live) is the paging row in this fixture.
    let mut s = s;
    s.home.rows[2].paging = Paging::Loading("c".into());
    let failed = Event::MoreLoaded {
        feed: FeedId::Home,
        row: RowId::twitch(RowKind::TopLive),
        cursor: "c".into(),
        outcome: PageOutcome::Failed,
    };
    insta::assert_snapshot!(render(&view_model(&reduce(&s, failed).0)));
}

#[test]
fn home_failed_to_load() {
    let (s, _) = init();
    let failed = Event::FeedFailed {
        feed: FeedId::Home,
        message: "network error: timed out".into(),
    };
    insta::assert_snapshot!(render(&view_model(&reduce(&s, failed).0)));
}

fn browse() -> AppState {
    let (s, _) = reduce(&home_loaded(), Intent::RailPick(RailEntry::Browse));
    let tiles: Vec<Tile> = (0..7)
        .map(|i| category(&format!("g{i}"), &format!("Game {i}")))
        .collect();
    let rows = grid_rows(RowKind::Categories, tiles, Paging::More("next".into()));
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Browse,
            rows,
        },
    )
    .0
}

#[test]
fn browse_directory() {
    insta::assert_snapshot!(render(&view_model(&browse())));
}

#[test]
fn category_streams_while_loading_and_loaded() {
    let (s, _) = reduce(&browse(), Intent::Select);
    let loading = render(&view_model(&s));
    let rows = grid_rows(
        RowKind::CategoryStreams,
        (0..4)
            .map(|i| stream(&format!("c{i}"), 500 * (i + 1), i != 1))
            .collect(),
        Paging::End,
    );
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Category("g0".into()),
            rows,
        },
    )
    .0;
    insta::assert_snapshot!(format!("{loading}---\n{}", render(&view_model(&s))));
}

fn player_line(vm: &ViewModel) -> String {
    format!(
        "screen={:?} playback={:?} error={:?} paused={} select={:?} retry={}\n",
        vm.screen, vm.playback, vm.playback_error, vm.paused, vm.select_label, vm.can_retry
    )
}

#[test]
fn player_loading_playing_paused_offline_and_failed() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let mut out = player_line(&view_model(&s));
    let s = reduce(&s, Event::PlaybackStarted).0;
    out += &player_line(&view_model(&s));
    let paused = reduce(&s, Intent::Select).0;
    out += &player_line(&view_model(&paused));
    let ended = reduce(&s, Event::PlaybackEnded).0;
    out += &player_line(&view_model(&ended));
    let (loading, _) = reduce(&home_loaded(), Intent::Select);
    let failed = reduce(
        &loading,
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("grandmaster_k"),
            message: "Streamlink is not installed".into(),
        },
    )
    .0;
    out += &player_line(&view_model(&failed));
    insta::assert_snapshot!(out);
}

fn menu_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "menu_open={} focus={} select={:?} values={:?}\n",
        vm.player_menu_open, vm.player_menu_focus, vm.select_label, vm.player_menu_values
    );
    if !vm.stats_text.is_empty() {
        for line in vm.stats_text.lines() {
            out += &format!("  stats| {line}\n");
        }
    }
    out
}

#[test]
fn player_menu_and_stats_box() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let mut out = menu_lines(&view_model(&s));
    // Start, then down to Zoom and fill the screen.
    let s = send(&s, &[Intent::Menu]);
    out += &menu_lines(&view_model(&s));
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Right],
    );
    out += &menu_lines(&view_model(&s));
    // Stats on: waiting for the first report, then the numbers.
    let s = send(&s, &[Intent::Down, Intent::Right]);
    out += &menu_lines(&view_model(&s));
    let report = PlayerStats {
        width: Some(1920),
        height: Some(1080),
        fps_milli: Some(59_940),
        codec: Some("h264".into()),
        hwdec: Some("vaapi".into()),
        video_bitrate: Some(6_240_000),
        audio_bitrate: Some(160_000),
        audio_codec: Some("aac".into()),
        buffer_ms: Some(1_450),
        dropped_frames: Some(3),
        decoder_dropped_frames: Some(0),
    };
    let s = reduce(&s, Event::PlayerStats(report)).0;
    out += &menu_lines(&view_model(&s));
    // B closes the menu, and the stats box stays.
    let s = send(&s, &[Intent::Back]);
    out += &menu_lines(&view_model(&s));
    // A report that knows nothing yet (software decoding, nothing measured).
    let s = reduce(&s, Event::PlayerStats(PlayerStats::default())).0;
    out += &menu_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

#[test]
fn player_audio_only() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    // Stats on, then down to Audio only and turn it on.
    let s = send(&s, &[Intent::Menu, Intent::Down, Intent::Down]);
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Right, Intent::Down],
    );
    let s = send(&s, &[Intent::Right]);
    let mut out = format!("audio_only={}\n", view_model(&s).audio_only);
    out += &menu_lines(&view_model(&s));
    let s = reduce(&s, Event::PlaybackStarted).0;
    let report = PlayerStats {
        audio_bitrate: Some(160_000),
        audio_codec: Some("aac".into()),
        buffer_ms: Some(2_000),
        ..PlayerStats::default()
    };
    let s = reduce(&s, Event::PlayerStats(report)).0;
    out += &menu_lines(&view_model(&s));
    // Up to the locked Quality row: A changes nothing.
    let s = send(&s, &[Intent::Up; 5]);
    let s = send(&s, &[Intent::Select]);
    out += &menu_lines(&view_model(&s));
    // Leaving the player turns it off.
    let s = send(&s, &[Intent::Back, Intent::Back]);
    out += &format!("after leaving: audio_only={}\n", view_model(&s).audio_only);
    insta::assert_snapshot!(out);
}

fn sleep_lines(vm: &ViewModel) -> String {
    format!(
        "screen={:?} sleep_label={:?} warning={} menu_value={:?}\n",
        vm.screen,
        vm.sleep_label,
        vm.sleep_warning,
        vm.player_menu_values
            .get(PlayerMenuRow::SleepTimer.index())
            .map(String::as_str)
            .unwrap_or("")
    )
}

fn subscribe_lines(vm: &ViewModel) -> String {
    format!(
        "menu_open={} focus={} select={:?} card={} url={:?} qr={}x{} can_open={}\n",
        vm.player_menu_open,
        vm.player_menu_focus,
        vm.select_label,
        vm.subscribe_open,
        vm.subscribe_url,
        vm.subscribe_qr_width,
        vm.subscribe_qr_width,
        vm.subscribe_can_open
    )
}

#[test]
fn subscribe_card_with_and_without_a_browser() {
    let mut out = String::new();
    for can_open_browser in [true, false] {
        let mut s = home_loaded();
        s.can_open_browser = can_open_browser;
        out += &format!("can_open_browser={can_open_browser}\n");
        let (s, _) = reduce(&s, Intent::Select);
        let s = reduce(&s, Event::PlaybackStarted).0;
        // Start, down to the last row.
        let s = send(&s, &[Intent::Menu]);
        let s = send(&s, &[Intent::Down; 7]);
        out += &subscribe_lines(&view_model(&s));
        // A opens the card, B closes it again.
        let s = send(&s, &[Intent::Select]);
        out += &subscribe_lines(&view_model(&s));
        let s = send(&s, &[Intent::Back]);
        out += &subscribe_lines(&view_model(&s));
    }
    insta::assert_snapshot!(out);
}

#[test]
fn sleep_timer_countdown_and_warning() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let mut out = sleep_lines(&view_model(&s));
    // Start, down to the sleep timer, 15 then 30 minutes.
    let s = send(&s, &[Intent::Menu]);
    let s = send(&s, &[Intent::Down; 6]);
    let s = send(&s, &[Intent::Right, Intent::Right]);
    out += &sleep_lines(&view_model(&s));
    let id = s.sleep.unwrap().id;
    let s = reduce(&s, Event::SleepTimerTick { id, left_min: 2 }).0;
    out += &sleep_lines(&view_model(&s));
    let s = send(&s, &[Intent::Back]);
    let s = reduce(&s, Event::SleepTimerWarning { id }).0;
    out += &sleep_lines(&view_model(&s));
    // A button keeps going: a fresh 30 minutes.
    let s = send(&s, &[Intent::Select]);
    out += &sleep_lines(&view_model(&s));
    // Left on the browse screens, the warning still shows.
    let s = send(&s, &[Intent::Back]);
    let id = s.sleep.unwrap().id;
    let s = reduce(&s, Event::SleepTimerWarning { id }).0;
    out += &sleep_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

fn raid_lines(vm: &ViewModel) -> String {
    format!(
        "screen={:?} channel={:?} playback={:?} raid_open={} title={:?} viewers={:?} seconds={} select={:?} retry={}\n",
        vm.screen,
        vm.now_playing.channel,
        vm.playback,
        vm.raid_open,
        vm.raid_title,
        vm.raid_viewers,
        vm.raid_seconds,
        vm.select_label,
        vm.can_retry
    )
}

#[test]
fn raid_card_counts_down_then_follows() {
    let user = htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let raid = |viewers| Event::Raided {
        from_login: "grandmaster_k".into(),
        to_login: "next_up".into(),
        to_name: "Next_Up".into(),
        viewers,
    };
    let mut out = String::from("-- playing\n");
    out += &raid_lines(&view_model(&s));
    let s = reduce(&s, raid(48_100)).0;
    out += "-- raided\n";
    out += &raid_lines(&view_model(&s));
    let id = s.raid.as_ref().unwrap().id;
    let s = reduce(&s, Event::RaidTick { id, left: 3 }).0;
    out += "-- three seconds left\n";
    out += &raid_lines(&view_model(&s));
    out += "-- one viewer, none\n";
    for viewers in [1, 0] {
        let (stay, _) = reduce(&s, Intent::Back);
        out += &raid_lines(&view_model(&reduce(&stay, raid(viewers)).0));
    }
    let s = reduce(&s, Event::RaidCountdownDone { id }).0;
    out += "-- followed\n";
    out += &raid_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

fn hype_lines(vm: &ViewModel) -> String {
    format!(
        "screen={:?} channel={:?} hype_open={} level={} permille={} points={:?} seconds={}\n",
        vm.screen,
        vm.now_playing.channel,
        vm.hype_open,
        vm.hype_level,
        vm.hype_permille,
        vm.hype_points,
        vm.hype_seconds,
    )
}

#[test]
fn hype_train_bar_follows_the_polls() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let channel = s.now_playing.as_ref().unwrap().channel.login.clone();
    let hype = |level, progress, goal, seconds_left| Event::HypeTrain {
        channel: channel.clone(),
        train: Some(htpc_core::HypeTrain {
            level,
            progress,
            goal,
            seconds_left,
        }),
    };
    let mut out = String::from("-- playing\n");
    out += &hype_lines(&view_model(&s));
    let s = reduce(&s, hype(1, 1_501, 1_600, 211)).0;
    out += "-- level 1\n";
    out += &hype_lines(&view_model(&s));
    let s = reduce(&s, hype(2, 401, 1_800, 297)).0;
    out += "-- level up\n";
    out += &hype_lines(&view_model(&s));
    let s = reduce(&s, hype(2, 2_000, 1_800, 12)).0;
    out += "-- overshoot is drawn full\n";
    out += &hype_lines(&view_model(&s));
    let ended = reduce(
        &s,
        Event::HypeTrain {
            channel: channel.clone(),
            train: None,
        },
    )
    .0;
    out += "-- ended\n";
    out += &hype_lines(&view_model(&ended));
    let (left, _) = reduce(&s, Intent::Back);
    out += "-- left the player\n";
    out += &hype_lines(&view_model(&left));
    insta::assert_snapshot!(out);
}

fn search_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "query={:?} key={} on_keys={} in_results={} select={:?} retry={} status={:?}\n",
        vm.search_query,
        vm.search_key_focus,
        vm.search_on_keys,
        vm.search_in_results,
        vm.select_label,
        vm.can_retry,
        vm.search_status
    );
    out += &render(vm);
    out
}

#[test]
fn search_empty_typing_and_results() {
    let user = htpc_core::User {
        login: "nightowl_dev".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let s = send(&s, &[Intent::RailPick(RailEntry::Search)]);
    let mut out = String::from("-- empty\n");
    out += &search_lines(&view_model(&s));

    // `l`, `o` typed on the keyboard (down a row, right to `o`), then waiting for the answer.
    let s = send(
        &s,
        &[
            Intent::Down,
            Intent::Right,
            Intent::Select,
            Intent::Right,
            Intent::Right,
            Intent::Right,
            Intent::Select,
        ],
    );
    out += "-- typing\n";
    out += &search_lines(&view_model(&s));

    let id = s.search.id;
    let rows = vec![
        Row::new(
            RowKind::SearchLive,
            vec![
                stream("lofi_lena", 860, true),
                stream("lorekeeper", 42, false),
            ],
        ),
        Row::new(RowKind::SearchCategories, vec![category("27471", "Lords")]),
        Row::new(
            RowKind::SearchOffline,
            vec![Tile::Channel(htpc_core::Channel {
                channel: ChannelRef::twitch("lonely_owl"),
                name: "Lonely_Owl".into(),
                tint: Rgb::from_hex(0x2d4a6b),
            })],
        ),
    ];
    let s = reduce(&s, Event::SearchResults { id, rows }).0;
    out += "-- results, keyboard focused\n";
    out += &search_lines(&view_model(&s));
    let s = send(&s, &[Intent::Menu, Intent::Down]);
    out += "-- results focused\n";
    out += &search_lines(&view_model(&s));

    // Nothing found, and a failed search.
    let s = send(&s, &[Intent::Back, Intent::Text('x')]);
    let id = s.search.id;
    let none = reduce(
        &s,
        Event::SearchResults {
            id,
            rows: Vec::new(),
        },
    )
    .0;
    out += "-- nothing found\n";
    out += &search_lines(&view_model(&none));
    let failed = reduce(
        &s,
        Event::SearchFailed {
            id,
            message: "network error: timed out".into(),
        },
    )
    .0;
    out += "-- failed\n";
    out += &search_lines(&view_model(&failed));
    insta::assert_snapshot!(out);
}

fn toast_lines(vm: &ViewModel) -> String {
    let mut out = format!("screen={:?} more={}\n", vm.screen, vm.toast_more);
    for t in &vm.toasts {
        let img = if t.thumb_url.is_empty() { "" } else { " | img" };
        out += &format!("  [#{} {} is live | {}{img}]\n", t.id, t.name, t.line);
    }
    out
}

#[test]
fn live_notification_toasts_one_three_and_more() {
    let user = htpc_core::User {
        login: "nightowl_dev".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let went_live = |s: &AppState, channels: &[&str]| {
        let streams = channels
            .iter()
            .map(|c| match stream(c, 100, *c != "quietcoast") {
                Tile::Stream(s) => s,
                _ => unreachable!(),
            })
            .collect();
        reduce(s, Event::WentLive(streams)).0
    };
    let mut out = String::from("-- one\n");
    let s = went_live(&s, &["lofi_lena"]);
    out += &toast_lines(&view_model(&s));
    out += "-- three, over the player\n";
    let s = send(&s, &[Intent::Select]);
    let s = went_live(&s, &["marrow", "quietcoast"]);
    out += &toast_lines(&view_model(&s));
    out += "-- five\n";
    let s = went_live(&s, &["pixelpilgrim", "sable"]);
    out += &toast_lines(&view_model(&s));
    out += "-- the oldest expired\n";
    let s = reduce(&s, Event::ToastExpired(1)).0;
    out += &toast_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

fn chat_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "open={} overlay={} status={:?}\n",
        vm.chat_open, vm.chat_overlay, vm.chat_status
    );
    for line in &vm.chat {
        let pieces: Vec<String> = line
            .pieces
            .iter()
            .map(|p| {
                if p.emote {
                    format!("<{}>", p.text)
                } else {
                    p.text.clone()
                }
            })
            .collect();
        out += &format!(
            "{:?} #{:02x}{:02x}{:02x} badges={} {}: {}\n",
            line.kind,
            line.tint.r,
            line.tint.g,
            line.tint.b,
            line.badges.len(),
            line.user,
            pieces.join(" ")
        );
    }
    out
}

#[test]
fn chat_connecting_live_busy_moderated_and_reconnecting() {
    let channel = "grandmaster_k";
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let mut out = chat_lines(&view_model(&s));

    let live = Event::ChatStatus {
        channel: ChannelRef::twitch(channel),
        status: ChatStatus::Live,
    };
    let s = reduce(&s, live).0;
    out += &chat_lines(&view_model(&s));

    let mod_line = ChatMessage {
        id: "1".into(),
        color: Some(Rgb::new(30, 144, 255)),
        badges: vec!["https://cdn/mod.png".into(), "https://cdn/sub.png".into()],
        spans: vec![
            ChatSpan::Text("nice play".into()),
            ChatSpan::Emote {
                name: "Kappa".into(),
                url: "https://cdn/kappa.png".into(),
            },
        ],
        ..ChatMessage::plain("ModMarie", "")
    };
    let troll = ChatMessage {
        id: "2".into(),
        // A near-black name is lightened.
        color: Some(Rgb::new(0, 0, 40)),
        ..ChatMessage::plain("Troll", "buy followers")
    };
    let me = ChatMessage {
        kind: ChatKind::Action,
        color: Some(Rgb::new(255, 105, 180)),
        ..ChatMessage::plain("Waver", "waves at chat")
    };
    let batch = Event::ChatMessages {
        channel: ChannelRef::twitch(channel),
        messages: vec![
            mod_line,
            troll,
            me,
            ChatMessage::notice("This room is now in slow mode."),
            ChatMessage::plain("plain_pat", "no colour set"),
        ],
    };
    let s = reduce(&s, batch).0;
    out += &chat_lines(&view_model(&s));

    let ban = Event::ChatModerated {
        channel: ChannelRef::twitch(channel),
        action: Moderation::ClearUser("troll".into()),
    };
    let s = reduce(&s, ban).0;
    out += &chat_lines(&view_model(&s));

    let dropped = Event::ChatStatus {
        channel: ChannelRef::twitch(channel),
        status: ChatStatus::Reconnecting,
    };
    let mut s = reduce(&s, dropped).0;
    s.chat_open = true;
    s.settings.chat_layout = ChatLayout::Overlay;
    out += &chat_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

fn compose_lines(vm: &ViewModel) -> String {
    let keys: Vec<&str> = vm.compose_keys.iter().map(|k| k.label.as_str()).collect();
    let focused = usize::try_from(vm.compose_key_focus)
        .ok()
        .and_then(|i| vm.compose_keys.get(i))
        .map_or("-", |k| k.label.as_str());
    format!(
        "open={} needs_sign_in={} overlay={} select={:?} text={:?} count={:?} status={:?} problem={} room={:?} hint={:?}\n  chips={:?} chip_focus={} shift={} key={:?} keys={}\n",
        vm.compose_open,
        vm.compose_needs_sign_in,
        vm.chat_overlay,
        vm.select_label,
        vm.compose_text,
        vm.compose_count,
        vm.compose_status,
        vm.compose_problem,
        vm.room_mode,
        vm.compose_hint,
        vm.compose_chips,
        vm.compose_chip_focus,
        vm.compose_shift,
        focused,
        keys.join(" ")
    )
}

#[test]
fn chat_compose_side_overlay_and_a_drop() {
    let viewer = |can_chat| htpc_core::User {
        login: "viewer".into(),
        can_chat,
    };
    let watching = |user: Option<htpc_core::User>| {
        let mut s = home_loaded();
        s.user = user;
        let (s, _) = reduce(&s, Intent::Select);
        let mut s = reduce(&s, Event::PlaybackStarted).0;
        s.chat_open = true;
        s
    };
    let mut out = String::from("-- signed out\n");
    out += &compose_lines(&view_model(&watching(None)));
    out += "-- signed in, not writing\n";
    let s = watching(Some(viewer(true)));
    out += &compose_lines(&view_model(&s));

    out += "-- side panel, typing with shift on\n";
    let s = send(&s, &[Intent::Retry, Intent::Text('G'), Intent::Text('G')]);
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Select],
    );
    out += &compose_lines(&view_model(&s));

    out += "-- overlay, the emote page\n";
    let mut overlay = send(&s, &[Intent::PageLeft]);
    overlay.settings.chat_layout = ChatLayout::Overlay;
    out += &compose_lines(&view_model(&overlay));

    out += "-- a quick message focused\n";
    let chips = send(
        &s,
        &[
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Right,
        ],
    );
    out += &compose_lines(&view_model(&chips));

    out += "-- sending, in slow mode\n";
    let s = reduce(
        &s,
        Event::ChatRoomMode {
            channel: ChannelRef::twitch("grandmaster_k"),
            change: htpc_core::RoomModeChange {
                slow_secs: Some(30),
                followers_min: Some(Some(10)),
                subs_only: Some(false),
                emote_only: Some(false),
            },
        },
    )
    .0;
    let s = send(&s, &[Intent::Menu]);
    out += &compose_lines(&view_model(&s));

    out += "-- dropped by slow mode\n";
    let id = s.compose.sending.as_ref().unwrap().id;
    let s = reduce(
        &s,
        Event::ChatDropped {
            id,
            problem: htpc_core::SendProblem::Dropped {
                code: "msg_slowmode".into(),
                message: "This room is in slow mode and you are sending messages too quickly."
                    .into(),
            },
        },
    )
    .0;
    out += &compose_lines(&view_model(&s));

    out += "-- closed with a draft\n";
    let s = send(&s, &[Intent::Back]);
    out += &compose_lines(&view_model(&s));

    out += "-- a sign-in from before chat sending\n";
    let s = send(&watching(Some(viewer(false))), &[]);
    out += &compose_lines(&view_model(&s));
    let s = send(&s, &[Intent::Retry]);
    out += &compose_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}
