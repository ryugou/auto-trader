use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;
use auto_trader_core::vegapunk_port::{SearchHit, SearchMode, SearchResults, VegapunkApi};

/// Per-method call counters.
#[derive(Debug, Default)]
pub struct CallCounters {
    pub ingest_raw: AtomicU32,
    pub search: AtomicU32,
    pub feedback: AtomicU32,
    pub merge: AtomicU32,
}

/// Arguments captured from an `ingest_raw` call.
#[derive(Debug, Clone)]
pub struct IngestRawCall {
    pub text: String,
    pub source_type: String,
    pub channel: String,
    pub timestamp: String,
}

/// Arguments captured from a `search` call.
#[derive(Debug, Clone)]
pub struct SearchCall {
    pub query: String,
    pub mode: String,
    pub top_k: i32,
}

/// Arguments captured from a `feedback` call.
#[derive(Debug, Clone)]
pub struct FeedbackCall {
    pub search_id: String,
    pub rating: i32,
    pub comment: String,
}

/// A call-tracking mock that replaces [`VegapunkClient`] in integration tests.
///
/// Does **not** start a gRPC server. Instead it provides async methods with
/// the same signatures as the real client, records every invocation, and
/// returns canned responses (or errors when failure injection is active).
pub struct MockVegapunkApi {
    pub counters: CallCounters,
    pub should_fail: AtomicBool,

    ingest_raw_calls: Mutex<Vec<IngestRawCall>>,
    search_calls: Mutex<Vec<SearchCall>>,
    feedback_calls: Mutex<Vec<FeedbackCall>>,
    merge_calls: Mutex<Vec<()>>,

    ingest_raw_failures: AtomicU32,
    search_failures: AtomicU32,
    feedback_failures: AtomicU32,
    merge_failures: AtomicU32,

    search_results: Mutex<Vec<SearchResult>>,
}

/// A simplified search result item returned by the mock.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub text: String,
    pub score: f32,
}

/// Builder for [`MockVegapunkApi`] with fluent configuration.
pub struct MockVegapunkApiBuilder {
    search_results: Vec<SearchResult>,
    failures: Vec<(&'static str, u32)>,
}

impl MockVegapunkApiBuilder {
    pub fn new() -> Self {
        Self {
            search_results: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// Pre-load search results returned by `search()`.
    pub fn with_search_results(mut self, results: Vec<SearchResult>) -> Self {
        self.search_results = results;
        self
    }

    /// Make the first `n` calls to `method` return an error.
    ///
    /// Supported method names: `"ingest_raw"`, `"search"`, `"feedback"`, `"merge"`.
    pub fn with_failures(mut self, method: &'static str, n: u32) -> Self {
        self.failures.push((method, n));
        self
    }

    pub fn build(self) -> MockVegapunkApi {
        let mock = MockVegapunkApi {
            counters: CallCounters::default(),
            should_fail: AtomicBool::new(false),
            ingest_raw_calls: Mutex::new(Vec::new()),
            search_calls: Mutex::new(Vec::new()),
            feedback_calls: Mutex::new(Vec::new()),
            merge_calls: Mutex::new(Vec::new()),
            ingest_raw_failures: AtomicU32::new(0),
            search_failures: AtomicU32::new(0),
            feedback_failures: AtomicU32::new(0),
            merge_failures: AtomicU32::new(0),
            search_results: Mutex::new(self.search_results),
        };

        for (method, n) in &self.failures {
            match *method {
                "ingest_raw" => mock.ingest_raw_failures.store(*n, Ordering::SeqCst),
                "search" => mock.search_failures.store(*n, Ordering::SeqCst),
                "feedback" => mock.feedback_failures.store(*n, Ordering::SeqCst),
                "merge" => mock.merge_failures.store(*n, Ordering::SeqCst),
                other => panic!("unsupported method: \"{other}\""),
            }
        }

        mock
    }
}

impl Default for MockVegapunkApiBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MockVegapunkApi {
    /// Shorthand: build a default mock with no failure injection.
    pub fn new() -> Self {
        MockVegapunkApiBuilder::new().build()
    }

    fn check_fail(global: &AtomicBool, per_method: &AtomicU32) -> Result<(), anyhow::Error> {
        if global.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!(
                "failed to call vegapunk: global failure enabled"
            ));
        }
        loop {
            let current = per_method.load(Ordering::SeqCst);
            if current == 0 {
                return Ok(());
            }
            match per_method.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return Err(anyhow::anyhow!(
                        "failed to call vegapunk: injected failure ({} remaining)",
                        current - 1
                    ));
                }
                Err(_) => continue,
            }
        }
    }

    pub async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<IngestRawResult> {
        self.counters.ingest_raw.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.ingest_raw_failures)?;

        self.ingest_raw_calls.lock().unwrap().push(IngestRawCall {
            text: text.to_string(),
            source_type: source_type.to_string(),
            channel: channel.to_string(),
            timestamp: timestamp.to_string(),
        });

        Ok(IngestRawResult { chunk_count: 1 })
    }

    /// Inherent search returns canned results truncated to `top_k`. Also
    /// returns the ordinal from the call counter so the trait impl can
    /// build a unique search_id atomically.
    pub async fn search(
        &self,
        query: &str,
        mode: &str,
        top_k: i32,
    ) -> anyhow::Result<(Vec<SearchResult>, u32)> {
        let ordinal = self.counters.search.fetch_add(1, Ordering::SeqCst) + 1;
        Self::check_fail(&self.should_fail, &self.search_failures)?;

        self.search_calls.lock().unwrap().push(SearchCall {
            query: query.to_string(),
            mode: mode.to_string(),
            top_k,
        });

        let mut results = self.search_results.lock().unwrap().clone();
        let limit = top_k_as_hard_cap_with_negative_as_zero(top_k);
        if limit < results.len() {
            results.truncate(limit);
        }
        Ok((results, ordinal))
    }

    pub async fn feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        self.counters.feedback.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.feedback_failures)?;

        self.feedback_calls.lock().unwrap().push(FeedbackCall {
            search_id: search_id.to_string(),
            rating,
            comment: comment.to_string(),
        });

        Ok(())
    }

    pub async fn merge(&self) -> anyhow::Result<()> {
        self.counters.merge.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.merge_failures)?;

        self.merge_calls.lock().unwrap().push(());

        Ok(())
    }

    pub fn ingest_raw_calls(&self) -> Vec<IngestRawCall> {
        self.ingest_raw_calls.lock().unwrap().clone()
    }

    pub fn search_calls(&self) -> Vec<SearchCall> {
        self.search_calls.lock().unwrap().clone()
    }

    pub fn feedback_calls(&self) -> Vec<FeedbackCall> {
        self.feedback_calls.lock().unwrap().clone()
    }

    pub fn merge_call_count(&self) -> u32 {
        self.merge_calls.lock().unwrap().len() as u32
    }
}

impl Default for MockVegapunkApi {
    fn default() -> Self {
        Self::new()
    }
}

/// Canned response from `ingest_raw`.
#[derive(Debug, Clone)]
pub struct IngestRawResult {
    pub chunk_count: i32,
}

#[async_trait]
impl VegapunkApi for MockVegapunkApi {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        MockVegapunkApi::ingest_raw(self, text, source_type, channel, timestamp).await?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults> {
        let (results, ordinal) = MockVegapunkApi::search(self, query, mode.as_str(), top_k).await?;
        let hits: Vec<SearchHit> = results
            .into_iter()
            .map(|r| SearchHit {
                text: r.text,
                score: r.score,
            })
            .collect();
        Ok(SearchResults {
            hits,
            search_id: format!("mock-search-{ordinal}"),
        })
    }

    async fn feedback(&self, search_id: &str, rating: i32, comment: &str) -> anyhow::Result<()> {
        MockVegapunkApi::feedback(self, search_id, rating, comment).await
    }

    async fn merge(&self) -> anyhow::Result<()> {
        MockVegapunkApi::merge(self).await
    }
}

fn top_k_as_hard_cap_with_negative_as_zero(top_k: i32) -> usize {
    top_k.max(0) as usize
}

#[cfg(test)]
mod tests;
