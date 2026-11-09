//! The pure reducer: `(state, msg) -> (new state, commands)`. No IO, no clocks, no randomness.
//!
//! All navigation behaviour lives here, not in the Slint files: the back stack, paged feeds (Home,
//! Browse, Category) and the commands needed to fetch data.

mod favorites;
mod multiview;
mod network;
mod resume;

use std::collections::HashSet;

use crate::domain::{
    grid_rows_on, ChannelRef, ChatMessage, ChatStatus, FeedId, Paging, Platform, RoomMode, RowId,
    Stream, Tile,
};
use crate::keyboard::{self, Key};
use crate::msg::{Command, Event, Intent, Msg, PageOutcome, PlayerOption, MAIN_SLOT};
use crate::state::{
    default_quick_messages, subscribe_url, AppState, CategoryFeed, Compose, Feed, Playback,
    PlayerMenuRow, RaidOffer, RailEntry, Screen, SearchState, Sending, SettingsRow, SleepTimer,
    Toast, ToastKind, RAID_SECONDS, TOASTS_KEPT,
};

/// A horizontal row loads its next page once focus is within this many tiles of its end.
pub const PREFETCH_DISTANCE: usize = 3;

/// A grid loads its next page once focus is within this many rows of its last row.
pub const PREFETCH_ROWS: usize = 1;

/// Initial state plus the commands to run at startup.
pub fn init() -> (AppState, Vec<Command>) {
    let state = AppState {
        home: Feed::loading(),
        quick_messages: default_quick_messages(),
        ..AppState::default()
    };
    (state, vec![Command::LoadFeed { feed: FeedId::Home }])
}

/// Applies one message to the state and returns the new state and the side effects to run.
pub fn reduce(state: &AppState, msg: impl Into<Msg>) -> (AppState, Vec<Command>) {
    let mut next = state.clone();
    let mut cmds = Vec::new();
    match msg.into() {
        Msg::Intent(intent) => on_intent(&mut next, intent, &mut cmds),
        Msg::Event(event) => on_event(&mut next, event, &mut cmds),
    }
    // Wherever it changed: the running multiview session is saved, and Home's resume card kept
    // where it belongs.
    multiview::track_saved(&mut next, &mut cmds);
    resume::settle(&mut next);
    (next, cmds)
}

fn on_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    let Some(intent) = typed_key(s, intent) else {
        return;
    };
    // "Still watching?": any button keeps going, and does nothing else (it must not also pause,
    // open chat or leave the stream).
    if let Some(timer) = s.sleep.filter(|t| t.warning) {
        start_sleep(s, timer.minutes, cmds);
        pulse_hud(s);
        return;
    }
    if s.exit_prompt {
        exit_prompt_intent(s, intent, cmds);
        return;
    }
    if s.popup.is_some() {
        multiview::popup_intent(s, intent, cmds);
        return;
    }
    match s.screen {
        Screen::Login => login_intent(s, intent, cmds),
        Screen::Player => player_intent(s, intent, cmds),
        // Any button shows the grid's controls again (they fade while idle, main.slint).
        Screen::Multiview => {
            multiview::grid_intent(s, intent, cmds);
            pulse_hud(s);
        }
        // Settings has no cards, but shares the rail handling and the Left-opens-the-rail rule.
        Screen::Home
        | Screen::Browse
        | Screen::Category
        | Screen::Following
        | Screen::Favorites
        | Screen::Search
        | Screen::Settings => home_intent(s, intent, cmds),
    }
}

/// What a physical keyboard's typing means here. Only the Search keyboard and the chat compose
/// panel take text (while they have focus and nothing is in front of them). Everywhere else `y`,
/// `r` and `v` stay the chat, retry and View shortcuts, Backspace is Back, and other characters do
/// nothing.
fn typed_key(s: &AppState, intent: Intent) -> Option<Intent> {
    let in_front = s.exit_prompt || s.sleep.is_some_and(|t| t.warning);
    let searching = s.screen == Screen::Search && !s.rail.focused;
    let composing = s.composing() && s.can_chat() && s.raid.is_none() && !s.subscribe_card;
    let typing = (searching || composing) && !in_front;
    match intent {
        Intent::Text(_) | Intent::Erase if typing => Some(intent),
        Intent::Text('y' | 'Y') => Some(Intent::ToggleChat),
        Intent::Text('r' | 'R') => Some(Intent::Retry),
        Intent::Text('v' | 'V') => Some(Intent::View),
        Intent::Text(_) => None,
        Intent::Erase => Some(Intent::Back),
        other => Some(other),
    }
}

/// Input while the quit prompt is open. A quits, B (or the Start button, which closes things)
/// cancels. Nothing else gets past the prompt.
fn exit_prompt_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Select => cmds.push(Command::Quit),
        Intent::Back | Intent::Menu => s.exit_prompt = false,
        _ => {}
    }
}

fn login_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Back => back(s, cmds),
        // The login ends by itself (`Event::SignedIn`). Only after a failure does A or X do
        // something: ask for a fresh code.
        Intent::Select | Intent::Retry => {
            if s.login_problem.is_some() {
                s.login_problem = None;
                s.login = None;
                cmds.push(Command::StartLogin);
            }
        }
        // The rail is visible on the sign-in screen, so mouse clicks on it still work.
        Intent::RailPick(entry) => activate_rail(s, entry, cmds),
        _ => {}
    }
}

/// Input on the Settings screen while the rail is closed. Up and Down move between rows. A, and
/// Left or Right on a choice, change it and save the settings. Left on the account row goes to the
/// rail (the choices use Left for "previous"). B opens the rail, as on every root screen.
fn settings_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Up => s.settings_focus = s.settings_focus.prev(),
        Intent::Down => s.settings_focus = s.settings_focus.next(),
        Intent::Select | Intent::Right => match s.settings_focus {
            SettingsRow::Account if intent == Intent::Select => settings_select(s, cmds),
            SettingsRow::Account => {}
            row => change_setting(s, row, true, cmds),
        },
        Intent::Left => match s.settings_focus {
            SettingsRow::Account => s.rail.focused = true,
            row => change_setting(s, row, false, cmds),
        },
        // Back opens the sidebar, where a second Back asks to quit.
        Intent::Back => s.rail.focused = true,
        _ => {}
    }
}

/// Steps a choice to its next or previous value and asks for the settings to be saved.
fn change_setting(s: &mut AppState, row: SettingsRow, forward: bool, cmds: &mut Vec<Command>) {
    match row {
        SettingsRow::Account => return,
        SettingsRow::ChatLayout => {
            s.settings.chat_layout = s.settings.chat_layout.cycled(forward);
        }
        SettingsRow::Quality => {
            s.settings.quality = s.settings.quality.cycled(forward);
            // The saved quality is where every stream starts again, also in this session.
            s.session.quality = None;
        }
        // On or off: both directions toggle.
        SettingsRow::LiveNotifications => {
            s.settings.live_notifications = !s.settings.live_notifications;
            if !s.settings.live_notifications {
                s.toasts.clear();
            }
            if s.is_signed_in() {
                cmds.push(if s.settings.live_notifications {
                    Command::StartLiveWatch
                } else {
                    Command::StopLiveWatch
                });
            }
        }
        // On or off. It is only read when the window loses focus, so nothing to apply now.
        SettingsRow::BackgroundPlay => {
            s.settings.background_play = !s.settings.background_play;
        }
        // On or off. Home is loaded again, with or without its Kick row.
        SettingsRow::ShowKick => {
            s.settings.show_kick = !s.settings.show_kick;
            cmds.push(Command::SetKick(s.settings.show_kick));
            s.home.loading = true;
            s.home.error = None;
            cmds.push(Command::LoadFeed { feed: FeedId::Home });
        }
    }
    cmds.push(Command::SaveSettings(s.settings));
}

/// A on the Settings screen's account row. Signed in, it signs out (the screen changes when
/// `Event::SignedOut` arrives). Signed out, it opens the sign-in screen.
fn settings_select(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.is_signed_in() {
        cmds.push(Command::SignOut);
    } else {
        open_login(s, None, cmds);
    }
}

/// Opens the sign-in screen on top of the current one and asks for a code. `then` is the screen
/// to continue to once the login succeeds, if it is not the one underneath.
fn open_login(s: &mut AppState, then: Option<Screen>, cmds: &mut Vec<Command>) {
    s.login = None;
    s.login_problem = None;
    s.after_login = then;
    push_screen(s, Screen::Login);
    cmds.push(Command::StartLogin);
}

/// Leaves the sign-in screen's state behind and stops a pending login.
fn leave_login(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.login = None;
    s.login_problem = None;
    s.after_login = None;
    cmds.push(Command::CancelLogin);
}

/// Starts a reload of Home while keeping its rows on screen (stale until the answer arrives).
fn reload_home(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.home.loading = true;
    s.home.error = None;
    cmds.push(Command::LoadFeed { feed: FeedId::Home });
}

/// Reloads the feed on screen, or the one under the player, without a spinner, so the rows pick up
/// streams that went live since the last load. Skipped while that feed is already loading, and on
/// screens whose content is not a feed that lists live streams (Search follows its text, Settings,
/// sign-in and multiview).
fn refresh_visible_feed(s: &AppState, cmds: &mut Vec<Command>) {
    let shows_feed = matches!(
        s.screen,
        Screen::Home
            | Screen::Browse
            | Screen::Category
            | Screen::Following
            | Screen::Favorites
            | Screen::Player
    );
    if !shows_feed {
        return;
    }
    let id = s.visible_feed_id();
    if matches!(id, FeedId::Search(_)) || s.feed(&id).is_some_and(|f| f.loading) {
        return;
    }
    cmds.push(Command::LoadFeed { feed: id });
}

/// Shows Following as a root screen and refreshes it. Rows from an earlier visit stay on screen
/// until the new ones arrive.
fn open_following(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.back_stack.clear();
    s.category = None;
    s.screen = Screen::Following;
    s.rail.index = RailEntry::Following;
    s.following.loading = true;
    s.following.error = None;
    cmds.push(Command::LoadFeed {
        feed: FeedId::Following,
    });
}

fn player_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    if s.raid.is_some() {
        raid_intent(s, intent, cmds);
        return;
    }
    if s.subscribe_card {
        subscribe_card_intent(s, intent, cmds);
        return;
    }
    if s.compose.open {
        compose_intent(s, intent, cmds);
        return;
    }
    if let Some(row) = s.player_menu {
        player_menu_intent(s, row, intent, cmds);
        return;
    }
    if s.multiview.is_some() && multiview::full_screen_intent(s, intent, cmds) {
        return;
    }
    match intent {
        Intent::View => multiview::player_view(s),
        // Start opens the player menu on its first row.
        Intent::Menu => {
            s.player_menu = Some(PlayerMenuRow::default());
            pulse_hud(s);
        }
        Intent::ToggleChat => {
            s.chat_open = !s.chat_open;
            pulse_hud(s);
        }
        Intent::Back => back(s, cmds),
        // A on a failed or ended stream tries again, on a live one it pauses.
        Intent::Select => match s.playback {
            Playback::Failed(_) | Playback::Offline => start_playback(s, cmds),
            Playback::Playing => toggle_pause(s, cmds),
            Playback::Idle | Playback::Loading => {}
        },
        // X tries a failed or ended stream again. Otherwise, with chat shown, it writes in chat.
        Intent::Retry => {
            if matches!(s.playback, Playback::Failed(_) | Playback::Offline) {
                start_playback(s, cmds);
            } else if s.chat_open {
                open_compose(s);
            }
        }
        // Any other button only brings the HUD back.
        Intent::Up | Intent::Down | Intent::Left | Intent::Right => pulse_hud(s),
        // Typing never reaches the player (see `typed_key`).
        Intent::Pick { .. }
        | Intent::RailPick(_)
        | Intent::Text(_)
        | Intent::Erase
        | Intent::PageLeft
        | Intent::PageRight => {}
    }
}

/// Opens the compose panel (and chat with it: the box is at the bottom of the chat). Signed out
/// there is nothing to open: the chat panel says to sign in. A token without the chat scope
/// opens it too, on the "sign in again" message.
fn open_compose(s: &mut AppState) {
    if !s.is_signed_in() || s.now_playing.is_none() || !s.on_screen_features().chat_send {
        return;
    }
    s.player_menu = None;
    s.compose.open = true;
    s.chat_open = true;
    pulse_hud(s);
}

/// Input while the compose panel is open. The keyboard has the D-pad and A; Up from its top row
/// goes to the quick messages, where A sends one at once. X deletes, Y types a space, Start sends,
/// LB and RB change the page, and B closes the panel (the text stays for later).
///
/// With a token that may not chat, the panel only says so: A signs in again, B closes it.
fn compose_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    if !s.can_chat() {
        match intent {
            Intent::Select => {
                s.compose.open = false;
                open_login(s, Some(Screen::Player), cmds);
            }
            Intent::Back => s.compose.open = false,
            _ => {}
        }
        return;
    }
    if let Some(chip) = s.compose.chip {
        match intent {
            Intent::Left => s.compose.chip = Some(chip.saturating_sub(1)),
            Intent::Right => {
                let last = s.quick_messages.len().saturating_sub(1);
                s.compose.chip = Some((chip + 1).min(last));
            }
            Intent::Down => s.compose.chip = None,
            Intent::Select => {
                if let Some(text) = s.quick_messages.get(chip).cloned() {
                    send_chat(s, text, cmds);
                }
            }
            Intent::Up | Intent::Pick { .. } | Intent::RailPick(_) => {}
            other => compose_common(s, other, cmds),
        }
        return;
    }
    match intent {
        Intent::Up => {
            if !s.compose.keys.up() && !s.quick_messages.is_empty() {
                s.compose.chip = Some(0);
            }
        }
        Intent::Down => {
            s.compose.keys.down();
        }
        Intent::Left => {
            s.compose.keys.left();
        }
        Intent::Right => {
            s.compose.keys.right();
        }
        Intent::Select => press_compose_key(s, cmds),
        Intent::Pick { .. } | Intent::RailPick(_) => {}
        other => compose_common(s, other, cmds),
    }
}

/// The buttons that do the same on the keys and on the quick messages.
fn compose_common(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Retry | Intent::Erase => edit_compose(s, keyboard::backspace),
        Intent::ToggleChat => edit_compose(s, |t| keyboard::chat_type(t, ' ')),
        Intent::Menu => {
            let text = s.compose.text.clone();
            send_chat(s, text, cmds);
        }
        Intent::Back => s.compose.open = false,
        Intent::PageLeft | Intent::PageRight => {
            let board = s.compose.keys.board().cycled(intent == Intent::PageRight);
            s.compose.keys.set_board(board);
        }
        Intent::Text(c) => edit_compose(s, |t| keyboard::chat_type(t, c)),
        Intent::Up
        | Intent::Down
        | Intent::Left
        | Intent::Right
        | Intent::Select
        | Intent::View
        | Intent::Pick { .. }
        | Intent::RailPick(_) => {}
    }
}

/// A on the focused key of the chat keyboard.
fn press_compose_key(s: &mut AppState, cmds: &mut Vec<Command>) {
    match s.compose.keys.key() {
        Key::Char(c) => {
            let shift = s.compose.shift;
            let typed = shift.apply(c);
            if c.is_ascii_alphabetic() {
                s.compose.shift = shift.after_letter();
            }
            edit_compose(s, |t| keyboard::chat_type(t, typed));
        }
        Key::Emote(code) => edit_compose(s, |t| keyboard::chat_type_word(t, code)),
        Key::Space => edit_compose(s, |t| keyboard::chat_type(t, ' ')),
        Key::Backspace => edit_compose(s, keyboard::backspace),
        Key::Clear => edit_compose(s, |t| {
            let had = !t.is_empty();
            t.clear();
            had
        }),
        Key::Shift => s.compose.shift = s.compose.shift.pressed(),
        Key::Page(board) => s.compose.keys.set_board(board),
        Key::Send | Key::Done => {
            let text = s.compose.text.clone();
            send_chat(s, text, cmds);
        }
    }
}

/// Applies `edit` to the message. A change clears the reason the last one was dropped.
fn edit_compose(s: &mut AppState, edit: impl FnOnce(&mut String) -> bool) {
    if edit(&mut s.compose.text) {
        s.compose.problem = None;
    }
}

/// Sends `text` (trimmed) to the chat on screen, unless it is empty or a message is still on its
/// way. The box is cleared once Twitch has posted it (`Event::ChatSent`).
fn send_chat(s: &mut AppState, text: String, cmds: &mut Vec<Command>) {
    let text = text.trim().to_owned();
    if text.is_empty() || s.compose.sending.is_some() {
        return;
    }
    let Some(stream) = &s.now_playing else {
        return;
    };
    s.last_send_id = s.last_send_id.wrapping_add(1);
    let id = s.last_send_id;
    cmds.push(Command::SendChat {
        id,
        channel: stream.channel.clone(),
        broadcaster_id: stream.user_id.clone(),
        text: text.clone(),
    });
    s.compose.sending = Some(Sending { id, text });
    s.compose.problem = None;
}

/// The channel on screen changed or was left: what was being written belongs to the old one.
fn reset_chat_room(s: &mut AppState) {
    s.compose = Compose::default();
    s.room_mode = RoomMode::default();
}

/// Input while the player menu is open. Up and Down pick a row, Left and Right (and A) change its
/// value, B or Start close the menu. Y still toggles chat. Nothing here is saved: only the
/// Settings screen saves.
fn player_menu_intent(
    s: &mut AppState,
    row: PlayerMenuRow,
    intent: Intent,
    cmds: &mut Vec<Command>,
) {
    match intent {
        Intent::Up => s.player_menu = Some(row.prev()),
        Intent::Down => s.player_menu = Some(row.next()),
        Intent::Select if row == PlayerMenuRow::Subscribe => {
            s.subscribe_card = s.on_screen_features().subscribe;
        }
        // Signed out the row only says to sign in.
        Intent::Select if row == PlayerMenuRow::WriteChat => open_compose(s),
        // Action rows have no value to step through.
        Intent::Right | Intent::Left if row.is_action() => {}
        Intent::Right | Intent::Select => change_player_option(s, row, true, cmds),
        Intent::Left => change_player_option(s, row, false, cmds),
        Intent::Back | Intent::Menu => s.player_menu = None,
        Intent::ToggleChat => s.chat_open = !s.chat_open,
        Intent::Retry
        | Intent::View
        | Intent::Pick { .. }
        | Intent::RailPick(_)
        | Intent::Text(_)
        | Intent::Erase
        | Intent::PageLeft
        | Intent::PageRight => {}
    }
    pulse_hud(s);
}

/// Steps a player-menu row to its next or previous value and applies it. Quality and latency are
/// chosen when a stream is resolved and loaded, so changing them (or audio only) starts the
/// stream again.
fn change_player_option(
    s: &mut AppState,
    row: PlayerMenuRow,
    forward: bool,
    cmds: &mut Vec<Command>,
) {
    match row {
        // Audio only has no quality to pick: the row is locked until it is turned off.
        PlayerMenuRow::Quality if s.audio_only => {}
        PlayerMenuRow::Quality => {
            s.session.quality = Some(s.quality().cycled(forward));
            reload_playback(s, cmds);
        }
        PlayerMenuRow::Latency => {
            s.session.latency = s.session.latency.cycled(forward);
            reload_playback(s, cmds);
        }
        PlayerMenuRow::Hwdec => {
            s.session.hwdec = s.session.hwdec.cycled(forward);
            cmds.push(Command::SetPlayerOption(PlayerOption::Hwdec(
                s.session.hwdec,
            )));
        }
        PlayerMenuRow::Zoom => {
            s.session.zoom = s.session.zoom.cycled(forward);
            cmds.push(Command::SetPlayerOption(PlayerOption::Zoom(s.session.zoom)));
        }
        PlayerMenuRow::Stats => {
            s.session.stats = !s.session.stats;
            if !s.session.stats {
                s.stats = None;
            }
            cmds.push(Command::SetStats {
                slot: s.sound_slot(),
                on: s.session.stats,
            });
        }
        // Multiview's tiles are video: their quality comes from their size.
        PlayerMenuRow::AudioOnly if s.multiview.is_some() => {}
        // Both directions toggle. The quality is kept, so turning it off goes back to it.
        PlayerMenuRow::AudioOnly => {
            s.audio_only = !s.audio_only;
            reload_playback(s, cmds);
        }
        // Every step starts the timer again at the new length, counted from now.
        PlayerMenuRow::SleepTimer => {
            match SleepTimer::cycled(s.sleep.map(|t| t.minutes), forward) {
                Some(minutes) => start_sleep(s, minutes, cmds),
                None => {
                    s.sleep = None;
                    cmds.push(Command::CancelSleepTimer);
                }
            }
        }
        // Action rows, handled in `player_menu_intent`.
        PlayerMenuRow::Subscribe | PlayerMenuRow::WriteChat => {}
    }
}

/// Input while the subscribe card is open. A opens the page in a browser where there is one we
/// can show, B (or Start) closes the card and returns to the menu. The stream keeps playing and
/// every other button is ignored.
fn subscribe_card_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Select if s.can_open_browser => {
            if let Some(stream) = &s.now_playing {
                cmds.push(Command::OpenUrl(subscribe_url(&stream.channel.login)));
            }
        }
        Intent::Back | Intent::Menu => s.subscribe_card = false,
        _ => {}
    }
    pulse_hud(s);
}

/// Input while the raid card is open. A follows the raid at once, B stays on this channel. Every
/// other button is ignored, so a random press does not count as a choice.
fn raid_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Select => follow_raid(s, cmds),
        // The countdown may still tick: its id no longer matches anything.
        Intent::Back => s.raid = None,
        _ => {}
    }
}

/// The channel on screen raided another one. Signed in, it opens the raid card (closing the menu
/// and the subscribe card) and starts its countdown. A repeat for the raid already on the card,
/// and anything about another channel, is ignored.
fn raided(
    s: &mut AppState,
    from: String,
    to_login: String,
    to_name: String,
    viewers: u64,
    cmds: &mut Vec<Command>,
) {
    let on_screen = s.is_signed_in()
        && s.stream_on_screen()
        && s.now_playing
            .as_ref()
            .is_some_and(|p| p.channel.is_same(&ChannelRef::twitch(&from)));
    if !on_screen || to_login.is_empty() || to_login.eq_ignore_ascii_case(&from) {
        return;
    }
    if s.raid
        .as_ref()
        .is_some_and(|r| r.to_login.eq_ignore_ascii_case(&to_login))
    {
        return;
    }
    s.last_raid_id = s.last_raid_id.wrapping_add(1);
    let id = s.last_raid_id;
    s.player_menu = None;
    s.subscribe_card = false;
    s.popup = None;
    s.raid = Some(RaidOffer {
        id,
        from,
        to_name: if to_name.is_empty() {
            to_login.clone()
        } else {
            to_name
        },
        to_login,
        viewers,
        seconds_left: RAID_SECONDS,
    });
    cmds.push(Command::StartRaidCountdown {
        id,
        seconds: RAID_SECONDS,
    });
}

/// Goes to the raided channel. The Player screen stays (it is replaced, not stacked, so B returns
/// to the browse screens). Chat moves along, and the player menu's choices, the sleep timer and
/// audio only carry over: following a raid feels like the same session. Title and category are
/// empty until `Event::StreamInfo` brings them, with the user id for the next raid.
///
/// In multiview the raided channel replaces the stream with sound in its tile, on the same player.
fn follow_raid(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(offer) = s.raid.take() else {
        return;
    };
    if !s.stream_on_screen() {
        return;
    }
    let tint = s.now_playing.as_ref().map(|p| p.tint).unwrap_or_default();
    let stream = Stream {
        channel: ChannelRef::twitch(&offer.to_login),
        user_id: String::new(),
        title: String::new(),
        category: String::new(),
        viewers: 0,
        tint,
        thumbnail: None,
    };
    if let Some(mv) = s.multiview.as_mut() {
        let sound = mv.sound;
        if let Some(tile) = mv.tile_mut(sound) {
            tile.stream = stream.clone();
        }
    }
    switch_now_playing(s, stream, cmds);
    start_playback(s, cmds);
    cmds.push(Command::FetchStreamInfo {
        channel: ChannelRef::twitch(offer.to_login),
    });
}

/// The stream with sound becomes `stream`: a raid was followed, or multiview moved the sound.
/// Chat is cleared and joins the new channel ("Connecting…"), raids and the hype train are watched
/// there instead, and what belonged to the old channel (menus, the text being written, its
/// numbers) goes. Playback is the caller's business.
fn switch_now_playing(s: &mut AppState, stream: Stream, cmds: &mut Vec<Command>) {
    s.player_menu = None;
    s.subscribe_card = false;
    s.chat.clear();
    s.chat_status = ChatStatus::Connecting;
    reset_chat_room(s);
    // The old channel's subscription. The new one follows once its user id is known.
    if s.is_signed_in() {
        cmds.push(Command::StopRaids);
    }
    cmds.push(Command::JoinChat {
        channel: stream.channel.clone(),
    });
    // The old channel's train is not this one's.
    s.hype_train = None;
    watch_hype_train(&stream, cmds);
    s.stats = None;
    s.now_playing = Some(stream);
}

/// Signed in, asks for raids of the stream on screen: by its user id, or after looking it up when
/// the card it came from did not have one.
/// Hype trains are Twitch's: a Kick stream stops the watch of the stream before it.
fn watch_hype_train(stream: &Stream, cmds: &mut Vec<Command>) {
    cmds.push(if stream.channel.features().hype_train {
        Command::WatchHypeTrain {
            channel: stream.channel.login.clone(),
        }
    } else {
        Command::StopHypeTrain
    });
}

fn watch_raids(s: &AppState, cmds: &mut Vec<Command>) {
    if !s.is_signed_in() || !s.stream_on_screen() || !s.on_screen_features().raids {
        return;
    }
    let Some(stream) = &s.now_playing else {
        return;
    };
    if stream.user_id.is_empty() {
        cmds.push(Command::FetchStreamInfo {
            channel: stream.channel.clone(),
        });
    } else {
        cmds.push(Command::WatchRaids {
            broadcaster_id: stream.user_id.clone(),
        });
    }
}

/// Fills in what Twitch knows of the stream on screen (and of its multiview tile). A user id that
/// was missing starts the raid watch.
fn stream_info(s: &mut AppState, info: Stream, cmds: &mut Vec<Command>) {
    if !s.stream_on_screen() {
        return;
    }
    let Some(p) = s
        .now_playing
        .as_mut()
        .filter(|p| p.channel.is_same(&info.channel))
    else {
        return;
    };
    let had_id = !p.user_id.is_empty();
    if !info.user_id.is_empty() {
        p.user_id = info.user_id;
    }
    if !info.title.is_empty() {
        p.title = info.title;
    }
    if !info.category.is_empty() {
        p.category = info.category;
    }
    if info.viewers > 0 {
        p.viewers = info.viewers;
    }
    if p.thumbnail.is_none() {
        if let Some(url) = info.thumbnail {
            p.thumbnail = Some(url.clone());
            cmds.push(Command::FetchThumbnails { urls: vec![url] });
        }
    }
    let now = p.clone();
    if let Some(mv) = s.multiview.as_mut() {
        let sound = mv.sound;
        if let Some(tile) = mv.tile_mut(sound) {
            tile.stream = now.clone();
        }
    }
    if !had_id && !now.user_id.is_empty() {
        watch_raids(s, cmds);
    }
}

/// Sets a fresh sleep timer of `minutes` (replacing any running one) and asks the app to run it.
fn start_sleep(s: &mut AppState, minutes: u16, cmds: &mut Vec<Command>) {
    s.last_sleep_id = s.last_sleep_id.wrapping_add(1);
    let id = s.last_sleep_id;
    s.sleep = Some(SleepTimer {
        id,
        minutes,
        left_min: minutes,
        warning: false,
    });
    cmds.push(Command::StartSleepTimer { id, minutes });
}

/// The running sleep timer, if its id is `id`. Events of an older timer find nothing.
fn sleep_timer(s: &mut AppState, id: u32) -> Option<&mut SleepTimer> {
    s.sleep.as_mut().filter(|t| t.id == id)
}

fn pulse_hud(s: &mut AppState) {
    s.hud_pulse = s.hud_pulse.wrapping_add(1);
}

/// Resolves and plays the stream on screen. Shows the loading state until the player reports.
fn start_playback(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.now_playing.is_none() {
        return;
    }
    if s.multiview.is_some() {
        multiview::play_sound(s, cmds);
        pulse_hud(s);
        return;
    }
    s.playback = Playback::Loading;
    s.rechecking = false;
    s.paused = false;
    s.background_paused = false;
    // The numbers of the last stream mean nothing for this one.
    s.stats = None;
    pulse_hud(s);
    push_play(s, cmds);
}

/// The stream on screen again at another quality or latency, or as sound only. While it plays it
/// stays `Playing`: the app loads the new one behind it and swaps at its first frame, so the
/// picture never stops. A stream that does not play starts again as in `start_playback`.
fn reload_playback(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.now_playing.is_none() {
        return;
    }
    if s.multiview.is_some() {
        multiview::reload_sound(s, cmds);
        pulse_hud(s);
        return;
    }
    if s.playback != Playback::Playing || s.rechecking {
        start_playback(s, cmds);
        return;
    }
    s.paused = false;
    s.background_paused = false;
    pulse_hud(s);
    push_play(s, cmds);
}

/// Shows the offline screen and schedules a quiet look for the stream coming back.
fn go_offline(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.playback = Playback::Offline;
    s.rechecking = false;
    s.paused = false;
    s.background_paused = false;
    s.offline_recheck = s.offline_recheck.wrapping_add(1);
    cmds.push(Command::ScheduleOfflineRecheck {
        id: s.offline_recheck,
    });
}

/// A quiet recheck failed one way or another: still offline, look again later.
fn recheck_failed(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.rechecking && s.playback == Playback::Offline {
        go_offline(s, cmds);
    }
}

/// Asks the single player for the stream on screen, as `start_playback` does, but leaves the
/// state alone.
fn push_play(s: &AppState, cmds: &mut Vec<Command>) {
    let Some(channel) = s.now_playing.as_ref().map(|p| p.channel.clone()) else {
        return;
    };
    cmds.push(Command::Play {
        slot: MAIN_SLOT,
        channel,
        stream: s.stream_kind(),
        latency: s.session.latency,
    });
    // Leaving the player stops the reports, so a stream started with the stats box on asks for
    // them again. This is what keeps the box on from stream to stream.
    if s.session.stats {
        cmds.push(Command::SetStats {
            slot: MAIN_SLOT,
            on: true,
        });
    }
}

/// A on the single player's live stream.
fn toggle_pause(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.paused = !s.paused;
    // From here the pause (or the playing) is the user's choice, not the background's.
    s.background_paused = false;
    pulse_hud(s);
    cmds.push(Command::SetPaused {
        slot: MAIN_SLOT,
        paused: s.paused,
    });
}

/// The window gained or lost focus. Without background play, a stream playing when the user
/// switches away is paused, and resumes when they come back, unless they paused or left it.
fn window_focus(s: &mut AppState, focused: bool, cmds: &mut Vec<Command>) {
    s.in_background = !focused;
    if !focused {
        pause_in_background(s, cmds);
        return;
    }
    if std::mem::take(&mut s.background_paused)
        && s.screen == Screen::Player
        && s.multiview.is_none()
        && s.playback == Playback::Playing
        && s.paused
    {
        s.paused = false;
        pulse_hud(s);
        cmds.push(Command::SetPaused {
            slot: MAIN_SLOT,
            paused: false,
        });
    }
}

/// Pauses the playing stream if the app is in the background and the setting says not to play
/// there. Also run when a stream starts, since one can finish loading after the user left.
/// Multiview is never paused this way: its grid never pauses.
fn pause_in_background(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.in_background
        && !s.settings.background_play
        && s.multiview.is_none()
        && s.screen == Screen::Player
        && s.playback == Playback::Playing
        && !s.paused
    {
        s.paused = true;
        s.background_paused = true;
        cmds.push(Command::SetPaused {
            slot: MAIN_SLOT,
            paused: true,
        });
    }
}

/// The current feed is separate rows (Home, Following), where each row keeps its own column.
/// In a grid (Browse, a category) Up and Down keep the column instead.
fn per_row_cols(s: &AppState) -> bool {
    s.current_feed_id().is_some_and(|id| !id.is_grid())
}

fn home_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    // On the Search keyboard Start means "done" (see `keyboard_intent`), not the rail.
    let on_search_keys = s.screen == Screen::Search && !s.rail.focused && !s.search.in_results;
    // Mouse fallbacks and Menu work whether or not the rail has focus.
    match intent {
        Intent::Pick { row, col } => {
            if s.tile(row, col).is_some() {
                let per_row = per_row_cols(s);
                if let Some(feed) = s.current_feed_mut() {
                    feed.focus_row(row, per_row);
                    feed.focus.col = col;
                }
                if s.screen == Screen::Search {
                    s.search.in_results = true;
                }
                select_tile(s, cmds);
            }
            return;
        }
        Intent::RailPick(entry) => {
            activate_rail(s, entry, cmds);
            return;
        }
        Intent::Menu if !on_search_keys => {
            // Menu, Menu: a hidden refresh of the feed on screen. The second Menu closes the
            // sidebar the first one opened (Menu, then Back, still asks to quit).
            let closes_own = s.rail.focused && s.rail.opened_by_menu;
            s.rail.focused = !s.rail.focused;
            s.rail.opened_by_menu = s.rail.focused;
            if closes_own {
                refresh_visible_feed(s, cmds);
            }
            return;
        }
        _ => {}
    }

    if s.rail.focused {
        // Any other press than Menu ends the Menu, Menu gesture.
        s.rail.opened_by_menu = false;
        match intent {
            Intent::Up => s.rail.index = s.rail.index.prev(),
            Intent::Down => s.rail.index = s.rail.index.next(),
            Intent::Select => activate_rail(s, s.rail.index, cmds),
            Intent::Right => s.rail.focused = false,
            // Back on the sidebar asks to quit: the sidebar is where Back from a root screen leads.
            Intent::Back => s.exit_prompt = true,
            _ => {}
        }
        return;
    }
    if intent == Intent::View {
        multiview::browse_view(s);
        return;
    }

    match s.screen {
        Screen::Settings => settings_intent(s, intent, cmds),
        Screen::Search if s.search.in_results => search_results_intent(s, intent, cmds),
        Screen::Search => keyboard_intent(s, intent, cmds),
        _ => feed_intent(s, intent, cmds),
    }
}

/// Input on the Search keyboard. The D-pad moves between keys and A presses one. Left at the left
/// edge opens the rail, Down from the bottom row goes into the results. X deletes, Y types a
/// space, Start jumps to the results. B deletes too, and once the text is empty opens the sidebar.
fn keyboard_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Up => {
            s.search.keys.up();
        }
        Intent::Down => {
            if !s.search.keys.down() {
                enter_results(s);
            }
        }
        Intent::Left => {
            if !s.search.keys.left() {
                s.rail.focused = true;
            }
        }
        Intent::Right => {
            s.search.keys.right();
        }
        Intent::Select => match s.search.keys.key() {
            Key::Char(c) => edit_search(s, cmds, |q| keyboard::type_char(q, c)),
            Key::Space => edit_search(s, cmds, |q| keyboard::type_char(q, ' ')),
            Key::Backspace => edit_search(s, cmds, keyboard::backspace),
            Key::Clear => edit_search(s, cmds, |q| {
                let had = !q.is_empty();
                q.clear();
                had
            }),
            Key::Done => search_done(s, cmds),
            // Chat keys: never on the Search board.
            Key::Shift | Key::Page(_) | Key::Emote(_) | Key::Send => {}
        },
        Intent::Retry => edit_search(s, cmds, keyboard::backspace),
        Intent::ToggleChat => edit_search(s, cmds, |q| keyboard::type_char(q, ' ')),
        Intent::Menu => search_done(s, cmds),
        Intent::Back | Intent::Erase if s.search.query.is_empty() => s.rail.focused = true,
        Intent::Back | Intent::Erase => edit_search(s, cmds, keyboard::backspace),
        Intent::Text(c) => type_on_search(s, c, cmds),
        // Search has one page of keys. View is handled before (`home_intent`).
        Intent::Pick { .. }
        | Intent::RailPick(_)
        | Intent::PageLeft
        | Intent::PageRight
        | Intent::View => {}
    }
}

/// Input on the Search results. They work like Home, except that Up from the first row and B go
/// back to the keyboard, and typing on a physical keyboard edits the text (which leaves the
/// results, as they are about to change).
fn search_results_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Up if s.search.feed.focus.row == 0 => s.search.in_results = false,
        Intent::Back => s.search.in_results = false,
        Intent::Retry if s.search.feed.error.is_some() => resend_search(s, cmds),
        Intent::Text(c) => type_on_search(s, c, cmds),
        Intent::Erase => edit_search(s, cmds, keyboard::backspace),
        Intent::ToggleChat => {}
        _ => feed_intent(s, intent, cmds),
    }
}

/// A character from a physical keyboard. Letters are lowercased like the on-screen keys.
fn type_on_search(s: &mut AppState, c: char, cmds: &mut Vec<Command>) {
    let c = c.to_ascii_lowercase();
    edit_search(s, cmds, |q| keyboard::type_char(q, c));
}

/// Applies `edit` to the search text. If it changed, focus returns to the keyboard and, once the
/// trimmed text differs from the last search, a new search is sent (a short text clears the rows
/// instead, and still tells the app, which drops a search it is waiting to send).
fn edit_search(s: &mut AppState, cmds: &mut Vec<Command>, edit: impl FnOnce(&mut String) -> bool) {
    if !edit(&mut s.search.query) {
        return;
    }
    s.search.in_results = false;
    let asked = s.search.query.trim().to_owned();
    if asked == s.search.asked {
        return;
    }
    s.search.asked = asked;
    if SearchState::is_searchable(&s.search.asked) {
        resend_search(s, cmds);
    } else {
        s.search.id = s.search.id.wrapping_add(1);
        s.search.feed = Feed::default();
        s.search.results_for.clear();
        cmds.push(Command::Search {
            id: s.search.id,
            query: s.search.asked.clone(),
        });
    }
}

/// Sends the current search again under a new id. The rows on screen stay until the answer.
fn resend_search(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.search.id = s.search.id.wrapping_add(1);
    s.search.feed.loading = true;
    s.search.feed.error = None;
    cmds.push(Command::Search {
        id: s.search.id,
        query: s.search.asked.clone(),
    });
}

/// "done" (or Start) on the keyboard: into the results, or try again after a failed search.
fn search_done(s: &mut AppState, cmds: &mut Vec<Command>) {
    let failed = s.search.feed.error.is_some() && !s.search.feed.loading;
    if failed && SearchState::is_searchable(&s.search.asked) {
        resend_search(s, cmds);
    } else {
        enter_results(s);
    }
}

/// Moves focus from the keyboard to the first result row, if there is one.
fn enter_results(s: &mut AppState) {
    if s.search.feed.rows.is_empty() {
        return;
    }
    s.search.in_results = true;
    s.search.feed.focus_row(0, true);
}

/// The D-pad, A and X on a feed of cards (Home, Following, Browse, a category, Search results).
fn feed_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Left => {
            let at_start = s.current_feed().is_none_or(|f| f.focus.col == 0);
            if at_start {
                s.rail.focused = true;
            } else if let Some(feed) = s.current_feed_mut() {
                feed.focus.col -= 1;
            }
        }
        Intent::Right => {
            if let Some(feed) = s.current_feed_mut() {
                if let Some(len) = feed.row_len(feed.focus.row) {
                    feed.focus.col = (feed.focus.col + 1).min(len.saturating_sub(1));
                }
            }
            load_more_if_near_end(s, cmds);
        }
        Intent::Up => {
            let per_row = per_row_cols(s);
            if let Some(feed) = s.current_feed_mut() {
                feed.focus_row(feed.focus.row.saturating_sub(1), per_row);
            }
            load_more_if_near_end(s, cmds);
        }
        Intent::Down => {
            let per_row = per_row_cols(s);
            if let Some(feed) = s.current_feed_mut() {
                if !feed.rows.is_empty() {
                    feed.focus_row((feed.focus.row + 1).min(feed.rows.len() - 1), per_row);
                }
            }
            load_more_if_near_end(s, cmds);
        }
        Intent::Select => select_tile(s, cmds),
        Intent::Retry => retry(s, cmds),
        // Category is the only feed screen above another one, so it is the only one Back leaves.
        // On a root screen there is nowhere further back: B opens the sidebar, and B there asks to quit.
        Intent::Back if s.screen == Screen::Category => back(s, cmds),
        Intent::Back => s.rail.focused = true,
        _ => {}
    }
}

fn on_event(s: &mut AppState, event: Event, cmds: &mut Vec<Command>) {
    match event {
        Event::FeedLoaded { feed, mut rows } => {
            if feed == FeedId::Home {
                resume::home_loaded(s, &mut rows);
            }
            // A category the user already left has no feed any more, so its answer is dropped.
            if let Some(f) = s.feed_mut(&feed) {
                f.rows = rows;
                f.loading = false;
                f.error = None;
                f.clamp_focus();
                fetch_thumbnails(f.rows.iter().flat_map(|r| &r.tiles), cmds);
            }
            if matches!(feed, FeedId::Favorites | FeedId::Home) {
                favorites::prune(s);
            }
        }
        Event::FeedFailed { feed, message } => {
            if let Some(f) = s.feed_mut(&feed) {
                f.loading = false;
                f.error = Some(message);
            }
        }
        Event::FeedRefreshDue => refresh_visible_feed(s, cmds),
        Event::MoreLoaded {
            feed,
            row,
            cursor,
            outcome,
        } => more_loaded(s, feed, row, cursor, outcome, cmds),
        Event::ChatMessages { channel, messages } => {
            // Drop late messages from a channel we already left.
            if watching(s, &channel) && !messages.is_empty() {
                fetch_chat_images(&messages, cmds);
                // Lines only arrive over a working connection.
                s.chat_status = ChatStatus::Live;
                s.chat.extend(messages);
            }
        }
        Event::ChatModerated { channel, action } => {
            if watching(s, &channel) {
                s.chat.moderate(&action);
            }
        }
        Event::ChatStatus { channel, status } => {
            if watching(s, &channel) {
                s.chat_status = status;
            }
        }
        // In multiview a resolver answer belongs to the tile showing its channel (two tiles never
        // show the same one), whichever has sound.
        Event::StreamOffline { channel } if s.multiview.is_some() => {
            multiview::tile_offline(s, &channel);
        }
        Event::PlaybackFailed { channel, message } if s.multiview.is_some() => {
            multiview::tile_failed(s, &channel, message);
        }
        // And a player's report to the tile playing on its slot.
        Event::PlaybackStarted { slot } if s.multiview.is_some() => {
            multiview::tile_started(s, slot);
        }
        Event::PlaybackEnded { slot } if s.multiview.is_some() => {
            multiview::tile_ended(s, slot);
        }
        Event::PlaybackError { slot, message } if s.multiview.is_some() => {
            multiview::tile_error(s, slot, message);
        }
        Event::QualitySettleDue(id) => multiview::settle_due(s, id, cmds),
        Event::PlayerHealth {
            slot,
            at_ms,
            health,
        } => network::health(s, slot, at_ms, health, cmds),
        // Without multiview only the single player's slot has a stream on screen.
        Event::PlaybackStarted { slot }
        | Event::PlaybackEnded { slot }
        | Event::PlaybackError { slot, .. }
            if slot != MAIN_SLOT => {}
        Event::StreamOffline { channel } => {
            if watching(s, &channel) {
                if s.playback == Playback::Loading {
                    go_offline(s, cmds);
                } else {
                    recheck_failed(s, cmds);
                }
            }
        }
        Event::PlaybackFailed { channel, message } => {
            if watching(s, &channel) {
                if s.playback == Playback::Loading {
                    s.playback = Playback::Failed(message);
                } else {
                    // A blip while looking for the stream again is not worth an error screen.
                    recheck_failed(s, cmds);
                }
            }
        }
        // The player's own events carry no channel. They are only trusted while a stream is on
        // screen, in the state that can produce them.
        Event::PlaybackStarted { .. } => {
            let rechecked = s.rechecking && s.playback == Playback::Offline;
            if s.screen == Screen::Player && (s.playback == Playback::Loading || rechecked) {
                s.playback = Playback::Playing;
                s.rechecking = false;
                pulse_hud(s);
                pause_in_background(s, cmds);
            }
        }
        Event::PlaybackEnded { .. } => {
            if s.screen == Screen::Player && s.playback == Playback::Playing {
                go_offline(s, cmds);
            }
        }
        Event::PlaybackError { message, .. } => {
            let active = matches!(s.playback, Playback::Loading | Playback::Playing);
            if s.screen == Screen::Player && active {
                s.playback = Playback::Failed(message);
                s.paused = false;
                s.background_paused = false;
            } else if s.screen == Screen::Player {
                recheck_failed(s, cmds);
            }
        }
        Event::WindowFocus(focused) => window_focus(s, focused, cmds),
        Event::DisplaySize { w, h } => {
            if w > 0 && h > 0 && s.display != (w, h) {
                s.display = (w, h);
                // Every tile's size in pixels changed.
                multiview::schedule_settle(s, cmds);
            }
        }
        Event::OfflineRecheckDue(id) => {
            let due = id == s.offline_recheck && !s.rechecking;
            let single = s.screen == Screen::Player && s.multiview.is_none();
            if due && single && s.playback == Playback::Offline {
                s.rechecking = true;
                push_play(s, cmds);
            }
        }
        // Like the other player events, only trusted while a stream is on screen, and only while
        // the stats box is on (a report can still be on its way after it was turned off).
        Event::PlayerStats(stats) => {
            if s.screen == Screen::Player && s.session.stats {
                s.stats = Some(stats);
            }
        }
        Event::DeviceCodeIssued(code) => {
            if s.screen == Screen::Login {
                s.login = Some(code);
                s.login_problem = None;
            }
        }
        Event::LoginFailed(problem) => {
            // A failure that arrives after the user left the screen is of no interest.
            if s.screen == Screen::Login {
                s.login = None;
                s.login_problem = Some(problem);
            }
        }
        Event::SignedIn(user) => {
            s.user = Some(user);
            // Home gains the Followed row. The Following tab loads when it is opened.
            reload_home(s, cmds);
            s.following = Feed::default();
            s.favorites_feed = Feed::default();
            if s.wants_live_watch() {
                cmds.push(Command::StartLiveWatch);
            }
            // A stream already on screen is watched for raids from now on.
            watch_raids(s, cmds);
            if s.screen == Screen::Login {
                // Polling already finished, so no CancelLogin is needed.
                s.login = None;
                s.login_problem = None;
                match s.after_login.take() {
                    Some(Screen::Following) => open_following(s, cmds),
                    Some(Screen::Favorites) => favorites::open(s, cmds),
                    Some(Screen::Search) => open_root(s, Screen::Search, cmds),
                    // Signed in again for chat: back on the stream, ready to write.
                    Some(Screen::Player) => {
                        pop_screen(s);
                        if s.screen == Screen::Player && s.can_chat() {
                            open_compose(s);
                        }
                    }
                    _ => pop_screen(s),
                }
            }
        }
        Event::SignedOut => {
            s.user = None;
            s.following = Feed::default();
            // The favorites stay (they are LiveDeck's own), but are only shown signed in.
            s.favorites_feed = Feed::default();
            // Live notifications are about the account's follows.
            s.toasts.clear();
            cmds.push(Command::StopLiveWatch);
            // Raids need the account's token.
            s.raid = None;
            cmds.push(Command::DisconnectRaids);
            // So does writing in chat. The room's rules still apply to what is on screen.
            s.compose = Compose::default();
            // Multiview is for signed-in users.
            s.picked.clear();
            // The results came from the account. The id carries on, so a late answer to an
            // old search can never pass for a new one.
            s.search = SearchState {
                id: s.search.id,
                ..SearchState::default()
            };
            reload_home(s, cmds);
            // Following and Search only exist while signed in. Anything that would lead back
            // there goes Home instead.
            for screen in &mut s.back_stack {
                if screen.needs_account() {
                    *screen = Screen::Home;
                }
            }
            if s.screen.needs_account() {
                s.screen = Screen::Home;
                s.back_stack.clear();
                s.rail.index = RailEntry::Home;
            }
        }
        Event::SearchResults { id, rows } => {
            if id == s.search.id && SearchState::is_searchable(&s.search.asked) {
                let search = &mut s.search;
                search.results_for = search.asked.clone();
                // New results start at their first card.
                search.feed = Feed {
                    rows,
                    ..Feed::default()
                };
                if search.in_results && search.feed.rows.is_empty() {
                    search.in_results = false;
                }
                fetch_thumbnails(search.feed.rows.iter().flat_map(|r| &r.tiles), cmds);
            }
        }
        Event::SearchFailed { id, message } => {
            if id == s.search.id {
                s.search.feed.loading = false;
                s.search.feed.error = Some(message);
            }
        }
        Event::Raided {
            from_login,
            to_login,
            to_name,
            viewers,
        } => raided(s, from_login, to_login, to_name, viewers, cmds),
        Event::RaidTick { id, left } => {
            if let Some(offer) = s.raid.as_mut().filter(|r| r.id == id) {
                offer.seconds_left = left;
            }
        }
        Event::RaidCountdownDone { id } => {
            if s.raid.as_ref().is_some_and(|r| r.id == id) {
                follow_raid(s, cmds);
            }
        }
        Event::StreamInfo(info) => stream_info(s, info, cmds),
        Event::HypeTrain { channel, train } => {
            let on_screen = s.stream_on_screen()
                && s.now_playing
                    .as_ref()
                    .is_some_and(|p| p.channel.is_same(&ChannelRef::twitch(&channel)));
            if on_screen {
                s.hype_train = train;
            }
        }
        Event::ChatSent { id } => {
            if let Some(sent) = s.compose.sending.take_if(|m| m.id == id) {
                // Words typed (or a quick message sent) meanwhile are not thrown away.
                if s.compose.text.trim() == sent.text {
                    s.compose.text.clear();
                    s.compose.shift = keyboard::Shift::Off;
                }
            }
        }
        Event::ChatDropped { id, problem } => {
            if s.compose.sending.take_if(|m| m.id == id).is_some() {
                // The text stays, to try again once the reason has passed.
                s.compose.problem = Some(problem);
            }
        }
        Event::ChatRoomMode { channel, change } => {
            if watching(s, &channel) {
                s.room_mode.apply(change);
            }
        }
        Event::WentLive(streams) => went_live(s, streams, cmds),
        Event::ToastExpired(id) => s.toasts.retain(|t| t.id != id),
        Event::SleepTimerTick { id, left_min } => {
            if let Some(timer) = sleep_timer(s, id) {
                timer.left_min = left_min;
            }
        }
        Event::SleepTimerWarning { id } => {
            if let Some(timer) = sleep_timer(s, id) {
                timer.left_min = 0;
                timer.warning = true;
            }
        }
        Event::SleepTimerFired { id } => {
            if sleep_timer(s, id).is_some() {
                s.sleep = None;
                // Like pressing B: the stream stops and chat is left, so nothing reconnects in
                // the background and the browse screens are there on wake. With nothing playing
                // it still suspends: the user asked for sleep.
                if s.multiview.is_some() {
                    multiview::leave(s, cmds);
                } else if s.screen == Screen::Player {
                    back(s, cmds);
                }
                cmds.push(Command::Suspend);
            }
        }
    }
}

/// Adds a toast for each channel that went live, newest first, and asks the app to take each
/// away again later. The channel on screen is never announced, nor one already on a toast. A
/// late report after sign-out or with the setting off is dropped.
fn went_live(s: &mut AppState, streams: Vec<Stream>, cmds: &mut Vec<Command>) {
    if !s.wants_live_watch() {
        return;
    }
    let mut fresh = Vec::new();
    for stream in streams {
        let on_screen = s.stream_on_screen()
            && s.now_playing
                .as_ref()
                .is_some_and(|p| p.channel.is_same(&stream.channel));
        let shown = s
            .toasts
            .iter()
            .filter_map(Toast::stream)
            .any(|t| t.channel.is_same(&stream.channel));
        if on_screen || shown {
            continue;
        }
        fresh.push(Tile::Stream(stream.clone()));
        push_toast(s, ToastKind::Live(stream), cmds);
    }
    fetch_thumbnails(fresh.iter(), cmds);
}

/// Shows a toast, newest first, and asks the app to take it away again later.
fn push_toast(s: &mut AppState, kind: ToastKind, cmds: &mut Vec<Command>) {
    s.last_toast_id = s.last_toast_id.wrapping_add(1);
    let id = s.last_toast_id;
    cmds.push(Command::ExpireToast { id });
    s.toasts.insert(0, Toast { id, kind });
    s.toasts.truncate(TOASTS_KEPT);
}

/// True while `channel` is the stream on screen (with sound, in multiview). Late answers for a
/// channel already left fail it.
fn watching(s: &AppState, channel: &ChannelRef) -> bool {
    s.stream_on_screen()
        && s.now_playing
            .as_ref()
            .is_some_and(|p| p.channel == *channel)
}

/// Applies the answer to a `LoadMore`.
fn more_loaded(
    s: &mut AppState,
    feed: FeedId,
    row: RowId,
    cursor: String,
    outcome: PageOutcome,
    cmds: &mut Vec<Command>,
) {
    let grid = feed.is_grid();
    let Some(f) = s.feed_mut(&feed) else {
        return;
    };
    // On a grid the paging state lives on the last row, on Home on the row with this id.
    let target = if grid {
        f.rows.len().checked_sub(1)
    } else {
        f.rows.iter().position(|r| r.id() == row)
    };
    let Some(idx) = target else {
        return;
    };
    // Ignore answers for a page the row no longer waits for (a reload replaced it).
    if f.rows[idx].id() != row || f.rows[idx].paging != Paging::Loading(cursor.clone()) {
        return;
    }
    let kind = row.kind;
    match outcome {
        PageOutcome::End => f.rows[idx].paging = Paging::End,
        PageOutcome::Failed => f.rows[idx].paging = Paging::Failed(cursor),
        PageOutcome::Page { tiles, next } => {
            // A source that hands back the cursor it was given would loop forever.
            let paging = match next {
                Some(next) if next != cursor => Paging::More(next),
                _ => Paging::End,
            };
            if grid {
                let mut all: Vec<Tile> = f.rows.drain(..).flat_map(|r| r.tiles).collect();
                let fresh = without_known(&all, tiles);
                fetch_thumbnails(fresh.iter(), cmds);
                all.extend(fresh);
                f.rows = grid_rows_on(row.platform, kind, all, paging);
            } else {
                let row = &mut f.rows[idx];
                let fresh = without_known(&row.tiles, tiles);
                fetch_thumbnails(fresh.iter(), cmds);
                row.tiles.extend(fresh);
                row.paging = paging;
            }
        }
    }
}

/// `tiles` minus the ones whose key is already in `known` (or repeated within `tiles`). Pages are
/// requested at different moments, so an item can show up twice as the ranking shifts.
fn without_known(known: &[Tile], tiles: Vec<Tile>) -> Vec<Tile> {
    let owned = |t: &Tile| {
        let (platform, key) = t.key();
        (platform, key.to_owned())
    };
    let mut seen: HashSet<(Platform, String)> = known.iter().map(owned).collect();
    tiles
        .into_iter()
        .filter(|t| seen.insert(owned(t)))
        .collect()
}

/// Asks for the badge and emote images of new chat lines, each URL once per batch. The app
/// remembers what it already fetched, so a popular emote is not downloaded for every line.
fn fetch_chat_images(lines: &[ChatMessage], cmds: &mut Vec<Command>) {
    let mut seen = HashSet::new();
    let urls: Vec<String> = lines
        .iter()
        .flat_map(ChatMessage::image_urls)
        .filter(|u| seen.insert(*u))
        .map(str::to_owned)
        .collect();
    if !urls.is_empty() {
        cmds.push(Command::FetchChatImages { urls });
    }
}

/// Asks for the thumbnails of `tiles`, each URL once, in order (the hero and first row first).
fn fetch_thumbnails<'a>(tiles: impl Iterator<Item = &'a Tile>, cmds: &mut Vec<Command>) {
    let mut seen = HashSet::new();
    let urls: Vec<String> = tiles
        .flat_map(Tile::thumbnails)
        .filter(|u| seen.insert(*u))
        .map(str::to_owned)
        .collect();
    if !urls.is_empty() {
        cmds.push(Command::FetchThumbnails { urls });
    }
}

/// Starts the next page of the focused row (Home) or of the whole list (grids) if focus is close
/// to its end and a page is waiting.
fn load_more_if_near_end(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(id) = s.current_feed_id() else {
        return;
    };
    let grid = id.is_grid();
    let Some(feed) = s.feed_mut(&id) else {
        return;
    };
    let (target, near) = if grid {
        let last = feed.rows.len().saturating_sub(1);
        (last, feed.focus.row + PREFETCH_ROWS >= last)
    } else {
        let len = feed.row_len(feed.focus.row).unwrap_or(0);
        (
            feed.focus.row,
            feed.focus.col + 1 + PREFETCH_DISTANCE >= len,
        )
    };
    if !near {
        return;
    }
    let Some(row) = feed.rows.get_mut(target) else {
        return;
    };
    if let Paging::More(cursor) = row.paging.clone() {
        row.paging = Paging::Loading(cursor.clone());
        cmds.push(Command::LoadMore {
            feed: id,
            row: row.id(),
            cursor,
        });
    }
}

/// X / `r`: reload a feed that failed to load, otherwise retry the pages that failed.
fn retry(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(id) = s.current_feed_id() else {
        return;
    };
    let Some(feed) = s.feed_mut(&id) else {
        return;
    };
    if feed.error.is_some() {
        if !feed.loading {
            feed.error = None;
            feed.loading = true;
            cmds.push(Command::LoadFeed { feed: id });
        }
        return;
    }
    for row in &mut feed.rows {
        if let Paging::Failed(cursor) = row.paging.clone() {
            row.paging = Paging::Loading(cursor.clone());
            cmds.push(Command::LoadMore {
                feed: id.clone(),
                row: row.id(),
                cursor,
            });
        }
    }
}

/// A / Enter on the focused card.
fn select_tile(s: &mut AppState, cmds: &mut Vec<Command>) {
    match s.focused_tile().cloned() {
        Some(Tile::Stream(stream)) => open_player(s, stream, cmds),
        // A followed channel that is offline. Opening it asks the resolver, which is the
        // authority (the list can be a minute old), and shows the end screen if it is still off.
        // Its chat still works, as on Twitch.
        Some(Tile::Channel(channel)) => {
            let stream = Stream {
                channel: channel.channel,
                user_id: String::new(),
                title: String::new(),
                category: String::new(),
                viewers: 0,
                tint: channel.tint,
                thumbnail: None,
            };
            open_player(s, stream, cmds);
        }
        Some(Tile::Category(category)) => {
            s.rail.focused = false;
            let feed = FeedId::Category(category.id.clone());
            s.category = Some(CategoryFeed {
                id: category.id,
                name: category.name,
                feed: Feed::loading(),
            });
            push_screen(s, Screen::Category);
            cmds.push(Command::LoadFeed { feed });
        }
        Some(Tile::Resume(card)) => multiview::resume(s, card, cmds),
        None => {}
    }
}

/// Opens the Player on `stream`: joins its chat (connected whether or not chat is shown) and
/// starts playback.
fn open_player(s: &mut AppState, stream: Stream, cmds: &mut Vec<Command>) {
    s.rail.focused = false;
    s.chat.clear();
    s.chat_status = ChatStatus::Connecting;
    reset_chat_room(s);
    cmds.push(Command::JoinChat {
        channel: stream.channel.clone(),
    });
    s.hype_train = None;
    watch_hype_train(&stream, cmds);
    s.now_playing = Some(stream);
    // Every stream opens as video.
    s.audio_only = false;
    push_screen(s, Screen::Player);
    start_playback(s, cmds);
    watch_raids(s, cmds);
}

/// Where a rail entry leads. Following, Favorites and Search while signed out go to Login (and on
/// to the screen once the login succeeds). The account entry is Login while signed out and
/// Settings, where the account can be managed, while signed in.
fn rail_target(s: &AppState, entry: RailEntry) -> Screen {
    match entry {
        RailEntry::Account if s.is_signed_in() => Screen::Settings,
        RailEntry::Account => Screen::Login,
        RailEntry::Following | RailEntry::Favorites | RailEntry::Search if !s.is_signed_in() => {
            Screen::Login
        }
        RailEntry::Following => Screen::Following,
        RailEntry::Favorites => Screen::Favorites,
        RailEntry::Search => Screen::Search,
        RailEntry::Browse => Screen::Browse,
        RailEntry::Settings => Screen::Settings,
        RailEntry::Home => Screen::Home,
    }
}

fn activate_rail(s: &mut AppState, entry: RailEntry, cmds: &mut Vec<Command>) {
    s.rail.focused = false;
    let target = rail_target(s, entry);
    if target == s.screen {
        return;
    }
    if s.screen == Screen::Login {
        leave_login(s, cmds);
    }
    if target == Screen::Login {
        // Login sits on top of the current screen, so Back returns there.
        let then = match entry {
            RailEntry::Following => Some(Screen::Following),
            RailEntry::Favorites => Some(Screen::Favorites),
            RailEntry::Search => Some(Screen::Search),
            _ => None,
        };
        open_login(s, then, cmds);
    } else if target == Screen::Following {
        open_following(s, cmds);
    } else if target == Screen::Favorites {
        favorites::open(s, cmds);
    } else {
        open_root(s, target, cmds);
    }
}

/// Shows a root screen. Root screens reset the back stack and drop any open category. Search
/// keeps its text and results from the last visit.
fn open_root(s: &mut AppState, target: Screen, cmds: &mut Vec<Command>) {
    s.back_stack.clear();
    s.category = None;
    s.screen = target;
    // The directory loads the first time it is opened. After that, Retry reloads it.
    if target == Screen::Browse && s.browse.rows.is_empty() && !s.browse.loading {
        s.browse = Feed::loading();
        cmds.push(Command::LoadFeed {
            feed: FeedId::Browse,
        });
    }
}

/// B / Escape on Player, Login or Category. Like the scaffold, it always closes chat.
fn back(s: &mut AppState, cmds: &mut Vec<Command>) {
    match s.screen {
        Screen::Player => {
            cmds.push(Command::StopPlayback { slot: MAIN_SLOT });
            if s.session.stats {
                cmds.push(Command::SetStats {
                    slot: MAIN_SLOT,
                    on: false,
                });
            }
            stop_watching(s, cmds);
        }
        Screen::Login => leave_login(s, cmds),
        Screen::Category => s.category = None,
        _ => {}
    }
    s.chat_open = false;
    pop_screen(s);
}

/// Forgets the stream on screen and everything that hangs off it (chat, raids, hype train, the
/// player's overlays). The players themselves are the caller's to stop.
fn stop_watching(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.now_playing = None;
    // Audio only belongs to the stream that was left, not to the next one.
    s.audio_only = false;
    s.playback = Playback::Idle;
    s.rechecking = false;
    s.paused = false;
    s.background_paused = false;
    s.chat.clear();
    s.chat_status = ChatStatus::Idle;
    reset_chat_room(s);
    // The menu's choices stay for the next stream. Its numbers do not.
    s.player_menu = None;
    s.subscribe_card = false;
    s.raid = None;
    s.stats = None;
    s.hype_train = None;
    cmds.push(Command::LeaveChat);
    cmds.push(Command::StopHypeTrain);
    if s.is_signed_in() {
        cmds.push(Command::StopRaids);
    }
}

fn push_screen(s: &mut AppState, screen: Screen) {
    s.back_stack.push(s.screen);
    s.screen = screen;
}

/// Returns to the previous screen, or Home if the stack is empty.
fn pop_screen(s: &mut AppState) {
    s.screen = s.back_stack.pop().unwrap_or(Screen::Home);
}
