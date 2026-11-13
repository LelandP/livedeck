//! The video player (vod-plan.md, phase 4): a past broadcast, highlight or clip on the Player
//! screen. `AppState::video` holds it and `now_playing` stays `None`, so nothing that follows a
//! live stream (chat, raids, hype trains, live notifications for the channel on screen, the
//! offline recheck, the slow-network rule) reaches it, without a check in each place.
//! `playback`, `paused` and `audio_only` are shared with live streams and mean the same.
//!
//! | Button  | Does |
//! |---------|------|
//! | A       | Pause / play. On the error screen: try again from where it was |
//! | ◀ ▶     | Taps add up into one seek, a hold scrubs (`seek.rs`) |
//! | LB / RB | Start of this chapter (or the previous one within 5 s of its start) / next chapter |
//! | Y       | Chat replay side panel (`replay.rs`): a broadcast or highlight, not a clip |
//! | Start   | The player menu, with a video's rows (`PlayerMenuRow::VIDEO`) |
//! | B       | Back. While scrubbing it cancels the scrub instead |
//!
//! The player takes a `Command::Seek` only once a video shows (`VideoLoad::Shown`). Before the
//! first frame, and while another quality loads behind the picture, a seek goes into the
//! `start_ms` of a fresh `Command::PlayVideo` instead, so it is never lost.

use crate::msg::{Command, Dir, Intent, MAIN_SLOT};
use crate::netlimit::Health;
use crate::seek::{self, Scrub, Seeking, TapRun};
use crate::state::{AppState, Playback, Screen, ToastKind};
use crate::video::{
    EndRow, Landing, VideoDetails, VideoItem, VideoLoad, VideoSession, VIDEO_GONE_MESSAGE,
};

use super::{
    back, channel, continue_watching, pause_in_background, pop_screen, pulse_hud, push_screen,
    push_toast, replay, toggle_pause,
};

/// Plays `item` from `start_ms` on the Player screen, over the current screen (the channel page),
/// so B comes back to it. `resumed`: `start_ms` is a saved place, and a toast says so.
///
/// A subscriber-only video opens the subscribe card instead (where one can be shown), and nothing
/// plays in multiview (vod-plan.md: no videos in multiview).
pub(super) fn open(
    s: &mut AppState,
    item: VideoItem,
    start_ms: u64,
    resumed: bool,
    cmds: &mut Vec<Command>,
) {
    if s.multiview.is_some() {
        return;
    }
    if let VideoItem::Video(video) = &item {
        if video.sub_only {
            s.subscribe_card = video.channel.features().subscribe
                && matches!(s.screen, Screen::Channel | Screen::Player);
            return;
        }
    }
    // The clock for *Streamed Nov 10*: the page's, or (from Home) the newest saved place's.
    let now = s
        .channel_page
        .as_ref()
        .map(|p| p.now)
        .filter(|now| *now > 0)
        .or_else(|| s.continue_watching.iter().map(|p| p.saved_at).max())
        .unwrap_or(0);
    // A live stream playing behind the page stops first: a video and a stream cannot share
    // `now_playing`. B from the page brings it back (`channel::back_to_live`).
    channel::park_live(s, cmds);
    let details = item.video_id().map(str::to_owned);
    let mut session = VideoSession::new(item, 0, now);
    session.pos_ms = session.clamp(start_ms);
    let resumed_at = (resumed && session.pos_ms > 0).then_some(session.pos_ms);
    s.rail.focused = false;
    s.subscribe_card = false;
    s.player_menu = None;
    s.chat_open = false;
    // The replay panel starts closed and empty, whatever the last stream or video left in it.
    s.chat.clear();
    // Every video opens as video, as every stream does.
    s.audio_only = false;
    s.video = Some(session);
    push_screen(s, Screen::Player);
    start(s, cmds);
    if let Some(id) = details {
        cmds.push(Command::LoadVideoDetails { id });
    }
    if let Some(ms) = resumed_at {
        let pos_s = u32::try_from(ms / 1_000).unwrap_or(u32::MAX);
        push_toast(s, ToastKind::Resumed { pos_s }, cmds);
    }
}

/// (Re)starts the video from `pos_ms`: the loading screen until its first frame. Also *Watch
/// again*, and A or X after a failure.
fn start(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    v.load = VideoLoad::Starting;
    v.seeking = Seeking::Idle;
    v.landing = None;
    v.end_card = None;
    s.playback = Playback::Loading;
    s.paused = false;
    s.background_paused = false;
    // The numbers of the last load mean nothing for this one.
    s.stats = None;
    disarm(s);
    pulse_hud(s);
    push_play(s, cmds);
    push_stats(s, cmds);
    // A fresh load from `pos_ms` is a jump for the chat replay too.
    replay::restart(s, cmds);
}

/// The video again at another quality, or as sound only, from where it is. While it plays it
/// stays on screen: the app loads the new one behind it and swaps at its first frame
/// (`VideoLoad::Reloading`). A video that does not play starts again.
pub(super) fn reload(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    // Taps not sent yet go into the new load.
    if let Some(run) = v.seeking.run() {
        v.pos_ms = v.clamp(run.raw_target());
    }
    v.seeking = Seeking::Idle;
    v.landing = None;
    if s.playback != Playback::Playing || v.end_card.is_some() {
        start(s, cmds);
        return;
    }
    v.load = VideoLoad::Reloading { reports: 0 };
    v.reload_from_ms = v.pos_ms;
    // The new player starts playing.
    s.paused = false;
    s.background_paused = false;
    disarm(s);
    pulse_hud(s);
    push_play(s, cmds);
    push_stats(s, cmds);
}

/// Asks the single player for the video from `pos_ms`, leaving the state alone.
fn push_play(s: &AppState, cmds: &mut Vec<Command>) {
    let Some(v) = &s.video else {
        return;
    };
    cmds.push(Command::PlayVideo {
        slot: MAIN_SLOT,
        target: v.item.target(),
        start_ms: v.pos_ms,
        stream: s.stream_kind(),
    });
}

/// Leaving the player stops the stats reports, so a video started with the box on asks again.
fn push_stats(s: &AppState, cmds: &mut Vec<Command>) {
    if s.session.stats {
        cmds.push(Command::SetStats {
            slot: MAIN_SLOT,
            on: true,
        });
    }
}

/// Stops the video and forgets it: B, *Back* on the end card, *Channel videos*, the sleep timer.
/// The caller leaves the screen. Its place goes into *Continue watching* first.
pub(super) fn leave(s: &mut AppState, cmds: &mut Vec<Command>) {
    continue_watching::save_leaving(s, cmds);
    cmds.push(Command::StopPlayback { slot: MAIN_SLOT });
    if s.session.stats {
        cmds.push(Command::SetStats {
            slot: MAIN_SLOT,
            on: false,
        });
    }
    s.video = None;
    // The replay's lines go with it (the panel closes in `back`).
    s.chat.clear();
    s.playback = Playback::Idle;
    s.paused = false;
    s.background_paused = false;
    // Audio only belongs to the video that was left, not to the next one.
    s.audio_only = false;
    // The menu's choices stay for the next stream. Its numbers do not.
    s.player_menu = None;
    s.subscribe_card = false;
    s.stats = None;
}

/// *Channel videos* (the menu's row and the end card's): back to the channel page the video was
/// opened from, with the focus where it was. Without one below (a video opened elsewhere), the
/// video's channel's page takes the player's place.
pub(super) fn to_channel_page(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_ref() else {
        return;
    };
    let channel = v.item.channel().clone();
    let name = v.item.channel_name().to_owned();
    leave(s, cmds);
    s.chat_open = false;
    pop_screen(s);
    let on_page = s.screen == Screen::Channel
        && s.channel_page
            .as_ref()
            .is_some_and(|p| p.channel.is_same(&channel));
    if !on_page {
        channel::open_channel(s, channel, name, cmds);
    }
}

/// A new seek timer id: timers started before find nothing to do.
fn arm(s: &mut AppState) -> u32 {
    s.last_seek_id = s.last_seek_id.wrapping_add(1);
    let id = s.last_seek_id;
    if let Some(v) = s.video.as_mut() {
        v.timer = id;
    }
    id
}

/// Lets every running seek timer run out without effect.
fn disarm(s: &mut AppState) {
    arm(s);
}

/// Sends the video to `to_ms`, kept inside it, and draws it there at once. A player that cannot
/// take a seek now gets it as the start of a fresh load instead.
fn seek_to(s: &mut AppState, to_ms: u64, cmds: &mut Vec<Command>) {
    let stream = s.stream_kind();
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let to = v.clamp(to_ms);
    v.pos_ms = to;
    let play = Command::PlayVideo {
        slot: MAIN_SLOT,
        target: v.item.target(),
        start_ms: to,
        stream,
    };
    let landing = Landing {
        to_ms: to,
        reports: seek::SEEK_LAND_REPORTS,
    };
    match v.load {
        VideoLoad::Shown => {
            cmds.push(Command::Seek {
                slot: MAIN_SLOT,
                to_ms: to,
            });
            v.landing = Some(landing);
        }
        // Nothing is loaded yet: a seek would be dropped.
        VideoLoad::Starting => cmds.push(play),
        // The picture moves now, and the new quality starts there too (a seek alone would be
        // lost when it takes over).
        VideoLoad::Reloading { .. } => {
            cmds.push(Command::Seek {
                slot: MAIN_SLOT,
                to_ms: to,
            });
            v.landing = Some(landing);
            v.load = VideoLoad::Reloading { reports: 0 };
            v.reload_from_ms = to;
            cmds.push(play);
        }
    }
    // Every jump (taps, a scrub's release, chapters, *Start over*) clears the chat replay and
    // fetches it again from the new spot.
    replay::restart(s, cmds);
}

/// A scrub holds the picture still, unless the viewer had paused it already.
fn hold_picture(s: &AppState, cmds: &mut Vec<Command>) {
    if !s.paused {
        cmds.push(Command::SetPaused {
            slot: MAIN_SLOT,
            paused: true,
        });
    }
}

/// The end of a scrub: the picture plays again, unless the viewer had paused it (or the app went
/// to the background meanwhile).
fn release_picture(s: &AppState, cmds: &mut Vec<Command>) {
    if !s.paused {
        cmds.push(Command::SetPaused {
            slot: MAIN_SLOT,
            paused: false,
        });
    }
}

/// Input on the Player screen while a video is on it, once the subscribe card and the player
/// menu have had their turn (the button table at the top of this file).
pub(super) fn intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_ref() else {
        return;
    };
    if v.end_card.is_some() {
        end_card_intent(s, intent, cmds);
        return;
    }
    if v.seeking.scrub().is_some() {
        // A scrub takes the D-pad, A and B. Everything else waits for it to end.
        match intent {
            Intent::Left => press(s, Dir::Left, cmds),
            Intent::Right => press(s, Dir::Right, cmds),
            Intent::Select => finish_scrub(s, cmds),
            Intent::Back => cancel_scrub(s, cmds),
            _ => {}
        }
        return;
    }
    let failed = matches!(s.playback, Playback::Failed(_));
    match intent {
        Intent::Left | Intent::Right | Intent::PageLeft | Intent::PageRight if failed => {
            pulse_hud(s);
        }
        Intent::Left => press(s, Dir::Left, cmds),
        Intent::Right => press(s, Dir::Right, cmds),
        Intent::Select | Intent::Retry if failed => start(s, cmds),
        Intent::Select if s.playback == Playback::Playing => toggle_pause(s, cmds),
        Intent::PageLeft => chapter(s, false, cmds),
        Intent::PageRight => chapter(s, true, cmds),
        Intent::Menu => open_menu(s, cmds),
        Intent::Back => back(s, cmds),
        Intent::Up | Intent::Down => pulse_hud(s),
        Intent::ToggleChat if !failed => replay::toggle(s, cmds),
        // View: no videos in multiview. Typing never reaches the player, and releases are
        // handled before (`released`).
        Intent::Select
        | Intent::Retry
        | Intent::ToggleChat
        | Intent::View
        | Intent::Pick { .. }
        | Intent::RailPick(_)
        | Intent::Text(_)
        | Intent::Erase
        | Intent::Released(_) => {}
    }
}

/// Input on the card at the end of the video: Up and Down pick a row, A runs it, B goes back.
fn end_card_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let Some(row) = v.end_card else {
        return;
    };
    match intent {
        Intent::Up => v.end_card = Some(row.prev()),
        Intent::Down => v.end_card = Some(row.next()),
        Intent::Select => match row {
            EndRow::WatchAgain => {
                v.pos_ms = 0;
                start(s, cmds);
            }
            EndRow::ChannelVideos => to_channel_page(s, cmds),
            EndRow::Back => back(s, cmds),
        },
        Intent::Back => back(s, cmds),
        _ => {}
    }
}

/// Start: the player menu on its first row, with *Chapters* pointing at the chapter on screen.
/// A press still down counts as a tap from here: the menu takes the D-pad.
fn open_menu(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let pressed = match v.seeking {
        Seeking::Pressed { run, .. } => {
            v.seeking = Seeking::Taps {
                run,
                settled: false,
            };
            true
        }
        _ => false,
    };
    v.menu_chapter = v.chapter_at(v.shown_ms()).unwrap_or(0);
    if pressed {
        let id = arm(s);
        cmds.push(Command::SettleSeek {
            id,
            ms: seek::SEEK_SETTLE_MS,
        });
    }
    s.player_menu = s.player_menu_rows().first().copied();
    pulse_hud(s);
}

/// ◀ or ▶ went down.
fn press(s: &mut AppState, dir: Dir, cmds: &mut Vec<Command>) {
    pulse_hud(s);
    let Some(v) = s.video.as_mut() else {
        return;
    };
    // Any press ends the carry-over window: only this one can use it.
    let carry = v.carry.take();
    match v.seeking {
        Seeking::Scrub(mut scrub) => {
            // The same direction still held: its release was lost, the scrub carries on.
            if scrub.held && scrub.dir == dir {
                return;
            }
            // The other direction reverses, slowly again. The same one again shortly after a
            // release (a rocker D-pad passing through the middle) carries on near the speed it
            // had, as a new hold soon after would.
            scrub.ticks = if scrub.dir == dir {
                seek::carried_ticks(scrub.ticks, v.length_ms)
            } else {
                0
            };
            scrub.dir = dir;
            scrub.held = true;
            scrub.step(v.length_ms, v.end_ms());
            v.seeking = Seeking::Scrub(scrub);
            let id = arm(s);
            cmds.push(Command::ScrubTick { id });
            return;
        }
        Seeking::Idle => {
            v.seeking = Seeking::Pressed {
                dir,
                run: TapRun::new(v.pos_ms, dir),
                carry: carry.filter(|c| c.dir == dir).map_or(0, |c| c.ticks),
            };
        }
        // Another tap of a run (or a press whose release was lost, which counts as a tap).
        Seeking::Pressed { mut run, .. } | Seeking::Taps { mut run, .. } => {
            run.tap(dir);
            v.seeking = Seeking::Pressed { dir, run, carry: 0 };
        }
    }
    let id = arm(s);
    cmds.push(Command::HoldCheck { id });
}

/// ◀ or ▶ was let go (`Intent::Released`). A press becomes a tap, whose run settles; a scrub
/// stops and jumps a moment later. Releases of anything else are ignored, as on every other
/// screen.
pub(super) fn released(s: &mut AppState, dir: Dir, cmds: &mut Vec<Command>) {
    if s.screen != Screen::Player {
        return;
    }
    let Some(v) = s.video.as_mut() else {
        return;
    };
    match v.seeking {
        Seeking::Pressed { dir: down, run, .. } if down == dir => {
            v.seeking = Seeking::Taps {
                run,
                settled: false,
            };
        }
        Seeking::Scrub(mut scrub) if scrub.held && scrub.dir == dir => {
            scrub.held = false;
            v.seeking = Seeking::Scrub(scrub);
        }
        _ => return,
    }
    let id = arm(s);
    cmds.push(Command::SettleSeek {
        id,
        ms: seek::SEEK_SETTLE_MS,
    });
}

/// Whatever ◀ ▶ hold is let go: the window lost focus (its key-up may never come).
pub(super) fn let_go(s: &mut AppState, cmds: &mut Vec<Command>) {
    let held = match s.video.as_ref().map(|v| v.seeking) {
        Some(Seeking::Pressed { dir, .. }) => Some(dir),
        Some(Seeking::Scrub(scrub)) if scrub.held => Some(scrub.dir),
        _ => None,
    };
    if let Some(dir) = held {
        released(s, dir, cmds);
    }
}

/// `Event::HoldDue`: the press is still down, so it scrubs. Its own tap is taken back; taps
/// before it in the same run still count, and are sent now if they were not yet. Only a video
/// that shows can scrub: before its first frame a hold stays a press, and its release a tap.
pub(super) fn hold_due(s: &mut AppState, id: u32, cmds: &mut Vec<Command>) {
    let free = s.screen == Screen::Player
        && s.playback == Playback::Playing
        && s.player_menu.is_none()
        && !s.subscribe_card;
    let Some(v) = s.video.as_mut().filter(|v| v.timer == id) else {
        return;
    };
    let Seeking::Pressed {
        dir,
        mut run,
        carry,
    } = v.seeking
    else {
        return;
    };
    if !free || v.load == VideoLoad::Starting || v.end_card.is_some() {
        return;
    }
    run.untap(dir);
    let from = v.clamp(run.raw_target());
    let unsent = from != run.video_at();
    let mut scrub = Scrub {
        dir,
        held: true,
        from_ms: from,
        at_ms: from,
        // Soon after a scrub the same way, near the speed it had.
        ticks: carry,
        total_ticks: 0,
    };
    // The first step at once, so the hold shows.
    scrub.step(v.length_ms, v.end_ms());
    v.seeking = Seeking::Scrub(scrub);
    if unsent {
        seek_to(s, from, cmds);
    }
    hold_picture(s, cmds);
    let id = arm(s);
    cmds.push(Command::ScrubTick { id });
    pulse_hud(s);
}

/// `Event::ScrubTickDue`: the next step while the button is held. Held for
/// `seek::SCRUB_MAX_TICKS`, the scrub ends as if let go (its release was lost).
pub(super) fn scrub_tick(s: &mut AppState, id: u32, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut().filter(|v| v.timer == id) else {
        return;
    };
    let Seeking::Scrub(mut scrub) = v.seeking else {
        return;
    };
    if !scrub.held {
        return;
    }
    if scrub.total_ticks >= seek::SCRUB_MAX_TICKS {
        finish_scrub(s, cmds);
        return;
    }
    scrub.step(v.length_ms, v.end_ms());
    scrub.total_ticks += 1;
    v.seeking = Seeking::Scrub(scrub);
    cmds.push(Command::ScrubTick { id });
}

/// `Event::SeekSettleDue`: the taps' seek goes out, and later the run ends (another tap can
/// still add to it until then). A released scrub jumps.
pub(super) fn settle_due(s: &mut AppState, id: u32, cmds: &mut Vec<Command>) {
    // The end of a carry-over window has its own id.
    if let Some(v) = s.video.as_mut() {
        if v.carry.is_some_and(|c| c.id == id) {
            v.carry = None;
            return;
        }
    }
    let Some(v) = s.video.as_mut().filter(|v| v.timer == id) else {
        return;
    };
    match v.seeking {
        Seeking::Taps {
            mut run,
            settled: false,
        } => {
            let to = v.clamp(run.raw_target());
            let moved = to != run.video_at();
            if moved {
                run.sent_ms = Some(to);
            }
            v.seeking = Seeking::Taps { run, settled: true };
            if moved {
                seek_to(s, to, cmds);
            }
            cmds.push(Command::SettleSeek {
                id,
                ms: seek::TAP_JOIN_MS.saturating_sub(seek::SEEK_SETTLE_MS),
            });
        }
        Seeking::Taps { settled: true, .. } => {
            v.seeking = Seeking::Idle;
            // The HUD stays a while after the chip goes.
            pulse_hud(s);
        }
        Seeking::Scrub(scrub) if !scrub.held => finish_scrub(s, cmds),
        Seeking::Idle | Seeking::Pressed { .. } | Seeking::Scrub(_) => {}
    }
}

/// The scrub ends: the video jumps to the playhead and plays again (unless it was paused).
fn finish_scrub(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let Seeking::Scrub(scrub) = v.seeking else {
        return;
    };
    v.seeking = Seeking::Idle;
    let carried = seek::carried_ticks(scrub.ticks, v.length_ms);
    disarm(s);
    // Let go (not A while held, nor the 30 s guard): a hold the same way soon after picks up
    // near this speed. The window counts from the release, `SEEK_SETTLE_MS` ago.
    if !scrub.held && carried > 0 {
        let id = arm_carry(s);
        if let Some(v) = s.video.as_mut() {
            v.carry = Some(seek::Carry {
                dir: scrub.dir,
                ticks: carried,
                id,
            });
        }
        cmds.push(Command::SettleSeek {
            id,
            ms: seek::SCRUB_CARRY_MS.saturating_sub(seek::SEEK_SETTLE_MS),
        });
    }
    if scrub.at_ms != scrub.from_ms {
        seek_to(s, scrub.at_ms, cmds);
    }
    release_picture(s, cmds);
    pulse_hud(s);
}

/// A new id for the end of a carry-over window, from the seek timers' count, without making it
/// the latest seek timer: the timers of the next press must not cancel it.
fn arm_carry(s: &mut AppState) -> u32 {
    s.last_seek_id = s.last_seek_id.wrapping_add(1);
    s.last_seek_id
}

/// B while scrubbing: back to where it started, as if nothing happened.
fn cancel_scrub(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let Seeking::Scrub(scrub) = v.seeking else {
        return;
    };
    v.seeking = Seeking::Idle;
    // The picture never left it.
    v.pos_ms = scrub.from_ms;
    disarm(s);
    release_picture(s, cmds);
    pulse_hud(s);
}

/// LB (`forward == false`): the start of this chapter, or of the one before within
/// `seek::CHAPTER_BACK_MS` of this one's start. RB: the next chapter. A video without chapters
/// is one chapter. Counted from where the playhead is drawn, so taps still adding up count.
fn chapter(s: &mut AppState, forward: bool, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let at = v.shown_ms();
    let starts: Vec<u64> = if v.chapters.is_empty() {
        vec![0]
    } else {
        v.chapters.iter().map(|c| c.start_ms).collect()
    };
    let i = starts.iter().rposition(|st| *st <= at).unwrap_or(0);
    let to = if forward {
        starts.get(i + 1).copied()
    } else if i > 0 && at.saturating_sub(starts[i]) < seek::CHAPTER_BACK_MS {
        Some(starts[i - 1])
    } else {
        Some(starts[i])
    };
    // The jump replaces the taps.
    v.seeking = Seeking::Idle;
    disarm(s);
    pulse_hud(s);
    if let Some(to) = to {
        seek_to(s, to, cmds);
    }
}

/// The menu's *Start over*: the video from 0:00, playing. The menu closes.
pub(super) fn start_over(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.player_menu = None;
    let Some(v) = s.video.as_mut() else {
        return;
    };
    v.seeking = Seeking::Idle;
    if !matches!(s.playback, Playback::Playing | Playback::Loading) || v.end_card.is_some() {
        v.pos_ms = 0;
        start(s, cmds);
        return;
    }
    disarm(s);
    pulse_hud(s);
    seek_to(s, 0, cmds);
    if s.paused {
        toggle_pause(s, cmds);
    }
}

/// Left or Right on the menu's *Chapters* row: the chapter it points at.
pub(super) fn menu_point(s: &mut AppState, forward: bool) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let last = v.chapters.len().saturating_sub(1);
    v.menu_chapter = if forward {
        (v.menu_chapter + 1).min(last)
    } else {
        v.menu_chapter.saturating_sub(1)
    };
}

/// A on the menu's *Chapters* row: there, and the menu closes.
pub(super) fn menu_jump(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let Some(to) = v.chapters.get(v.menu_chapter).map(|c| c.start_ms) else {
        return;
    };
    s.player_menu = None;
    v.seeking = Seeking::Idle;
    // On the error screen or the end card there is no picture to seek in: the video starts again
    // there, as *Start over* does (a bare load would play on behind the error).
    if !matches!(s.playback, Playback::Playing | Playback::Loading) || v.end_card.is_some() {
        v.pos_ms = v.clamp(to);
        start(s, cmds);
        return;
    }
    disarm(s);
    pulse_hud(s);
    seek_to(s, to, cmds);
}

/// Left or Right (or A) on the menu's *Speed* row: the next speed, for this video only.
pub(super) fn change_speed(s: &mut AppState, forward: bool, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    v.speed = seek::cycled_speed(v.speed, forward);
    cmds.push(Command::SetSpeed {
        slot: MAIN_SLOT,
        milli: v.speed,
    });
}

/// The player's first frame (`Event::PlaybackStarted`): the video shows, or the new quality took
/// over from the old one.
pub(super) fn started(s: &mut AppState, slot: u8, cmds: &mut Vec<Command>) {
    if slot != MAIN_SLOT || s.screen != Screen::Player {
        return;
    }
    let Some(v) = s.video.as_mut() else {
        return;
    };
    match v.load {
        VideoLoad::Starting if s.playback == Playback::Loading => {
            v.load = VideoLoad::Shown;
            s.playback = Playback::Playing;
            pulse_hud(s);
            pause_in_background(s, cmds);
        }
        VideoLoad::Reloading { .. } => {
            v.load = VideoLoad::Shown;
            // The old player played on while this one loaded: catch up to where it got to, or
            // the last seconds would play twice.
            if v.pos_ms > v.reload_from_ms.saturating_add(seek::RELOAD_CATCH_UP_MS) {
                let to = v.clamp(v.pos_ms);
                cmds.push(Command::Seek {
                    slot: MAIN_SLOT,
                    to_ms: to,
                });
                v.landing = Some(Landing {
                    to_ms: to,
                    reports: seek::SEEK_LAND_REPORTS,
                });
            }
            // The new player starts playing: a scrub still holds the picture.
            if v.seeking.scrub().is_some() {
                cmds.push(Command::SetPaused {
                    slot: MAIN_SLOT,
                    paused: true,
                });
            }
        }
        VideoLoad::Starting | VideoLoad::Shown => {}
    }
}

/// The video played to its end (`Event::PlaybackEnded`): no autoplay, the last frame stays under
/// the end card.
pub(super) fn ended(s: &mut AppState, slot: u8, cmds: &mut Vec<Command>) {
    if slot != MAIN_SLOT || s.screen != Screen::Player || s.playback != Playback::Playing {
        return;
    }
    // Watched: it leaves *Continue watching*.
    continue_watching::finished(s, cmds);
    let Some(v) = s.video.as_mut() else {
        return;
    };
    v.seeking = Seeking::Idle;
    v.landing = None;
    v.load = VideoLoad::Shown;
    if v.length_ms > 0 {
        v.pos_ms = v.length_ms;
    }
    v.end_card = Some(EndRow::default());
    s.player_menu = None;
    s.paused = false;
    s.background_paused = false;
    disarm(s);
    pulse_hud(s);
}

/// The player could not play the video (`Event::PlaybackError`): a video that was deleted, a
/// failed resolve, a failed load. The error screen offers A to try again from where it was, and B.
pub(super) fn failed(s: &mut AppState, slot: u8, message: String, cmds: &mut Vec<Command>) {
    if slot != MAIN_SLOT || s.screen != Screen::Player {
        return;
    }
    if !matches!(s.playback, Playback::Loading | Playback::Playing) {
        return;
    }
    // A video that is gone has nothing to continue.
    if message == VIDEO_GONE_MESSAGE {
        continue_watching::finished(s, cmds);
    }
    let Some(v) = s.video.as_mut() else {
        return;
    };
    if let Some(run) = v.seeking.run() {
        v.pos_ms = v.clamp(run.raw_target());
    }
    v.seeking = Seeking::Idle;
    v.landing = None;
    v.end_card = None;
    v.load = VideoLoad::Starting;
    s.playback = Playback::Failed(message);
    s.paused = false;
    s.background_paused = false;
    disarm(s);
}

/// A health report of the single player: the video's position and length. Reports still on their
/// way from before a seek are ignored until it has landed (`Landing`), so the playhead never
/// flicks back. Entering a muted part brings the HUD back, with its chip.
pub(super) fn health(s: &mut AppState, slot: u8, health: Health, cmds: &mut Vec<Command>) {
    if slot != MAIN_SLOT || s.screen != Screen::Player || s.playback != Playback::Playing {
        return;
    }
    let Some(v) = s.video.as_mut() else {
        return;
    };
    if v.end_card.is_some() {
        return;
    }
    if let Some(length) = health.length_ms.filter(|l| *l > 0) {
        v.length_ms = length;
    }
    if let VideoLoad::Reloading { reports } = &mut v.load {
        *reports += 1;
        if *reports >= seek::RELOAD_GIVE_UP_REPORTS {
            v.load = VideoLoad::Shown;
        }
    }
    let Some(pos) = health.pos_ms else {
        return;
    };
    if let Some(landing) = v.landing.as_mut() {
        let near = pos.saturating_add(seek::SEEK_LAND_BEFORE_MS) >= landing.to_ms
            && pos <= landing.to_ms.saturating_add(seek::SEEK_LAND_AFTER_MS);
        if near || landing.reports == 0 {
            v.landing = None;
        } else {
            landing.reports -= 1;
            return;
        }
    }
    let was_muted = v.is_muted_at(v.pos_ms);
    v.pos_ms = pos;
    if !was_muted && v.is_muted_at(pos) {
        pulse_hud(s);
    }
    // The chat replay follows the playhead: lines come due, the next window is asked for.
    replay::tick(s, cmds);
}

/// `Event::VideoDetails`: the chapters, muted parts and seek previews of the video on screen.
pub(super) fn details(s: &mut AppState, details: VideoDetails) {
    let Some(v) = s
        .video
        .as_mut()
        .filter(|v| v.item.video_id() == Some(details.id.as_str()))
    else {
        return;
    };
    v.chapters = details.chapters;
    v.muted = details.muted;
    v.previews = details.previews;
    if v.length_ms == 0 {
        v.length_ms = u64::from(details.length_s) * 1_000;
    }
    v.menu_chapter = v.chapter_at(v.shown_ms()).unwrap_or(0);
}
