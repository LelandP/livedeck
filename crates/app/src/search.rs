//! Runs `Command::Search` once typing pauses.
//!
//! The reducer sends a search on every change of the text and has no clock, so the waiting
//! happens here: each search waits [`SEARCH_DEBOUNCE`] before it asks Twitch, and a newer one
//! aborts the one before it (waiting or already asking). A text too short to search only aborts.
//! The reducer also drops answers with an old id, so an answer that slips through is harmless.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use htpc_core::{Command, Msg, SearchState};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::provider::Provider;

/// How long typing must pause before a search goes out.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct Searcher {
    pending: Mutex<Option<JoinHandle<()>>>,
}

impl Searcher {
    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(cmd, Command::Search { .. })
    }

    /// Replaces the pending search with `cmd`. Must be called inside the tokio runtime.
    pub fn run<P: Provider>(&self, provider: Arc<P>, tx: UnboundedSender<Msg>, cmd: Command) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = pending.take() {
            task.abort();
        }
        let Command::Search { query, .. } = &cmd else {
            return;
        };
        if !SearchState::is_searchable(query) {
            return;
        }
        *pending = Some(tokio::spawn(async move {
            tokio::time::sleep(SEARCH_DEBOUNCE).await;
            for event in provider.execute(cmd).await {
                if tx.send(event.into()).is_err() {
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_core::Event;
    use htpc_ui::Thumbnail;
    use tokio::sync::mpsc::unbounded_channel;

    /// Answers every search with no rows and remembers the texts it was asked for.
    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    impl Provider for Recording {
        async fn execute(&self, cmd: Command) -> Vec<Event> {
            match cmd {
                Command::Search { id, query } => {
                    self.0.lock().unwrap().push(query);
                    vec![Event::SearchResults {
                        id,
                        rows: Vec::new(),
                    }]
                }
                _ => Vec::new(),
            }
        }

        async fn thumbnail(&self, _url: &str) -> Option<Thumbnail> {
            None
        }
    }

    fn search(id: u32, query: &str) -> Command {
        Command::Search {
            id,
            query: query.into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn three_fast_edits_make_one_request() {
        let provider = Arc::new(Recording::default());
        let (tx, mut rx) = unbounded_channel();
        let searcher = Searcher::default();
        for (id, text) in [(1, "ab"), (2, "abc"), (3, "abcd")] {
            searcher.run(Arc::clone(&provider), tx.clone(), search(id, text));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(SEARCH_DEBOUNCE).await;
        assert_eq!(*provider.0.lock().unwrap(), ["abcd"]);
        let Ok(Msg::Event(Event::SearchResults { id, .. })) = rx.try_recv() else {
            panic!("the answer was not sent");
        };
        assert_eq!(id, 3);
        assert!(rx.try_recv().is_err(), "only one answer");
    }

    #[tokio::test(start_paused = true)]
    async fn a_short_text_cancels_the_pending_search() {
        let provider = Arc::new(Recording::default());
        let (tx, mut rx) = unbounded_channel();
        let searcher = Searcher::default();
        searcher.run(Arc::clone(&provider), tx.clone(), search(1, "ab"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        searcher.run(Arc::clone(&provider), tx.clone(), search(2, "a"));
        tokio::time::sleep(SEARCH_DEBOUNCE * 3).await;
        assert!(provider.0.lock().unwrap().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn searches_that_pause_each_go_out() {
        let provider = Arc::new(Recording::default());
        let (tx, _rx) = unbounded_channel();
        let searcher = Searcher::default();
        searcher.run(Arc::clone(&provider), tx.clone(), search(1, "ab"));
        tokio::time::sleep(SEARCH_DEBOUNCE * 2).await;
        searcher.run(Arc::clone(&provider), tx.clone(), search(2, "xy"));
        tokio::time::sleep(SEARCH_DEBOUNCE * 2).await;
        assert_eq!(*provider.0.lock().unwrap(), ["ab", "xy"]);
    }

    #[test]
    fn handles_only_search() {
        assert!(Searcher::handles(&search(1, "ab")));
        assert!(!Searcher::handles(&Command::StopPlayback { slot: 0 }));
    }
}
