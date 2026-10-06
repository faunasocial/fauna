import Foundation

/// Category-3 capture helpers (observability.md § What must be logged): a
/// **meaningfully swallowed** error — a `try?` / empty `catch` where the error
/// carried information (a wire op, RPC, parse, crypto, IO failure) — must reach
/// the shared `fauna_log` ring. These run the body exactly like `try?` (returning
/// the value or `nil`, preserving the call site's control flow) but log the error
/// at `level` on the way past, so the swallow stays a swallow while the failure
/// becomes visible on the Logs page.
///
/// Level (observability.md § Level mapping): `.warn` if the swallow degrades the
/// user's task, `.debug` if it is best-effort / expected-control-flow (e.g. a
/// decrypt that filters "not addressed to us"). `target` is the dotted source
/// (`fauna.client`, `fauna.mls`, `fauna.sync`, …); `context` names the operation.
///
/// **Redaction** (observability.md § Persistence & privacy): callers must not pass
/// a `body` whose error embeds a secret or message plaintext — only the error's
/// own description is logged here.
@discardableResult
public func logTry<T>(_ level: LogLevel, _ target: String, _ context: String,
                      _ body: () throws -> T) -> T? {
    do {
        return try body()
    } catch {
        logMessage(level: level, target: target, message: "\(context): \(error)")
        return nil
    }
}

/// Async counterpart of `logTry` — for `try? await …` swallows.
@discardableResult
public func logTryAsync<T>(_ level: LogLevel, _ target: String, _ context: String,
                           _ body: () async throws -> T) async -> T? {
    do {
        return try await body()
    } catch {
        logMessage(level: level, target: target, message: "\(context): \(error)")
        return nil
    }
}
