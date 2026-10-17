use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::Subscriber;
use tracing_subscriber::Layer;

/// Tracing layer that appends a line of the form `{LEVEL} {message}` for each event to a shared buffer.
pub struct TracingCapture {
    buffer: Arc<Mutex<Vec<String>>>,
}

impl TracingCapture {
    /// Returns a new capture layer together with a handle to its buffer.
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
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let level = event.metadata().level();
        let line = format!("{level} {}", visitor.0);

        if let Ok(mut buf) = self.buffer.lock() {
            buf.push(line);
        }
    }
}

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

/// Returns a failure report made of the test name, the fixture, the expected and actual values, the application logs, the database snapshot and the output of `git diff HEAD~1 HEAD`.
///
/// If running `git diff` fails, the report holds a line that states the failure in place of the diff.
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

    let _ = writeln!(out, "  === application log ===");
    for line in logs {
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "  === db state ===");
    for line in db_snapshot.lines() {
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "  === git diff (last 1 commit) ===");
    match std::process::Command::new("git")
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let diff = String::from_utf8_lossy(&output.stdout);
            for line in diff.lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let _ = writeln!(
                out,
                "  (failed to run git diff: exit code {}, stderr: {})",
                output.status,
                stderr.trim()
            );
        }
        Err(e) => {
            let _ = writeln!(out, "  (failed to run git diff: {e})");
        }
    }

    out
}

#[cfg(test)]
mod tests;
