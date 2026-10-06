package imap

import (
	"log/slog"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestBackend_ApplyConfigHotSwapsIdleTimeout — ApplyConfig changes the IDLE
// timeout that *future* sessions see (the next-request-boundary hot-reload
// contract), while a session already created keeps the value it was built
// with. This is the bridge-side half of the config_changed hot-reload
// track.
func TestBackend_ApplyConfigHotSwapsIdleTimeout(t *testing.T) {
	b := NewBackend(&recordingCaller{}, slog.Default(), 0, time.Second, 0, nil, nil).(*backend)

	// A session built before any config change sees the boot value.
	before := b.NewSession(nil).(*Session)
	if before.idleTimeout != time.Second {
		t.Fatalf("pre-apply session idleTimeout = %v, want 1s", before.idleTimeout)
	}

	// Admin lowered put_imap_policy{idle_timeout_secs} to 5 → config_changed
	// → re-fetch → ApplyConfig.
	b.ApplyConfig(wsrpc.ConfigSnapshot{IMAP: wsrpc.ImapPolicy{IdleTimeoutSecs: 5}})

	// A NEW session picks up the new timeout at its request boundary.
	after := b.NewSession(nil).(*Session)
	if after.idleTimeout != 5*time.Second {
		t.Fatalf("post-apply session idleTimeout = %v, want 5s", after.idleTimeout)
	}
	// The earlier session is unchanged (no restart, no mid-session mutation).
	if before.idleTimeout != time.Second {
		t.Fatalf("in-flight session idleTimeout changed to %v, want unchanged 1s", before.idleTimeout)
	}
}

// A `0` idle_timeout_secs must reach BOTH consumers of the field as the same
// effective value. Session.idle already floored `<= 0` to defaultIdleTimeout,
// but IdleTimeout() — which the vendored imapserver's handleIdle reads to set
// the IDLE connection read deadline — returned the raw 0, so the two disagreed
// about what the knob meant. Sweep ruled `0` = "unset, use the default" for
// this knob and made the accessor say so.
func TestSession_IdleTimeoutZeroFallsBackToDefault(t *testing.T) {
	b := NewBackend(&recordingCaller{}, slog.Default(), 0, 0, 0, nil, nil).(*backend)
	s := b.NewSession(nil).(*Session)

	if s.idleTimeout != 0 {
		t.Fatalf("fixture precondition: raw idleTimeout = %v, want 0", s.idleTimeout)
	}
	if got := s.IdleTimeout(); got != defaultIdleTimeout {
		t.Fatalf("IdleTimeout() = %v, want the %v fallback Session.idle applies", got, defaultIdleTimeout)
	}
}

// TestBackend_ApplyConfigHotSwapsBayesianKnobs — ApplyConfig threads the
// Tier-2 `mail.spam.bayesian_*` knobs off the snapshot into the per-user
// scorer config future sessions snapshot, so an admin's `put_spam_policy`
// weight / confidence-ramp override reaches the SELECT-time scorer at the next
// request boundary (mail-policy-config.md § Spam). An unseeded backend yields
// the catalog defaults (700/50/200); an in-flight session keeps its copy.
func TestBackend_ApplyConfigHotSwapsBayesianKnobs(t *testing.T) {
	b := NewBackend(&recordingCaller{}, slog.Default(), 0, time.Second, 0, nil, nil).(*backend)

	// A session built before any seed sees the catalog defaults.
	before := b.NewSession(nil).(*Session)
	if before.bayesianKnobs != mailfauna.DefaultBayesianKnobs() {
		t.Fatalf("pre-seed session knobs = %+v, want catalog defaults", before.bayesianKnobs)
	}

	// Admin raised put_spam_policy{bayesian_*} → config_changed → re-fetch →
	// ApplyConfig carries the new knobs off snap.Spam.
	b.ApplyConfig(wsrpc.ConfigSnapshot{Spam: wsrpc.SpamPolicyThresholds{
		MaxScoreBeforeSpamFolder:      5,
		BayesianWeightMilli:           900,
		BayesianMinSamples:            40,
		BayesianFullConfidenceSamples: 300,
	}})

	after := b.NewSession(nil).(*Session)
	want := mailfauna.BayesianKnobs{BayesianWeightMilli: 900, MinSamples: 40, FullConfidenceSamples: 300}
	if after.bayesianKnobs != want {
		t.Fatalf("post-apply session knobs = %+v, want %+v", after.bayesianKnobs, want)
	}
	// The earlier session is unchanged (no mid-session mutation).
	if before.bayesianKnobs != mailfauna.DefaultBayesianKnobs() {
		t.Fatalf("in-flight session knobs changed to %+v, want unchanged defaults", before.bayesianKnobs)
	}
}

// TestBackend_ApplyConfigResizesBodyStructureCache — ApplyConfig hot-resizes the
// shared BODYSTRUCTURE LRU from the snapshot's bodystructure_cache_max, evicting
// the LRU tail immediately when the cap shrinks.
func TestBackend_ApplyConfigResizesBodyStructureCache(t *testing.T) {
	b := NewBackend(&recordingCaller{}, slog.Default(), 4, time.Second, 0, nil, nil).(*backend)
	ks := make([]bodyStructureCacheKey, 4)
	for i := range ks {
		ks[i] = bodyStructureCacheKey{actorID: "a", mailbox: "INBOX", uidValidity: 1, uid: uint32(i + 1)}
		b.cache.Put(ks[i], makeBS("k"), makeEnv("<x>"))
	}

	// Admin lowered put_imap_policy{bodystructure_cache_max} to 2 → config_changed
	// → re-fetch → ApplyConfig. (IdleTimeoutSecs carried along, unrelated here.)
	b.ApplyConfig(wsrpc.ConfigSnapshot{IMAP: wsrpc.ImapPolicy{IdleTimeoutSecs: 1740, BodyStructureCacheMax: 2}})

	if _, _, ok := b.cache.Get(ks[0]); ok {
		t.Errorf("k1 (LRU) must be evicted by the hot-applied cap=2")
	}
	if _, _, ok := b.cache.Get(ks[1]); ok {
		t.Errorf("k2 must be evicted by the hot-applied cap=2")
	}
	if _, _, ok := b.cache.Get(ks[2]); !ok {
		t.Errorf("k3 must be retained after the hot-applied cap=2")
	}
	if _, _, ok := b.cache.Get(ks[3]); !ok {
		t.Errorf("k4 (MRU) must be retained after the hot-applied cap=2")
	}
}

// TestBackend_ApplyConfigHotSwapsAuthLockout — ApplyConfig rebuilds the
// per-(credential, source-IP) AUTH-failure lockout from the snapshot's
// max_auth_failures_per_minute, so an admin who tightens the ceiling sees it
// take effect on the next session without a restart; an in-flight session
// keeps the instance it was built with (security review § D4/M1).
func TestBackend_ApplyConfigHotSwapsAuthLockout(t *testing.T) {
	// Boot with the gate disabled (limit 0).
	b := NewBackend(&recordingCaller{}, slog.Default(), 0, time.Second, 0, nil, nil).(*backend)

	disabled := b.NewSession(nil).(*Session).lockout
	for i := 0; i < 5; i++ {
		disabled.RecordFailureFor("u", "c", "ip")
	}
	if disabled.IsLockedFor("u", "c", "ip") {
		t.Fatal("limit-0 lockout must never lock")
	}

	// Admin set put_*_policy{max_auth_failures_per_minute}=1 → config_changed.
	b.ApplyConfig(wsrpc.ConfigSnapshot{Auth: wsrpc.AuthPolicy{MaxAuthFailuresPerMinute: 1}})

	enabled := b.NewSession(nil).(*Session).lockout
	enabled.RecordFailureFor("u", "c", "ip")
	if !enabled.IsLockedFor("u", "c", "ip") {
		t.Fatal("post-apply lockout (limit 1) must lock after one failure")
	}
	// The earlier session's lockout instance is unchanged.
	if disabled.IsLockedFor("u", "c", "ip") {
		t.Fatal("in-flight session's lockout must keep its disabled instance")
	}
}
