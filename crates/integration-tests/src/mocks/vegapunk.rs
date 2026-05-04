// TODO(Phase 4): Introduce a VegapunkApi trait to replace direct VegapunkClient usage,
// enabling proper mock injection in production code paths.

//! Call-tracking mock for the Vegapunk GraphRAG client.
//!
//! The real [`VegapunkClient`] uses tonic gRPC stubs generated with
//! `build_server(false)`, so no server trait exists in the crate.
//! Spinning up a full tonic server from the proto adds significant
//! complexity with minimal test value for Phase 3 integration tests.
//!
//! Instead, this module provides [`MockVegapunk`] — a lightweight struct
//! that mirrors the four methods used by auto-trader (`ingest_raw`,
//! `search`, `feedback`, `merge`) and tracks every call with atomic
//! counters plus captured arguments. Failure injection is supported
//! via [`MockVegapunkBuilder::with_failures`].

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// CallCounters
// ---------------------------------------------------------------------------

/// Per-method call counters.
#[derive(Debug, Default)]
pub struct CallCounters {
    pub ingest_raw: AtomicU32,
    pub search: AtomicU32,
    pub feedback: AtomicU32,
    pub merge: AtomicU32,
}

// ---------------------------------------------------------------------------
// Captured arguments
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// MockVegapunk
// ---------------------------------------------------------------------------

/// A call-tracking mock that replaces [`VegapunkClient`] in integration tests.
///
/// Does **not** start a gRPC server. Instead it provides async methods with
/// the same signatures as the real client, records every invocation, and
/// returns canned responses (or errors when failure injection is active).
pub struct MockVegapunk {
    pub counters: CallCounters,
    pub should_fail: AtomicBool,

    // Captured calls for assertion
    ingest_raw_calls: Mutex<Vec<IngestRawCall>>,
    search_calls: Mutex<Vec<SearchCall>>,
    feedback_calls: Mutex<Vec<FeedbackCall>>,
    merge_calls: Mutex<Vec<()>>,

    // Per-method remaining failure counts
    ingest_raw_failures: AtomicU32,
    search_failures: AtomicU32,
    feedback_failures: AtomicU32,
    merge_failures: AtomicU32,

    // Canned search results
    search_results: Mutex<Vec<SearchResult>>,
}

/// A simplified search result item returned by the mock.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub text: String,
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`MockVegapunk`] with fluent configuration.
pub struct MockVegapunkBuilder {
    search_results: Vec<SearchResult>,
    failures: Vec<(&'static str, u32)>,
}

impl MockVegapunkBuilder {
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

    pub fn build(self) -> MockVegapunk {
        let mock = MockVegapunk {
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
                other => panic!("MockVegapunk: unknown method '{other}'"),
            }
        }

        mock
    }
}

impl Default for MockVegapunkBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Mock method implementations
// ---------------------------------------------------------------------------

impl MockVegapunk {
    /// Shorthand: build a default mock with no failure injection.
    pub fn new() -> Self {
        MockVegapunkBuilder::new().build()
    }

    // -- helpers --

    fn check_fail(global: &AtomicBool, per_method: &AtomicU32) -> Result<(), anyhow::Error> {
        if global.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("MockVegapunk: global failure enabled"));
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
                        "MockVegapunk: injected failure ({} remaining)",
                        current - 1
                    ));
                }
                Err(_) => continue, // retry
            }
        }
    }

    // -- public API (mirrors VegapunkClient) --

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

    pub async fn search(
        &self,
        query: &str,
        mode: &str,
        top_k: i32,
    ) -> anyhow::Result<Vec<SearchResult>> {
        self.counters.search.fetch_add(1, Ordering::SeqCst);
        Self::check_fail(&self.should_fail, &self.search_failures)?;

        self.search_calls.lock().unwrap().push(SearchCall {
            query: query.to_string(),
            mode: mode.to_string(),
            top_k,
        });

        let results = self.search_results.lock().unwrap().clone();
        Ok(results)
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

    // -- accessors for captured calls --

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

impl Default for MockVegapunk {
    fn default() -> Self {
        Self::new()
    }
}

/// Canned response from `ingest_raw`.
#[derive(Debug, Clone)]
pub struct IngestRawResult {
    pub chunk_count: i32,
}
