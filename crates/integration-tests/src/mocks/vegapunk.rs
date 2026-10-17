use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;
use auto_trader_core::vegapunk_port::{SearchHit, SearchMode, SearchResults, VegapunkApi};

use super::lock_mock_state;

/// Counters of the calls to each method of the mock, including the calls that fail.
#[derive(Debug, Default)]
pub struct CallCounters {
    /// Number of calls to `ingest_raw`.
    pub ingest_raw: AtomicU32,
    /// Number of calls to `search`.
    pub search: AtomicU32,
    /// Number of calls to `feedback`.
    pub feedback: AtomicU32,
    /// Number of calls to `merge`.
    pub merge: AtomicU32,
}

/// Arguments of an `ingest_raw` call.
#[derive(Debug, Clone)]
pub struct IngestRawCall {
    /// Text to ingest.
    pub text: String,
    /// Source type of the text.
    pub source_type: String,
    /// Channel that the text came from.
    pub channel: String,
    /// Timestamp of the text.
    pub timestamp: String,
}

/// Arguments of a `search` call.
#[derive(Debug, Clone)]
pub struct SearchCall {
    /// Query text.
    pub query: String,
    /// Search mode that the call passed.
    pub mode: String,
    /// Maximum number of results requested.
    pub top_k: i32,
}

/// Arguments of a `feedback` call.
#[derive(Debug, Clone)]
pub struct FeedbackCall {
    /// Id of the search that the feedback is about.
    pub search_id: String,
    /// Rating given to the search.
    pub rating: i32,
    /// Comment on the search.
    pub comment: String,
}

/// Mock of the Vegapunk client that starts no gRPC server, records the calls it receives and returns canned results or injected failures.
pub struct MockVegapunkApi {
    /// Counters of the calls to each method.
    pub counters: CallCounters,
    /// Whether every call fails while the flag is set.
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

/// Search result that the mock returns.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Text of the result.
    pub text: String,
    /// Score of the result.
    pub score: f32,
}

/// Builder of `MockVegapunkApi`.
pub struct MockVegapunkApiBuilder {
    search_results: Vec<SearchResult>,
    failures: Vec<(&'static str, u32)>,
}

impl MockVegapunkApiBuilder {
    /// Returns a builder whose mock has no search results and no injected failures.
    pub fn new() -> Self {
        Self {
            search_results: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// Sets the results that `search` returns.
    pub fn with_search_results(mut self, results: Vec<SearchResult>) -> Self {
        self.search_results = results;
        self
    }

    /// Makes the first `n` calls to `method` return an error; `method` must be `"ingest_raw"`, `"search"`, `"feedback"` or `"merge"`.
    pub fn with_failures(mut self, method: &'static str, n: u32) -> Self {
        self.failures.push((method, n));
        self
    }

    /// Builds the mock.
    ///
    /// # Panics
    ///
    /// Panics if the method name given to `with_failures` is not one of the four methods.
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
    /// Returns a mock that has no search results and no injected failures.
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

    /// Records the call and returns a result of one chunk.
    ///
    /// # Errors
    ///
    /// - Returns an error if `should_fail` is set.
    /// - Returns an error if a failure injected for `ingest_raw` remains.
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<IngestRawResult> {
        self.counters.ingest_raw.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.ingest_raw_failures)?;

        lock_mock_state(&self.ingest_raw_calls, "ingest_raw_calls")?.push(IngestRawCall {
            text: text.to_string(),
            source_type: source_type.to_string(),
            channel: channel.to_string(),
            timestamp: timestamp.to_string(),
        });

        Ok(IngestRawResult { chunk_count: 1 })
    }

    /// Records the call and returns the canned results, cut to the first `top_k` (none if `top_k` is negative), together with the number of this call to `search`.
    ///
    /// # Errors
    ///
    /// - Returns an error if `should_fail` is set.
    /// - Returns an error if a failure injected for `search` remains.
    /// - Returns an error if the lock on the recorded calls or on the canned results is poisoned.
    pub async fn search(
        &self,
        query: &str,
        mode: &str,
        top_k: i32,
    ) -> anyhow::Result<(Vec<SearchResult>, u32)> {
        let ordinal = self.counters.search.fetch_add(1, Ordering::SeqCst) + 1;
        Self::check_fail(&self.should_fail, &self.search_failures)?;

        lock_mock_state(&self.search_calls, "search_calls")?.push(SearchCall {
            query: query.to_string(),
            mode: mode.to_string(),
            top_k,
        });

        let mut results = lock_mock_state(&self.search_results, "search_results")?.clone();
        let limit = top_k_as_hard_cap_with_negative_as_zero(top_k);
        if limit < results.len() {
            results.truncate(limit);
        }
        Ok((results, ordinal))
    }

    /// Records the call.
    ///
    /// # Errors
    ///
    /// - Returns an error if `should_fail` is set.
    /// - Returns an error if a failure injected for `feedback` remains.
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub async fn feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        self.counters.feedback.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.feedback_failures)?;

        lock_mock_state(&self.feedback_calls, "feedback_calls")?.push(FeedbackCall {
            search_id: search_id.to_string(),
            rating,
            comment: comment.to_string(),
        });

        Ok(())
    }

    /// Records the call.
    ///
    /// # Errors
    ///
    /// - Returns an error if `should_fail` is set.
    /// - Returns an error if a failure injected for `merge` remains.
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub async fn merge(&self) -> anyhow::Result<()> {
        self.counters.merge.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.merge_failures)?;

        lock_mock_state(&self.merge_calls, "merge_calls")?.push(());

        Ok(())
    }

    /// Returns the `ingest_raw` calls that succeeded, in call order.
    ///
    /// # Errors
    ///
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub fn ingest_raw_calls(&self) -> anyhow::Result<Vec<IngestRawCall>> {
        Ok(lock_mock_state(&self.ingest_raw_calls, "ingest_raw_calls")?.clone())
    }

    /// Returns the `search` calls that succeeded, in call order.
    ///
    /// # Errors
    ///
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub fn search_calls(&self) -> anyhow::Result<Vec<SearchCall>> {
        Ok(lock_mock_state(&self.search_calls, "search_calls")?.clone())
    }

    /// Returns the `feedback` calls that succeeded, in call order.
    ///
    /// # Errors
    ///
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub fn feedback_calls(&self) -> anyhow::Result<Vec<FeedbackCall>> {
        Ok(lock_mock_state(&self.feedback_calls, "feedback_calls")?.clone())
    }

    /// Returns the number of `merge` calls that succeeded.
    ///
    /// # Errors
    ///
    /// - Returns an error if the lock on the recorded calls is poisoned.
    pub fn merge_call_count(&self) -> anyhow::Result<u32> {
        Ok(lock_mock_state(&self.merge_calls, "merge_calls")?.len() as u32)
    }
}

impl Default for MockVegapunkApi {
    fn default() -> Self {
        Self::new()
    }
}

/// Result that the mock returns from `ingest_raw`.
#[derive(Debug, Clone)]
pub struct IngestRawResult {
    /// Number of chunks that the mock reports for the text, which is always 1.
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
