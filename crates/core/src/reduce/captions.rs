//! Captions in the reducer (captions-plan.md, phases 3 and 3b).
//!
//! - The player menu's *Captions* row turns them on or off, saves the setting and tells the app
//!   (`Command::SetCaptions`); *Delay video for caption sync* turns the delay on or off.
//! - A caption from the speech engine (`Event::Caption`) is queued word by word by its time on
//!   the stream clock.
//! - The clock comes from the single player's health reports and the app's ticks
//!   (`captions.rs`). On each, words whose time has come go on screen (rolling up two lines),
//!   the lines clear after a pause, and chat lines held back with the picture come out.
//! - A video or clip has captions too, with the same queue. Its health reports reach `health`
//!   from `video::health` once a seek has landed, and its ticks move the clock at its speed.
//! - After every message, `settle` empties it all unless captions are showing, and on a load, a
//!   seek, a stop or a new channel: the clock is another one then. It also tells the player whether to
//!   hold the stream back (`Command::SetLiveDelay`) and lets held chat go when chat is no
//!   longer held back.

use crate::captions::{
    words_of, CaptionWord, CaptionsRow, ModelState, CLEAR_AFTER_MS, DELAY_BUFFER_MS, TICK_MS,
};
use crate::domain::ChatMessage;
use crate::msg::{Command, Slot, MAIN_SLOT};
use crate::netlimit::Health;
use crate::state::{AppState, Playback};

/// A, Left or Right on the menu row. Rows that cannot change say why instead (multiview, Kick,
/// sound only, no engine), and do nothing. A press while the model downloads cancels (turns them
/// off); a press after a failed download tries again.
pub(super) fn toggle(s: &mut AppState, cmds: &mut Vec<Command>) {
    let row = s.captions_row();
    if !row.can_toggle() {
        return;
    }
    if row == CaptionsRow::DownloadFailed {
        s.captions_models = ModelState::Downloading(0);
        cmds.push(Command::SetCaptions(true));
        return;
    }
    s.settings.captions = !s.settings.captions;
    s.captions_models = ModelState::Ready;
    cmds.push(Command::SetCaptions(s.settings.captions));
    cmds.push(Command::SaveSettings(s.settings));
}

/// Left or Right on the *Caption quality* row: the next model. With captions on, the app loads it
/// (downloading it first if it is missing).
pub(super) fn cycle_quality(s: &mut AppState, forward: bool, cmds: &mut Vec<Command>) {
    if s.captions_unavailable {
        return;
    }
    let next = s
        .captions_quality()
        .cycled_in(s.captions_qualities(), forward);
    s.settings.captions_quality = next;
    cmds.push(Command::SetCaptionsQuality(next));
    if s.settings.captions {
        s.captions_models = ModelState::Ready;
        cmds.push(Command::SetCaptions(true));
    }
    cmds.push(Command::SaveSettings(s.settings));
}

/// A, Left or Right on *Delay video for caption sync*: on or off, saved. The player hears of it
/// from `settle`, and only while captions run on the stream.
pub(super) fn toggle_delay(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.settings.caption_delay = !s.settings.caption_delay;
    cmds.push(Command::SaveSettings(s.settings));
}

/// The app's word on the model download. Late words from a download that was cancelled are
/// ignored.
pub(super) fn model(s: &mut AppState, state: ModelState) {
    if s.settings.captions {
        s.captions_models = state;
    }
}

/// A caption spoken from `start_ms` to `end_ms`, with its words' times (none: spread over it).
pub(super) fn caption(
    s: &mut AppState,
    start_ms: u64,
    end_ms: u64,
    text: &str,
    words: Vec<CaptionWord>,
    cmds: &mut Vec<Command>,
) {
    if !s.captions_live() {
        return;
    }
    let now = s.captions.clock_ms;
    let after = s.captions.show_after_ms();
    // Over before it came (the engine was slow, or the player jumped on): nobody reads it now.
    if now.is_some_and(|now| end_ms.saturating_add(after + CLEAR_AFTER_MS) <= now) {
        return;
    }
    let words = if words.is_empty() {
        words_of(start_ms, end_ms, text)
    } else {
        words
    };
    for mut word in words {
        // Late words come on at once, in their order (`advance` adds `after` back).
        if let Some(now) = now {
            word.start_ms = word.start_ms.max(now.saturating_sub(after));
        }
        s.captions.enqueue(word);
    }
    advance(s, cmds);
}

/// The single player's health report: where its stream is, and how much it has buffered.
pub(super) fn health(s: &mut AppState, health: &Health, cmds: &mut Vec<Command>) {
    if !s.captions_live() {
        return;
    }
    if let Some(ms) = health.stream_ms {
        s.captions.clock_ms = Some(ms);
    }
    s.captions.buffer_ms = health.buffer_ms;
    // A held player sends none: this one plays, and its buffer includes the hold.
    s.captions.held = false;
    s.captions.held_ms = 0;
    advance(s, cmds);
}

/// The player of `slot` held its picture for the delay, or let it go.
pub(super) fn held(s: &mut AppState, slot: Slot, held: bool, cmds: &mut Vec<Command>) {
    if slot != MAIN_SLOT || !s.captions_live() {
        return;
    }
    s.captions.held = held;
    advance(s, cmds);
}

/// The tick `id` of the clock ran out.
pub(super) fn tick_due(s: &mut AppState, id: u32, cmds: &mut Vec<Command>) {
    if id != s.captions.last_tick || !s.captions.ticking {
        return;
    }
    s.captions.ticking = false;
    if !s.captions_live() {
        return;
    }
    // A stream that is paused, held or still loading does not move on, nor does a video that is
    // being scrubbed (its picture is held). Held, its buffer grows. A video moves at its speed.
    if s.captions.held {
        s.captions.held_ms += TICK_MS;
    } else if !s.paused && s.playback == Playback::Playing && !s.video_scrubbing() {
        let step = s.caption_tick_step_ms();
        s.captions.clock_ms = s.captions.clock_ms.map(|ms| ms + step);
    }
    advance(s, cmds);
}

/// New chat lines of the channel on screen: held back with the picture while the delay is on
/// (the user's own lines show at once: they were just sent), shown at once otherwise.
pub(super) fn chat_lines(s: &mut AppState, lines: Vec<ChatMessage>, cmds: &mut Vec<Command>) {
    let Some(due) = s.chat_due_ms() else {
        s.chat.release_all();
        s.chat.extend(lines);
        return;
    };
    let own = s.user.as_ref().map(|u| u.login.clone());
    for line in lines {
        if own
            .as_deref()
            .is_some_and(|login| line.login.eq_ignore_ascii_case(login))
        {
            s.chat.push(line);
        } else {
            s.chat.hold(due, line);
        }
    }
    advance(s, cmds);
}

/// Moves words whose time has come to the screen, clears the lines after a pause, lets held chat
/// out, and asks for the next tick while there is anything left to do.
fn advance(s: &mut AppState, cmds: &mut Vec<Command>) {
    let paused = s.paused;
    let Some(now) = s.captions.clock_ms else {
        return;
    };
    let c = &mut s.captions;
    let after = c.show_after_ms();
    while c
        .queue
        .front()
        .is_some_and(|w| w.start_ms.saturating_add(after) <= now)
    {
        let Some(word) = c.queue.pop_front() else {
            break;
        };
        // Missed altogether: the clock jumped over it.
        if word.start_ms.saturating_add(after + CLEAR_AFTER_MS) <= now {
            continue;
        }
        c.show(&word.text);
        c.last_word_ms = Some(now);
    }
    if c.last_word_ms
        .is_some_and(|at| at.saturating_add(CLEAR_AFTER_MS) <= now)
    {
        c.shown.clear();
        c.last_word_ms = None;
    }
    s.chat.release(now);
    let c = &mut s.captions;
    // Held for the delay, the ticks count how long (at most `HOLD_MAX` in the player, 20 s).
    let busy = !c.queue.is_empty() || !c.shown.is_empty() || s.chat.held_len() > 0 || c.held;
    // A paused stream sends no reports, but the one going on does and starts them again.
    if !c.ticking && !paused && busy {
        c.ticking = true;
        c.last_tick = c.last_tick.wrapping_add(1);
        cmds.push(Command::CaptionTick {
            id: c.last_tick,
            ms: TICK_MS,
        });
    }
}

/// Run after every message. A load, a seek, a stop or a new channel of the single player (its
/// commands) starts another clock (the app restarts the audio tap at the new place), and anything
/// but a Twitch stream, video or clip playing on the Player screen with captions on has no use for
/// the queue: it goes, with the clock. Until the first report after a seek there is no clock, so
/// the lines of the old place cannot show. Chat that is no longer held
/// back comes out, and the player is told when the delay starts or stops applying (before a load
/// in the same batch, so the load starts behind).
pub(super) fn settle(s: &mut AppState, cmds: &mut Vec<Command>) {
    let restarts = cmds.iter().any(|c| {
        matches!(
            c,
            Command::Play {
                slot: MAIN_SLOT,
                ..
            } | Command::PlayVideo {
                slot: MAIN_SLOT,
                ..
            } | Command::Seek {
                slot: MAIN_SLOT,
                ..
            } | Command::StopPlayback { slot: MAIN_SLOT }
        )
    });
    if restarts || !s.captions_live() {
        s.captions.clear();
    }
    if s.chat_due_ms().is_none() {
        s.chat.release_all();
    }
    let delay = s.caption_delay_wanted();
    if delay != s.captions.delaying {
        s.captions.delaying = delay;
        cmds.insert(0, Command::SetLiveDelay(delay.then_some(DELAY_BUFFER_MS)));
    }
}
