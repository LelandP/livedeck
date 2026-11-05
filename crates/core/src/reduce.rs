//! The pure reducer: `(state, msg) -> (new state, commands)`. No IO, no clocks, no randomness.
//!
//! All navigation behaviour lives here, not in the Slint files: the back stack, paged feeds (Home,
//! Browse, Category) and the commands needed to fetch data.

use std::collections::HashSet;

use crate::domain::{grid_rows, ChatMessage, ChatStatus, FeedId, Paging, RowKind, Stream, Tile};
use crate::msg::{Command, Event, Intent, Msg, PageOutcome};
use crate::state::{AppState, CategoryFeed, Feed, Playback, RailEntry, Screen, SettingsRow};

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
    if s.exit_prompt {
        exit_prompt_intent(s, intent, cmds);
        return;
    }
    match s.screen {
        Screen::Login => login_intent(s, intent, cmds),
        Screen::Player => player_intent(s, intent, cmds),
        // Settings has no cards, but shares the rail handling and the Left-opens-the-rail rule.
        Screen::Home | Screen::Browse | Screen::Category | Screen::Following | Screen::Settings => {
            home_intent(s, intent, cmds)
        }
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
        SettingsRow::Quality => s.settings.quality = s.settings.quality.cycled(forward),
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
    match intent {
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
        Intent::Up | Intent::Down | Intent::Left | Intent::Right | Intent::Menu => pulse_hud(s),
        Intent::Pick { .. } | Intent::RailPick(_) => {}
    }
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
    pulse_hud(s);
    cmds.push(Command::Play {
        channel,
        quality: s.settings.quality,
    });
}

fn toggle_pause(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.paused = !s.paused;
    pulse_hud(s);
    cmds.push(Command::SetPaused(s.paused));
}

fn home_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    // Mouse fallbacks and Menu work whether or not the rail has focus.
    match intent {
        Intent::Pick { row, col } => {
            if s.tile(row, col).is_some() {
                if let Some(feed) = s.current_feed_mut() {
                    feed.focus.row = row;
                    feed.focus.col = col;
                }
                select_tile(s, cmds);
            }
            return;
        }
        Intent::RailPick(entry) => {
            activate_rail(s, entry, cmds);
            return;
        }
        Intent::Menu => {
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

    if s.screen == Screen::Settings {
        settings_intent(s, intent, cmds);
        return;
    }

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
            if let Some(feed) = s.current_feed_mut() {
                feed.focus.row = feed.focus.row.saturating_sub(1);
                feed.clamp_col();
            }
            load_more_if_near_end(s, cmds);
        }
        Intent::Down => {
            if let Some(feed) = s.current_feed_mut() {
                if !feed.rows.is_empty() {
                    feed.focus.row = (feed.focus.row + 1).min(feed.rows.len() - 1);
                    feed.clamp_col();
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
            if s.screen == Screen::Login {
                // Polling already finished, so no CancelLogin is needed.
                s.login = None;
                s.login_problem = None;
                match s.after_login.take() {
                    Some(Screen::Following) => open_following(s, cmds),
                    _ => pop_screen(s),
                }
            }
        }
        Event::SignedOut => {
            s.user = None;
            s.following = Feed::default();
            reload_home(s, cmds);
            // Following only exists while signed in. Anything that would lead back there goes
            // Home instead.
            for screen in &mut s.back_stack {
                if *screen == Screen::Following {
                    *screen = Screen::Home;
                }
            }
            if s.screen == Screen::Following {
                s.screen = Screen::Home;
                s.back_stack.clear();
                s.rail.index = RailEntry::Home;
            }
        }
    }
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
    push_screen(s, Screen::Player);
    start_playback(s, cmds);
}

/// Where a rail entry leads. Following while signed out goes to Login (and on to Following once
/// the login succeeds). The account entry is Login while signed out and Settings, where the
/// account can be managed, while signed in.
fn rail_target(s: &AppState, entry: RailEntry) -> Screen {
    match entry {
        RailEntry::Account if s.is_signed_in() => Screen::Settings,
        RailEntry::Account => Screen::Login,
        RailEntry::Following if !s.is_signed_in() => Screen::Login,
        RailEntry::Following => Screen::Following,
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
        let then = (entry == RailEntry::Following).then_some(Screen::Following);
        open_login(s, then, cmds);
    } else if target == Screen::Following {
        open_following(s, cmds);
    } else {
        // Root screens reset the back stack and drop any open category.
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
}

/// B / Escape on Player, Login or Category. Like the scaffold, it always closes chat.
fn back(s: &mut AppState, cmds: &mut Vec<Command>) {
    match s.screen {
        Screen::Player => {
            s.now_playing = None;
            s.playback = Playback::Idle;
            s.paused = false;
            s.chat.clear();
            s.chat_status = ChatStatus::Idle;
            cmds.push(Command::StopPlayback);
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
