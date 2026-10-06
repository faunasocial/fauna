//! The pure, wasm-safe half of the cross-app e2e observable contract — the
//! counting and parsing every app's leg shares, native and web alike.
//!
//! `fauna-e2e-agent` owns the contract (its state keys and command names, with
//! their docs) and hosts the native automation surface; it re-exports these so a
//! native host names them where it always has. They live here because the agent
//! crate carries an HTTP server a browser build must never link: the web leg
//! (`fauna-wasm` and `fauna-rpc-wasm`, both behind `test-helpers`, convention 15
//! rule (b)) reaches the same counters through this crate instead of a
//! TypeScript re-implementation that could drift from them.
//!
//! Pure data and arithmetic — no clock, no I/O, no runtime.

use serde_json::{Value, json};

/// The counter behind `fauna_e2e_agent::CONNECTION_REPORTS_KEY` — one per app, fed every
/// connection-state word the indicator receives.
#[derive(Debug, Default)]
pub struct ConnectionReports {
    reports: u64,
    transitions: u64,
    word: Option<&'static str>,
}

impl ConnectionReports {
    /// Record one report of `word` (a `ConnectionState::as_wire_word` answer).
    pub fn observe(&mut self, word: &'static str) {
        self.reports += 1;
        if self.word.is_some_and(|last| last != word) {
            self.transitions += 1;
        }
        self.word = Some(word);
    }

    /// The `fauna_e2e_agent::CONNECTION_REPORTS_KEY` value.
    pub fn json(&self) -> Value {
        json!({
            "reports": self.reports,
            "transitions": self.transitions,
            "word": self.word,
        })
    }
}

/// Parse a `fauna_e2e_agent::RECONNECT_BACKOFF` payload: `Ok(Some((initial, max)))` to pace,
/// `Ok(None)` to restore, `Err` naming what is wrong. Shared so every app reads
/// the same shape and refuses the same malformed ones.
pub fn reconnect_backoff_bounds(
    payload: &Value,
) -> Result<Option<(std::time::Duration, std::time::Duration)>, String> {
    let initial = payload.get("initial_ms");
    let max = payload.get("max_ms");
    match (initial, max) {
        (None, None) => Ok(None),
        (Some(initial), Some(max)) => {
            let (Some(initial), Some(max)) = (initial.as_u64(), max.as_u64()) else {
                return Err(format!(
                    "initial_ms/max_ms must be non-negative integers, got {initial} / {max}"
                ));
            };
            if initial == 0 || max < initial {
                return Err(format!(
                    "need 0 < initial_ms <= max_ms, got {initial} / {max}"
                ));
            }
            Ok(Some((
                std::time::Duration::from_millis(initial),
                std::time::Duration::from_millis(max),
            )))
        }
        _ => Err("give both initial_ms and max_ms, or neither to restore".to_string()),
    }
}

/// Whether `id` names an error surface in `fauna_e2e_agent::PAINTED_ERRORS_KEY`'s sense.
pub fn is_error_surface(id: &str) -> bool {
    id == "error-message" || id.ends_with("-error")
}

/// The tally behind `fauna_e2e_agent::PAINTED_ERRORS_KEY` — one per app, fed each painted
/// frame's `(id, text)` pairs (every element, or just the error-shaped ones;
/// it filters with [`is_error_surface`] itself).
#[derive(Debug, Default)]
pub struct PaintedErrorTally {
    count: u64,
    showing: Vec<(String, String)>,
}

impl PaintedErrorTally {
    /// Record one painted frame.
    pub fn observe<'a>(&mut self, frame: impl IntoIterator<Item = (&'a str, &'a str)>) {
        let now: Vec<(String, String)> = frame
            .into_iter()
            .filter(|(id, text)| is_error_surface(id) && !text.trim().is_empty())
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect();
        self.count += now.iter().filter(|e| !self.showing.contains(e)).count() as u64;
        self.showing = now;
    }

    /// The `fauna_e2e_agent::PAINTED_ERRORS_KEY` value.
    pub fn json(&self) -> Value {
        json!({
            "count": self.count,
            "showing": self
                .showing
                .iter()
                .map(|(id, text)| json!({ "id": id, "text": text }))
                .collect::<Vec<_>>(),
        })
    }
}

/// Build `fauna_e2e_agent::LAUNCH_TOKEN_KEY`'s value:
/// `{"expires_in_secs": <i64>|null, "own_session_ids": [..]}`.
/// `expires_at_secs` is the held bearer's deadline on the client clock (or
/// `None` when no bearer is held), `now_secs` that same clock's `now` — both
/// read by the caller, since this crate has no clock. Here rather than in the
/// agent crate so web (`fauna-wasm`) and the UniFFI apps (`fauna-ffi`) build
/// the same shape as tui and linux.
pub fn launch_token_json(
    expires_at_secs: Option<u64>,
    now_secs: i64,
    own_session_ids: &[String],
) -> Value {
    json!({
        "expires_in_secs": expires_at_secs.map(|e| e as i64 - now_secs),
        "own_session_ids": own_session_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stickiness is read as "reports moved, transitions did not", so a repeat
    /// of the same word must count as a report and never as a transition.
    #[test]
    fn connection_reports_count_repeats_as_reports_only() {
        let mut reports = ConnectionReports::default();
        for word in [
            "connecting",
            "disconnected",
            "unreachable",
            "unreachable",
            "unreachable",
        ] {
            reports.observe(word);
        }
        assert_eq!(
            reports.json(),
            json!({ "reports": 5, "transitions": 2, "word": "unreachable" })
        );
        reports.observe("connected");
        assert_eq!(reports.json()["transitions"], 3);
    }

    #[test]
    fn reconnect_backoff_payloads_parse_or_refuse() {
        use std::time::Duration;
        assert_eq!(reconnect_backoff_bounds(&json!({})), Ok(None));
        assert_eq!(
            reconnect_backoff_bounds(&json!({ "initial_ms": 20, "max_ms": 100 })),
            Ok(Some((
                Duration::from_millis(20),
                Duration::from_millis(100)
            )))
        );
        // Each of these would leave the production pace silently in force.
        for bad in [
            json!({ "initial_ms": 20 }),
            json!({ "initial_ms": 0, "max_ms": 100 }),
            json!({ "initial_ms": 200, "max_ms": 100 }),
            json!({ "initial_ms": "20", "max_ms": 100 }),
        ] {
            assert!(
                reconnect_backoff_bounds(&bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    /// The tally counts APPEARANCES: a standing error once, a re-raised one
    /// again, and never a log or an empty line.
    #[test]
    fn the_painted_error_tally_counts_each_appearance() {
        let mut tally = PaintedErrorTally::default();
        tally.observe([("feed-view", "posts"), ("error-message", "")]);
        assert_eq!(tally.json()["count"], 0, "an empty error line is no error");
        tally.observe([
            ("error-message", "boom"),
            ("archive-import-error-log", "old"),
        ]);
        tally.observe([("error-message", "boom")]);
        assert_eq!(tally.json()["count"], 1, "a standing error counts once");
        tally.observe([("error-message", "boom"), ("contact-find-error", "nope")]);
        assert_eq!(tally.json()["count"], 2, "a second surface counts");
        tally.observe([]);
        tally.observe([("error-message", "boom")]);
        assert_eq!(
            tally.json()["count"],
            3,
            "cleared and raised again counts again"
        );
        assert_eq!(
            tally.json()["showing"],
            json!([{ "id": "error-message", "text": "boom" }])
        );
    }
}
