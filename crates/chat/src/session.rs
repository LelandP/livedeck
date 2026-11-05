//! The connection loop of one joined channel: connect, say hello, read, batch, and start over
//! with a growing pause whenever the connection fails.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use htpc_core::{ChatMessage, ChatStatus};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{interval, sleep, sleep_until, timeout, Instant, MissedTickBehavior};
use twitch_irc::message::{IRCMessage, ServerMessage};

use crate::badges::{BadgeBook, HttpBadges};
use crate::client::ChatEvent;
use crate::link::{Connector, Link};
use crate::map::{Mapped, Mapper};

/// Pause between connection attempts: `initial`, doubling up to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Backoff {
    /// The pause before attempt number `attempt` (1 is the first retry).
    pub fn delay(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        self.initial.saturating_mul(1 << doublings).min(self.max)
    }
}

/// Timing of a chat connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    pub backoff: Backoff,
    /// How often we ping the server, so a dead connection shows up (sleep, lost Wi-Fi).
    pub keepalive: Duration,
    /// Nothing at all heard for this long (the server answers our pings) means it is dead.
    pub idle_timeout: Duration,
    /// Lines are handed on in batches this far apart, so a busy chat is a few updates a second
    /// and not hundreds.
    pub flush_every: Duration,
    /// A connection must have been live this long before the next failure starts the backoff
    /// over. Without it a server that accepts and then drops us would be hammered every second.
    pub stable_after: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            backoff: Backoff {
                initial: Duration::from_secs(1),
                max: Duration::from_secs(30),
            },
            keepalive: Duration::from_secs(45),
            idle_timeout: Duration::from_secs(120),
            flush_every: Duration::from_millis(150),
            stable_after: Duration::from_secs(30),
        }
    }
}

/// Everything a session needs.
pub struct Ctx {
    pub connector: Arc<dyn Connector>,
    /// Lower-case login.
    pub channel: String,
    pub mapper: Mapper,
    pub badges: Arc<BadgeBook>,
    pub loader: Option<Arc<HttpBadges>>,
    pub config: SessionConfig,
}

/// The sending side of the session, which remembers the last status so it is not repeated.
struct Out {
    tx: UnboundedSender<ChatEvent>,
    last: Option<ChatStatus>,
}

impl Out {
    /// False once the receiver is gone: nobody is interested any more.
    fn send(&self, event: ChatEvent) -> bool {
        self.tx.send(event).is_ok()
    }

    fn status(&mut self, status: ChatStatus) -> bool {
        if self.last == Some(status) {
            return true;
        }
        self.last = Some(status);
        self.send(ChatEvent::Status(status))
    }
}

enum Ended {
    /// The connection is gone. `stable` if it had been live for a good while.
    Dropped { stable: bool },
    /// Nobody listens any more.
    ConsumerGone,
}

/// Runs until the task is aborted (the session handle was dropped) or the receiver is gone.
pub async fn run(ctx: Ctx, events: UnboundedSender<ChatEvent>) {
    let mut out = Out {
        tx: events,
        last: None,
    };
    if twitch_irc::validate::validate_login(&ctx.channel).is_err() {
        tracing::warn!(channel = %ctx.channel, "not a valid channel name, no chat");
        out.send(ChatEvent::Messages(vec![ChatMessage::notice(
            "Chat is not available for this channel.",
        )]));
        return;
    }
    if !out.status(ChatStatus::Connecting) {
        return;
    }
    // The badge images are fetched while the first connection is made, so the first lines already
    // have their badges. Reconnects keep what was loaded.
    let (first, ()) = tokio::join!(ctx.connector.connect(), load_badges(&ctx));
    let mut first = Some(first);
    let mut attempt: u32 = 0;
    loop {
        let connected = match first.take() {
            Some(result) => result,
            None => ctx.connector.connect().await,
        };
        match connected {
            Ok(link) => {
                match drive(link, &ctx, &mut out).await {
                    Ended::ConsumerGone => return,
                    Ended::Dropped { stable } => {
                        if stable {
                            attempt = 0;
                        }
                    }
                }
                tracing::info!(channel = %ctx.channel, "chat connection lost");
            }
            Err(err) => tracing::warn!(channel = %ctx.channel, %err, "chat connection failed"),
        }
        attempt += 1;
        if !out.status(ChatStatus::Reconnecting) {
            return;
        }
        let pause = ctx.config.backoff.delay(attempt);
        tracing::debug!(?pause, attempt, "reconnecting chat");
        sleep(pause).await;
    }
}

/// How long the first connection waits for the badge images. Slow badges must not hold chat up:
/// the lines then show without them, and the images still arrive for the lines that follow.
const BADGE_WAIT: Duration = Duration::from_secs(3);

/// Loads the global and the channel's badges into the book, waiting for them for at most
/// [`BADGE_WAIT`] (the loading carries on in the background after that).
async fn load_badges(ctx: &Ctx) {
    let Some(loader) = ctx.loader.clone() else {
        return;
    };
    let badges = Arc::clone(&ctx.badges);
    let channel = ctx.channel.clone();
    let mut task = tokio::spawn(async move {
        let (global, own) = tokio::join!(loader.global(), loader.channel(&channel));
        match global {
            Ok(set) => badges.set_global(set),
            Err(err) => tracing::warn!(%err, "global badges unavailable"),
        }
        match own {
            Ok(set) => badges.set_channel(set),
            Err(err) => tracing::warn!(%err, "channel badges unavailable"),
        }
    });
    if timeout(BADGE_WAIT, &mut task).await.is_err() {
        tracing::debug!("badges are slow, chat starts without them");
    }
}

fn message(command: &str, params: &[&str]) -> IRCMessage {
    IRCMessage::new_simple(
        command.to_owned(),
        params.iter().map(|p| (*p).to_owned()).collect(),
    )
}

/// An anonymous name the server accepts without a password (`justinfan` plus digits).
fn anonymous_nick() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("justinfan{}", 10_000 + nanos % 80_000)
}

/// What to do after one server message.
enum Flow {
    Continue,
    /// The server asked us to reconnect.
    Reconnect,
    Gone,
}

struct Driver<'a> {
    ctx: &'a Ctx,
    out: &'a mut Out,
    outgoing: &'a tokio::sync::mpsc::UnboundedSender<IRCMessage>,
    pending: Vec<ChatMessage>,
    live_since: Option<Instant>,
}

impl Driver<'_> {
    fn mark_live(&mut self) -> bool {
        if self.live_since.is_none() {
            self.live_since = Some(Instant::now());
        }
        self.out.status(ChatStatus::Live)
    }

    /// Hands the collected lines on. False once nobody listens.
    fn flush(&mut self) -> bool {
        if self.pending.is_empty() {
            return true;
        }
        let lines = std::mem::take(&mut self.pending);
        self.out.send(ChatEvent::Messages(lines))
    }

    fn on_message(&mut self, irc: IRCMessage) -> Flow {
        let msg = match ServerMessage::try_from(irc) {
            Ok(msg) => msg,
            Err(err) => {
                tracing::trace!(%err, "unreadable chat message");
                return Flow::Continue;
            }
        };
        match &msg {
            ServerMessage::Ping(ping) => {
                let params: Vec<String> = ping.source.params.clone();
                let pong = IRCMessage::new_simple("PONG".to_owned(), params);
                // A closed link shows up as the end of `incoming`.
                let _ = self.outgoing.send(pong);
                return Flow::Continue;
            }
            ServerMessage::Reconnect(_) => return Flow::Reconnect,
            // The server confirms our join with a JOIN, and sends the room's settings after it.
            ServerMessage::Join(_) | ServerMessage::RoomState(_) if !self.mark_live() => {
                return Flow::Gone;
            }
            _ => {}
        }
        for mapped in self.ctx.mapper.map(msg) {
            match mapped {
                Mapped::Line(line) => {
                    if !self.mark_live() {
                        return Flow::Gone;
                    }
                    self.pending.push(line);
                }
                Mapped::Moderation(action) => {
                    // The lines before the ban must exist before the ban can remove them.
                    if !self.flush() || !self.out.send(ChatEvent::Moderated(action)) {
                        return Flow::Gone;
                    }
                }
            }
        }
        Flow::Continue
    }
}

/// Talks over one connection until it breaks.
async fn drive(mut link: Link, ctx: &Ctx, out: &mut Out) -> Ended {
    let cfg = ctx.config;
    let hello = [
        message("CAP", &["REQ", "twitch.tv/tags twitch.tv/commands"]),
        message("NICK", &[&anonymous_nick()]),
        message("JOIN", &[&format!("#{}", ctx.channel)]),
    ];
    for line in hello {
        if link.outgoing.send(line).is_err() {
            return Ended::Dropped { stable: false };
        }
    }

    let outgoing = link.outgoing.clone();
    let mut driver = Driver {
        ctx,
        out,
        outgoing: &outgoing,
        pending: Vec::new(),
        live_since: None,
    };
    let mut flush = interval(cfg.flush_every);
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut keepalive = interval(cfg.keepalive);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick is immediate. The hello is the first thing the server hears.
    keepalive.tick().await;
    let mut last_heard = Instant::now();

    loop {
        tokio::select! {
            item = link.incoming.recv() => match item {
                None => break,
                Some(Err(err)) => {
                    tracing::debug!(%err, "chat connection error");
                    break;
                }
                Some(Ok(irc)) => {
                    last_heard = Instant::now();
                    match driver.on_message(irc) {
                        Flow::Continue => {}
                        Flow::Reconnect => break,
                        Flow::Gone => return Ended::ConsumerGone,
                    }
                }
            },
            _ = flush.tick() => {
                if !driver.flush() {
                    return Ended::ConsumerGone;
                }
            }
            _ = keepalive.tick() => {
                if link.outgoing.send(message("PING", &["tmi.twitch.tv"])).is_err() {
                    break;
                }
            }
            () = sleep_until(last_heard + cfg.idle_timeout) => {
                tracing::warn!(channel = %ctx.channel, "chat went silent, reconnecting");
                break;
            }
        }
    }
    // Whatever arrived before the break still counts.
    if !driver.flush() {
        return Ended::ConsumerGone;
    }
    let stable = driver
        .live_since
        .is_some_and(|since| since.elapsed() >= cfg.stable_after);
    Ended::Dropped { stable }
}

#[cfg(test)]
mod tests;
