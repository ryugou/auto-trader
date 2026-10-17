#![doc = "Fetches FX-related news, summarizes it with Gemini and broadcasts the summaries as macro updates."]

/// Analyst that fetches news, summarizes it and publishes macro updates.
pub mod analyst;
/// Economic calendar events.
pub mod calendar;
mod http_client;
/// News feed fetching.
pub mod news;
/// Gemini-based summarizing of news text.
pub mod summarizer;
