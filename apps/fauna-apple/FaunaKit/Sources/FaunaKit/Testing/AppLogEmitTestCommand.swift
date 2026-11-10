import FaunaFFISwift
import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it. Also required
// for correctness: `logEmitBurstForTest` is a UniFFI export only the
// `test-helpers` flavor carries (`libs/fauna-ffi/src/logs.rs`).
#if DEBUG

/// Shared handler for the cross-app `app_log_emit` TestAgent command — the
/// burst `test_app_log_size_cap_tui.py` writes through the app's own tracing
/// subscriber to watch the on-device log directory hold its size cap
/// (`observability.md` § 1. Shared capture; `docs/features/app-log.md`
/// outcome 7). The apple twin of tui's, linux's and android's arms: one shared
/// body, `fauna_log::rolling::emit_burst_for_test`, over the FFI.
///
/// The lowered cap needs no reload here: both apple drivers hand the launch
/// environment to the process before `installLogging` runs (macOS launches the
/// bare binary with it, iOS forwards it as `SIMCTL_CHILD_*`), so the writer
/// opens at the lowered cap — unlike android, whose launch extras become
/// process env only after logging is up.
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation; the
/// shells' switch arms are the same few lines as `DeviceSetStateTestCommand`'s.
public enum AppLogEmitTestCommand {
    /// The same two-case shape as `DeviceSetStateTestCommand.Outcome`.
    public enum Outcome: Equatable {
        /// The burst's JSON result (`{"emitted": n}`, or `{"error": "…"}` for a
        /// burst the shared body refused), for the shell's result slot.
        case report(String)
        /// A malformed payload: the shell surfaces it as a **loud** TestAgent
        /// failure (convention 11) and leaves the result slot empty.
        case refused(String)
    }

    public static func apply(_ command: [String: Any]) -> Outcome {
        guard let tag = command["tag"] as? String,
              let count = (command["count"] as? NSNumber)?.uint64Value,
              let bytes = (command["bytes"] as? NSNumber)?.uint64Value else {
            return .refused("app_log_emit: needs a `tag` string and `count`/`bytes` numbers")
        }
        return .report(logEmitBurstForTest(tag: tag, count: count, bytes: bytes))
    }
}

#endif
