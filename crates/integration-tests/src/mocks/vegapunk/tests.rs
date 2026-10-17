use std::sync::Arc;

use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};

use super::*;

fn build_mock_with_one_search_result() -> MockVegapunkApi {
    MockVegapunkApiBuilder::new()
        .with_search_results(vec![SearchResult {
            text: "hi".into(),
            score: 0.9,
        }])
        .build()
}

#[tokio::test]
async fn mock_implements_vegapunk_api() {
    let mock: Arc<dyn VegapunkApi> = Arc::new(build_mock_with_one_search_result());
    mock.ingest_raw(
        "t",
        "trade_signal",
        "usd_jpy-trades",
        "2026-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    let r = mock.search("q", SearchMode::Local, 5).await.unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].text, "hi");
    mock.feedback("sid", 5, "ok").await.unwrap();
    mock.merge().await.unwrap();
}

#[tokio::test]
async fn search_returns_no_results_for_negative_top_k() {
    let mock = build_mock_with_one_search_result();
    let (results, _) = mock.search("q", "local", -1).await.unwrap();
    assert!(results.is_empty());
}
