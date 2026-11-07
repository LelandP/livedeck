//! The connection loop of one Kick chatroom: look the channel up, connect, subscribe, read,
//! batch, and start over with a growing pause whenever something fails.

use std::sync::Arc;

use htpc_core::{ChatMessage, ChatStatus};
use htpc_kick_api::{fallback_pusher_app, pusher_url, KickApi, PusherApp};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{interval, sleep, Instant, MissedTickBehavior};

use super::map::{read_frame, Frame};
use super::socket::{FrameConnector, FrameLink};
use crate::client::ChatEvent;
use crate::session::SessionConfig;

pub struct Ctx<K> {
    pub api: Arc<K>,
    pub connector: Arc<dyn FrameConnector>,
    pub slug: String,
    pub config: SessionConfig,
}

/// The sending side, which remembers the last status so it is not repeated.
struct Out {
    tx: UnboundedSender<ChatEvent>,
    last: Option<ChatStatus>,
}

impl Out {
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
    /// The connection is gone. `stable` if it had been live for a good while; `refused` if
    /// Pusher refused the app (its key may have changed).
    Dropped {
        stable: bool,
        refused: bool,
    },
    ConsumerGone,
}

/// Runs until the task is aborted (the session handle was dropped) or the receiver is gone.
pub async fn run<K: KickApi>(ctx: Ctx<K>, events: UnboundedSender<ChatEvent>) {
    let mut out = Out {
        tx: events,
        last: None,
    };
    if !out.status(ChatStatus::Connecting) {
        return;
    }
    let mut attempt: u32 = 0;
    let mut app: Option<PusherApp> = None;
    let mut chatroom: Option<(u64, u64)> = None;
    loop {
        // The channel's ids, once. A channel that does not exist has no chat to wait for.
        let (channel_id, room) = match chatroom {
            Some(ids) => ids,
            None => match ctx.api.channel(&ctx.slug).await {
                Ok(Some(info)) => {
                    chatroom = Some((info.id, info.chatroom_id));
                    (info.id, info.chatroom_id)
                }
                Ok(None) => {
                    tracing::warn!(slug = %ctx.slug, "no such Kick channel, no chat");
                    out.send(ChatEvent::Messages(vec![ChatMessage::notice(
                        "Chat is not available for this channel.",
                    )]));
                    return;
                }
                Err(err) => {
                    tracing::info!(slug = %ctx.slug, %err, "could not look up the Kick chatroom");
                    attempt += 1;
                    if !retry(&ctx, &mut out, attempt).await {
                        return;
                    }
                    continue;
                }
            },
        };
        let current = match &app {
            Some(app) => app.clone(),
            None => {
                let asked = ctx.api.chat_app(channel_id).await.unwrap_or_else(|err| {
                    tracing::info!(%err, "Kick did not name its chat app, using the known one");
                    fallback_pusher_app()
                });
                app = Some(asked.clone());
                asked
            }
        };
        match ctx
            .connector
            .connect(&pusher_url(&current.key, &current.cluster))
            .await
        {
            Ok(link) => match drive(link, room, &ctx, &mut out).await {
                Ended::ConsumerGone => return,
                Ended::Dropped { stable, refused } => {
                    if stable {
                        attempt = 0;
                    }
                    if refused {
                        // Ask Kick again next time.
                        app = None;
                    }
                }
            },
            Err(err) => tracing::info!(slug = %ctx.slug, %err, "Kick chat connection failed"),
        }
        attempt += 1;
        if !retry(&ctx, &mut out, attempt).await {
            return;
        }
    }
}

/// Says "reconnecting" and waits the backoff. False once nobody listens.
async fn retry<K>(ctx: &Ctx<K>, out: &mut Out, attempt: u32) -> bool {
    if !out.status(ChatStatus::Reconnecting) {
        return false;
    }
    sleep(ctx.config.backoff.delay(attempt)).await;
    true
}

/// Reads one connection until it ends.
async fn drive<K>(mut link: FrameLink, room: u64, ctx: &Ctx<K>, out: &mut Out) -> Ended {
    let subscribe = serde_json::json!({
        "event": "pusher:subscribe",
        "data": { "auth": "", "channel": format!("chatrooms.{room}.v2") },
    })
    .to_string();
    if link.outgoing.send(subscribe).is_err() {
        return Ended::Dropped {
            stable: false,
            refused: false,
        };
    }
    let started = Instant::now();
    let mut heard = Instant::now();
    let mut batch: Vec<ChatMessage> = Vec::new();
    let mut flush = interval(ctx.config.flush_every);
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut keepalive = interval(ctx.config.keepalive);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    keepalive.tick().await;
    let ended = |refused: bool| Ended::Dropped {
        stable: started.elapsed() >= ctx.config.stable_after,
        refused,
    };
    loop {
        tokio::select! {
            frame = link.incoming.recv() => {
                let Some(Ok(text)) = frame else {
                    return ended(false);
                };
                heard = Instant::now();
                match read_frame(&text) {
                    Frame::Subscribed => {
                        tracing::info!(slug = %ctx.slug, "Kick chat live");
                        if !out.status(ChatStatus::Live) {
                            return Ended::ConsumerGone;
                        }
                    }
                    Frame::Ping => {
                        let _ = link.outgoing.send(r#"{"event":"pusher:pong","data":{}}"#.to_owned());
                    }
                    Frame::Message(line) => batch.push(line),
                    Frame::Moderation(action) => {
                        // Lines already batched go first, so a delete finds its line.
                        if !send_batch(out, &mut batch) || !out.send(ChatEvent::Moderated(action)) {
                            return Ended::ConsumerGone;
                        }
                    }
                    Frame::Error(message) => {
                        tracing::warn!(slug = %ctx.slug, %message, "Kick chat refused");
                        return ended(true);
                    }
                    Frame::Connected | Frame::Pong | Frame::Other => {}
                }
            }
            _ = flush.tick() => {
                if !send_batch(out, &mut batch) {
                    return Ended::ConsumerGone;
                }
            }
            _ = keepalive.tick() => {
                if heard.elapsed() >= ctx.config.idle_timeout {
                    tracing::info!(slug = %ctx.slug, "Kick chat went quiet, reconnecting");
                    return ended(false);
                }
                let _ = link.outgoing.send(r#"{"event":"pusher:ping","data":{}}"#.to_owned());
            }
        }
    }
}

/// Hands on the batched lines, if any. False once nobody listens.
fn send_batch(out: &mut Out, batch: &mut Vec<ChatMessage>) -> bool {
    if batch.is_empty() {
        return true;
    }
    out.send(ChatEvent::Messages(std::mem::take(batch)))
}

#[cfg(test)]
mod tests;
