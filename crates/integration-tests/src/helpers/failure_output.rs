use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::Subscriber;
use tracing_subscriber::Layer;

// ---------------------------------------------------------------------------
// TracingCapture — in-memory log capture layer
// ---------------------------------------------------------------------------

/// A [`tracing_subscriber::Layer`] that appends formatted log lines
/// (`"{LEVEL} {message}"`) to a shared buffer.
///
/// Usage:
/// ```ignore
/// let (layer, buffer) = TracingCapture::new();
/// let _guard = tracing_subscriber::registry().with(layer).set_default();
/// tracing::info!("hello");
/// let logs = buffer.lock().unwrap();
/// assert!(logs[0].contains("hello"));
/// ```
pub struct TracingCapture {
    buffer: Arc<Mutex<Vec<String>>>,
}

impl TracingCapture {
    /// Create a new capture layer together with a handle to the shared buffer.
    pub fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                buffer: Arc::clone(&buffer),
            },
            buffer,
        )
    }
}

impl<S> Layer<S> for TracingCapture
where
    S: Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Extract message field from the event.
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let level = event.metadata().level();
        let line = format!("{level} {}", visitor.0);

        if let Ok(mut buf) = self.buffer.lock() {
            buf.push(line);
        }
    }
}

/// Simple field visitor that collects the `message` field.
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }
}

// ---------------------------------------------------------------------------
// format_failure — structured failure report
// ---------------------------------------------------------------------------

/// Build a human-readable (and future-LLM-parseable) failure report.
///
/// Includes application logs, DB state snapshot, and the git diff of the
/// last commit so that vibepod / auto-fix agents have full context.
pub fn format_failure(
    test_name: &str,
    fixture: &str,
    expected: &str,
    actual: &str,
    logs: &[String],
    db_snapshot: &str,
) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "[FAIL] {test_name}");
    let _ = writeln!(out, "  fixture: {fixture}");
    let _ = writeln!(out, "  expected: {expected}");
    let _ = writeln!(out, "  actual: {actual}");
    let _ = writeln!(out);

    // --- application log ---
    let _ = writeln!(out, "  === application log ===");
    for line in logs {
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out);

    // --- db state ---
    let _ = writeln!(out, "  === db state ===");
    for line in db_snapshot.lines() {
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out);

    // --- git diff ---
    let _ = writeln!(out, "  === git diff (last 1 commit) ===");
    match std::process::Command::new("git")
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
    {
        Ok(output) => {
            let diff = String::from_utf8_lossy(&output.stdout);
            for line in diff.lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
        Err(e) => {
            let _ = writeln!(out, "  (git diff failed: {e})");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_failure_contains_all_sections() {
        let logs = vec!["INFO starting".to_string(), "ERROR boom".to_string()];
        let report = format_failure(
            "my_test",
            "smoke.csv",
            "balance=100",
            "balance=90",
            &logs,
            "trading_accounts: 1 row",
        );

        assert!(report.contains("[FAIL] my_test"));
        assert!(report.contains("fixture: smoke.csv"));
        assert!(report.contains("expected: balance=100"));
        assert!(report.contains("actual: balance=90"));
        assert!(report.contains("=== application log ==="));
        assert!(report.contains("INFO starting"));
        assert!(report.contains("ERROR boom"));
        assert!(report.contains("=== db state ==="));
        assert!(report.contains("trading_accounts: 1 row"));
        assert!(report.contains("=== git diff (last 1 commit) ==="));
    }
}
