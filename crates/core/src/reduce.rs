//! The pure reducer: `(state, msg) -> (new state, commands)`. No IO, no clocks, no randomness.
//!
//! All navigation behaviour lives here, not in the Slint files: the back stack, paged feeds (Home,
//! Browse, Category) and the commands needed to fetch data.

use std::collections::HashSet;

use crate::domain::{grid_rows, ChatMessage, ChatStatus, FeedId, Paging, RowKind, Stream, Tile};
use crate::keyboard::{self, Key};
use crate::msg::{Command, Event, Intent, Msg, PageOutcome, PlayerOption};
use crate::state::{
    subscribe_url, AppState, CategoryFeed, Feed, Playback, PlayerMenuRow, RailEntry, Screen,
    SearchState, SettingsRow, SleepTimer, Toast, TOASTS_KEPT,
};

/// A horizontal row loads its next page once focus is within this many tiles of its end.
pub const PREFETCH_DISTANCE: usize = 3;

/// A grid loads its next page once focus is within this many rows of its last row.
pub const PREFETCH_ROWS: usize = 1;

/// Initial state plus the commands to run at startup.
pub fn init() -> (AppState, Vec<Command>) {
    let state = AppState {
        home: Feed::loading(),
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
    match s.screen {
        Screen::Login => login_intent(s, intent, cmds),
        Screen::Player => player_intent(s, intent, cmds),
        // Settings has no cards, but shares the rail handling and the Left-opens-the-rail rule.
        Screen::Home
        | Screen::Browse
        | Screen::Category
        | Screen::Following
        | Screen::Search
        | Screen::Settings => home_intent(s, intent, cmds),
    }
}

/// What a physical keyboard's typing means here. Only the Search keyboard takes text (while it
/// has focus and nothing is in front of it). Everywhere else `y` and `r` stay the chat and retry
/// shortcuts, Backspace is Back, and other characters do nothing.
fn typed_key(s: &AppState, intent: Intent) -> Option<Intent> {
    let typing = s.screen == Screen::Search
        && !s.rail.focused
        && !s.exit_prompt
        && !s.sleep.is_some_and(|t| t.warning);
    match intent {
        Intent::Text(_) | Intent::Erase if typing => Some(intent),
        Intent::Text('y' | 'Y') => Some(Intent::ToggleChat),
        Intent::Text('r' | 'R') => Some(Intent::Retry),
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
/// rail (the choices use Left for "previous"). B asks to quit, as on every root screen.
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
        Intent::Back => s.exit_prompt = true,
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
    if s.subscribe_card {
        subscribe_card_intent(s, intent, cmds);
        return;
    }
    if let Some(row) = s.player_menu {
        player_menu_intent(s, row, intent, cmds);
        return;
    }
    match intent {
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
        Intent::Retry => {
            if matches!(s.playback, Playback::Failed(_) | Playback::Offline) {
                start_playback(s, cmds);
            }
        }
        // Any other button only brings the HUD back.
        Intent::Up | Intent::Down | Intent::Left | Intent::Right => pulse_hud(s),
        // Typing never reaches the player (see `typed_key`).
        Intent::Pick { .. } | Intent::RailPick(_) | Intent::Text(_) | Intent::Erase => {}
    }
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
        Intent::Select if row == PlayerMenuRow::Subscribe => s.subscribe_card = true,
        // Action rows have no value to step through.
        Intent::Right | Intent::Left if row.is_action() => {}
        Intent::Right | Intent::Select => change_player_option(s, row, true, cmds),
        Intent::Left => change_player_option(s, row, false, cmds),
        Intent::Back | Intent::Menu => s.player_menu = None,
        Intent::ToggleChat => s.chat_open = !s.chat_open,
        Intent::Retry
        | Intent::Pick { .. }
        | Intent::RailPick(_)
        | Intent::Text(_)
        | Intent::Erase => {}
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
            start_playback(s, cmds);
        }
        PlayerMenuRow::Latency => {
            s.session.latency = s.session.latency.cycled(forward);
            start_playback(s, cmds);
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
            cmds.push(Command::SetStats(s.session.stats));
        }
        // Both directions toggle. The quality is kept, so turning it off goes back to it.
        PlayerMenuRow::AudioOnly => {
            s.audio_only = !s.audio_only;
            start_playback(s, cmds);
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
        // An action row, handled in `player_menu_intent`.
        PlayerMenuRow::Subscribe => {}
    }
}

/// Input while the subscribe card is open. A opens the page in a browser where there is one we
/// can show, B (or Start) closes the card and returns to the menu. The stream keeps playing and
/// every other button is ignored.
fn subscribe_card_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    match intent {
        Intent::Select if s.can_open_browser => {
            if let Some(stream) = &s.now_playing {
                cmds.push(Command::OpenUrl(subscribe_url(&stream.channel)));
            }
        }
        Intent::Back | Intent::Menu => s.subscribe_card = false,
        _ => {}
    }
    pulse_hud(s);
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
    let Some(channel) = s.now_playing.as_ref().map(|p| p.channel.clone()) else {
        return;
    };
    s.playback = Playback::Loading;
    s.paused = false;
    // The numbers of the last stream mean nothing for this one.
    s.stats = None;
    pulse_hud(s);
    cmds.push(Command::Play {
        channel,
        stream: s.stream_kind(),
        latency: s.session.latency,
    });
    // Leaving the player stops the reports, so a stream started with the stats box on asks for
    // them again. This is what keeps the box on from stream to stream.
    if s.session.stats {
        cmds.push(Command::SetStats(true));
    }
}

fn toggle_pause(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.paused = !s.paused;
    pulse_hud(s);
    cmds.push(Command::SetPaused(s.paused));
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
            s.rail.focused = !s.rail.focused;
            return;
        }
        _ => {}
    }

    if s.rail.focused {
        match intent {
            Intent::Up => s.rail.index = s.rail.index.prev(),
            Intent::Down => s.rail.index = s.rail.index.next(),
            Intent::Select => activate_rail(s, s.rail.index, cmds),
            Intent::Right | Intent::Back => s.rail.focused = false,
            _ => {}
        }
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
/// space, Start jumps to the results. B deletes too, and only asks to quit once the text is empty.
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
        },
        Intent::Retry => edit_search(s, cmds, keyboard::backspace),
        Intent::ToggleChat => edit_search(s, cmds, |q| keyboard::type_char(q, ' ')),
        Intent::Menu => search_done(s, cmds),
        Intent::Back | Intent::Erase if s.search.query.is_empty() => s.exit_prompt = true,
        Intent::Back | Intent::Erase => edit_search(s, cmds, keyboard::backspace),
        Intent::Text(c) => type_on_search(s, c, cmds),
        Intent::Pick { .. } | Intent::RailPick(_) => {}
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
        // On a root screen there is nowhere further back: B asks to quit.
        Intent::Back if s.screen == Screen::Category => back(s, cmds),
        Intent::Back => s.exit_prompt = true,
        _ => {}
    }
}

fn on_event(s: &mut AppState, event: Event, cmds: &mut Vec<Command>) {
    match event {
        Event::FeedLoaded { feed, rows } => {
            // A category the user already left has no feed any more, so its answer is dropped.
            if let Some(f) = s.feed_mut(&feed) {
                f.rows = rows;
                f.loading = false;
                f.error = None;
                f.clamp_focus();
                fetch_thumbnails(f.rows.iter().flat_map(|r| &r.tiles), cmds);
            }
        }
        Event::FeedFailed { feed, message } => {
            if let Some(f) = s.feed_mut(&feed) {
                f.loading = false;
                f.error = Some(message);
            }
        }
        Event::MoreLoaded {
            feed,
            kind,
            cursor,
            outcome,
        } => more_loaded(s, feed, kind, cursor, outcome, cmds),
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
        Event::StreamOffline { channel } => {
            if watching(s, &channel) && s.playback == Playback::Loading {
                s.playback = Playback::Offline;
            }
        }
        Event::PlaybackFailed { channel, message } => {
            if watching(s, &channel) && s.playback == Playback::Loading {
                s.playback = Playback::Failed(message);
            }
        }
        // The player's own events carry no channel. They are only trusted while a stream is on
        // screen, in the state that can produce them.
        Event::PlaybackStarted => {
            if s.screen == Screen::Player && s.playback == Playback::Loading {
                s.playback = Playback::Playing;
                pulse_hud(s);
            }
        }
        Event::PlaybackEnded => {
            if s.screen == Screen::Player && s.playback == Playback::Playing {
                s.playback = Playback::Offline;
                s.paused = false;
            }
        }
        Event::PlaybackError(message) => {
            let active = matches!(s.playback, Playback::Loading | Playback::Playing);
            if s.screen == Screen::Player && active {
                s.playback = Playback::Failed(message);
                s.paused = false;
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
            if s.wants_live_watch() {
                cmds.push(Command::StartLiveWatch);
            }
            if s.screen == Screen::Login {
                // Polling already finished, so no CancelLogin is needed.
                s.login = None;
                s.login_problem = None;
                match s.after_login.take() {
                    Some(Screen::Following) => open_following(s, cmds),
                    Some(Screen::Search) => open_root(s, Screen::Search, cmds),
                    _ => pop_screen(s),
                }
            }
        }
        Event::SignedOut => {
            s.user = None;
            s.following = Feed::default();
            // Live notifications are about the account's follows.
            s.toasts.clear();
            cmds.push(Command::StopLiveWatch);
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
                if s.screen == Screen::Player {
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
        let on_screen = s.screen == Screen::Player
            && s.now_playing
                .as_ref()
                .is_some_and(|p| p.channel.eq_ignore_ascii_case(&stream.channel));
        let shown = s
            .toasts
            .iter()
            .any(|t| t.stream.channel.eq_ignore_ascii_case(&stream.channel));
        if on_screen || shown {
            continue;
        }
        s.last_toast_id = s.last_toast_id.wrapping_add(1);
        let id = s.last_toast_id;
        cmds.push(Command::ExpireToast { id });
        fresh.push(Tile::Stream(stream.clone()));
        s.toasts.insert(0, Toast { id, stream });
    }
    s.toasts.truncate(TOASTS_KEPT);
    fetch_thumbnails(fresh.iter(), cmds);
}

/// True while the Player screen shows `channel`. Late answers for a channel already left fail it.
fn watching(s: &AppState, channel: &str) -> bool {
    s.screen == Screen::Player && s.now_playing.as_ref().is_some_and(|p| p.channel == channel)
}

/// Applies the answer to a `LoadMore`.
fn more_loaded(
    s: &mut AppState,
    feed: FeedId,
    kind: RowKind,
    cursor: String,
    outcome: PageOutcome,
    cmds: &mut Vec<Command>,
) {
    let grid = feed.is_grid();
    let Some(f) = s.feed_mut(&feed) else {
        return;
    };
    // On a grid the paging state lives on the last row, on Home on the row of this kind.
    let target = if grid {
        f.rows.len().checked_sub(1)
    } else {
        f.rows.iter().position(|r| r.kind == kind)
    };
    let Some(idx) = target else {
        return;
    };
    // Ignore answers for a page the row no longer waits for (a reload replaced it).
    if f.rows[idx].kind != kind || f.rows[idx].paging != Paging::Loading(cursor.clone()) {
        return;
    }
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
                f.rows = grid_rows(kind, all, paging);
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
    let mut seen: HashSet<String> = known.iter().map(|t| t.key().to_owned()).collect();
    tiles
        .into_iter()
        .filter(|t| seen.insert(t.key().to_owned()))
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
        .filter_map(Tile::thumbnail)
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
            kind: row.kind,
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
                kind: row.kind,
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
        None => {}
    }
}

/// Opens the Player on `stream`: joins its chat (connected whether or not chat is shown) and
/// starts playback.
fn open_player(s: &mut AppState, stream: Stream, cmds: &mut Vec<Command>) {
    s.rail.focused = false;
    s.chat.clear();
    s.chat_status = ChatStatus::Connecting;
    cmds.push(Command::JoinChat {
        channel: stream.channel.clone(),
    });
    s.now_playing = Some(stream);
    // Every stream opens as video.
    s.audio_only = false;
    push_screen(s, Screen::Player);
    start_playback(s, cmds);
}

/// Where a rail entry leads. Following and Search while signed out go to Login (and on to the
/// screen once the login succeeds). The account entry is Login while signed out and Settings,
/// where the account can be managed, while signed in.
fn rail_target(s: &AppState, entry: RailEntry) -> Screen {
    match entry {
        RailEntry::Account if s.is_signed_in() => Screen::Settings,
        RailEntry::Account => Screen::Login,
        RailEntry::Following | RailEntry::Search if !s.is_signed_in() => Screen::Login,
        RailEntry::Following => Screen::Following,
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
            RailEntry::Search => Some(Screen::Search),
            _ => None,
        };
        open_login(s, then, cmds);
    } else if target == Screen::Following {
        open_following(s, cmds);
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
            s.now_playing = None;
            // Audio only belongs to the stream that was left, not to the next one.
            s.audio_only = false;
            s.playback = Playback::Idle;
            s.paused = false;
            s.chat.clear();
            s.chat_status = ChatStatus::Idle;
            // The menu's choices stay for the next stream. Its numbers do not.
            s.player_menu = None;
            s.subscribe_card = false;
            s.stats = None;
            cmds.push(Command::StopPlayback);
            if s.session.stats {
                cmds.push(Command::SetStats(false));
            }
            cmds.push(Command::LeaveChat);
        }
        Screen::Login => leave_login(s, cmds),
        Screen::Category => s.category = None,
        _ => {}
    }
    s.chat_open = false;
    pop_screen(s);
}

fn push_screen(s: &mut AppState, screen: Screen) {
    s.back_stack.push(s.screen);
    s.screen = screen;
}

/// Returns to the previous screen, or Home if the stack is empty.
fn pop_screen(s: &mut AppState) {
    s.screen = s.back_stack.pop().unwrap_or(Screen::Home);
}
