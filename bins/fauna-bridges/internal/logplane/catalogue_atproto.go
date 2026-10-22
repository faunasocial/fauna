package logplane

import "fmt"

// This file is the **atproto bridge's own** part of the log-plane catalogue —
// the events only the PDS binary (`cmd/fauna-atproto-bridge`) reports, on top
// of the shared set in catalogue.go (ready, reconnects, the PDS listener's bind
// and TLS cert fetch, a forced shutdown). The authoring rules there
// (§ Deliverability, § The interpolation rule) bind here unchanged: no DID,
// handle, repo path, file name or upstream error text ever rides an event —
// those stay in the bridge's own slog record at the same site.
//
// # What is deliberately NOT here
//
//   - *Firehose subscribe/resubscribe.* The bridge SERVES
//     `com.atproto.sync.subscribeRepos` (internal/atprotofirehose); it
//     subscribes to nothing, so such an event would have no call site.
//   - *A did:web identity whose handle changed.* That is one user's identity
//     limit, logged at error level for diagnosis, not a box-health signal an
//     admin acts on; it belongs on the user's own surface, not the admin ring.
//   - *confinement_degraded.* Shared, but this binary is not wrapped in
//     `fauna-sandbox` today, so it would fire on every boot about a state the
//     admin cannot change. It is wired the day this role gains a profile.

// atprotoEventIDs is every event id this file defines.
var atprotoEventIDs = []string{
	"pds_listener_failed",
	"repo_self_check_failed",
	"video_remux_failed",
}

// PDSListenerFailed — the PDS (XRPC) listener stopped serving after it had
// bound, so the bridge is shutting down for a supervisor restart. Until it is
// back no relay can crawl and no app can reach this nest's atproto accounts.
// (A listener that never bound is the shared ListenerBindFailed("pds", port).)
//
// Deliverable: the bridge still holds its nest link when the serve loop
// returns, and the plane's final flush runs before the link is closed.
func PDSListenerFailed() {
	Emit(LevelError, "pds_listener_failed",
		"pds listener stopped serving; bridge shutting down for a restart")
}

// repoSelfCheckFailures coalesces the repo self-check's failures: the boot pass
// that reloads every persisted repo, and the projection pass that verifies a
// repo head under the account's current signing key. A repo that fails either
// is storage a relay will reject — an admin-actionable fact — but the DID that
// failed is user data, so the plane carries the count and the bridge log the
// rest.
var repoSelfCheckFailures = newTally(LevelError, "repo_self_check_failed", func(n uint64) string {
	return fmt.Sprintf("%d hosted atproto repo(s) failed the self-check (reload or signature); see the bridge log", n)
})

// RepoSelfCheckFailed counts one repo failing the self-check; the next flush
// reports the count since the last one.
//
// Deliverable: both self-check passes run while the bridge holds its nest link.
func RepoSelfCheckFailed() { repoSelfCheckFailures.add() }

// videoRemuxFailures coalesces video remux failures — the projection skips a
// post whose video it could not assemble into an MP4, so the post never reaches
// the network. One event per failing post would let a bad batch flood the ring;
// the count since the last flush is what an admin acts on.
var videoRemuxFailures = newTally(LevelWarn, "video_remux_failed", func(n uint64) string {
	return fmt.Sprintf("%d video post(s) skipped: remux to MP4 failed; see the bridge log", n)
})

// VideoRemuxFailed counts one failed remux; the next flush reports the count
// since the last one.
//
// Deliverable: the projection runs while the bridge holds its nest link (it
// fetches the segments from nest over it).
func VideoRemuxFailed() { videoRemuxFailures.add() }
