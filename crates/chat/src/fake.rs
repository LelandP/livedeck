//! A scripted chat for tests and `--mock`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use htpc_core::ChatMessage;
use tokio::sync::mpsc::UnboundedSender;

use crate::client::{ChatClient, ChatEvent, ChatSession};

/// Plays a fixed script into every channel that is joined, then optionally keeps adding lines.
#[derive(Clone, Default)]
pub struct FakeChat {
    script: Vec<ChatEvent>,
    /// Lines repeated for ever, one per interval, after the script.
    trickle: Option<(Vec<ChatMessage>, Duration)>,
    joined: Arc<Mutex<Vec<String>>>,
    alive: Arc<AtomicUsize>,
}

/// Counts a running session. Dropped with its task when the session is dropped.
struct Alive(Arc<AtomicUsize>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FakeChat {
    /// Every join plays `script` at once.
    pub fn new(script: Vec<ChatEvent>) -> Self {
        Self {
            script,
            ..Self::default()
        }
    }

    /// After the script, add the next of `lines` every `every`, going round for ever.
    pub fn trickling(mut self, lines: Vec<ChatMessage>, every: Duration) -> Self {
        self.trickle = Some((lines, every));
        self
    }

    /// The channels joined so far, in order.
    pub fn joined(&self) -> Vec<String> {
        self.joined
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Sessions that have been joined and not dropped.
    pub fn sessions_alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }
}

impl ChatClient for FakeChat {
    fn join(&self, channel: &str, events: UnboundedSender<ChatEvent>) -> ChatSession {
        self.joined
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(channel.to_owned());
        self.alive.fetch_add(1, Ordering::SeqCst);
        let guard = Alive(Arc::clone(&self.alive));
        let script = self.script.clone();
        let trickle = self.trickle.clone();
        ChatSession::new(tokio::spawn(async move {
            let _guard = guard;
            for event in script {
                if events.send(event).is_err() {
                    return;
                }
            }
            let Some((lines, every)) = trickle else {
                // A real session stays open: so does this one.
                std::future::pending::<()>().await;
                return;
            };
            if lines.is_empty() {
                std::future::pending::<()>().await;
            }
            for line in lines.into_iter().cycle() {
                tokio::time::sleep(every).await;
                if events.send(ChatEvent::Messages(vec![line])).is_err() {
                    return;
                }
            }
        }))
    }
}
