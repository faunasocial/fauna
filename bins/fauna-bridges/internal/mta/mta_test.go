package mta

import (
	"context"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestRunReturnsWhenContextCancelled — Run idles until ctx is
// cancelled and returns nil cleanly. The fixture omits MailEnabled
// (zero value → false), so this exercises the "mail disabled, idle
// gracefully" gate that Phase C.1 introduces alongside the listener.
// A regression here (e.g. blocking on a select that never closes)
// would hang the SIGTERM path in main.go.
func TestRunReturnsWhenContextCancelled(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{
				MailEnabled:   false, // ← mail disabled, idle gate fires
				LocalDomains:  []string{"test.example.com"},
				PrimaryDomain: "test.example.com",
			},
			BridgeID: "test-mta-1",
		})
	}()

	// Give Run a moment to log its startup banner.
	time.Sleep(50 * time.Millisecond)
	cancel()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Run did not return within 1s of ctx cancel")
	}
}

// TestRunIdlesWhenLocalDomainsEmpty pins the listener-startup gate
// under the multi-domain refactor: even with mail enabled, when
// nest has no mail_domains rows yet (fetch_config returned an empty
// `LocalDomains` projection), Run must idle on ctx.Done() rather
// than attempt to bind the SMTP listener. Without at least one
// local domain there's nothing to accept RCPT TO for, so binding
// port 25 would expose a degraded service.
//
// The fixture deliberately sets MTABindAddr="not-an-address" so the
// negative assertion is sharp: had Run reached the bind path,
// net.Listen would reject the malformed addr and Run would return
// an error. Returning nil under ctx cancel proves Run never reached
// the bind path — the LocalDomains gate fired first.
//
// Sibling regression guard to TestRunReturnsWhenContextCancelled,
// which covers the mail-disabled idle path. Replaces the prior
// Phase C.1 TestRunIdlesWhenDomainEmpty (per-bridge Domain field
// dropped from WhoamiReply; multi-domain projection now lives on
// fetch_config's ConfigSnapshot).
func TestRunIdlesWhenLocalDomainsEmpty(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{
				MailEnabled:   true,
				LocalDomains:  []string{}, // ← gate condition
				PrimaryDomain: "",
			},
			BridgeID:    "test-mta-1",
			MTABindAddr: "not-an-address", // poisoned — bind would fail loudly
		})
	}()
	time.Sleep(50 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run with empty LocalDomains should idle without binding (nil), got: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Run did not return within 1s of ctx cancel — gate is broken")
	}
}

// TestRunBindsPort25WithoutTLSProvider pins the target shape per
// docs/goal/behavior/smtp-server.md § Implementation status today:
// when TLSProvider is nil (admin hasn't provisioned an admin-uploaded
// TLS cert yet, or ACME hasn't run yet), Run still binds the port-25
// MX listener (plaintext-with-no-STARTTLS — port 25 doesn't wire
// srv.TLSConfig in runListenerWithBackend today; STARTTLS-on-25 is a
// forward-pointer in the goal-doc) and skips the submission listeners
// 465/587, which genuinely require TLS at bind. The bridge can accept
// inbound MX even before TLS lands; submission is unavailable until
// TLS is provisioned.
//
// Sibling regression guard to TestRunIdlesWhenDomainEmpty (Domain gate
// fires before the TLSProvider gate) and TestRunReturnsWhenContext
// Cancelled (MailEnabled gate fires before everything else). The
// three together pin the full pre-Phase-C.1 idle-vs-bind decision
// tree.
func TestRunBindsPort25WithoutTLSProvider(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{
				MailEnabled:   true,
				LocalDomains:  []string{"test.example.com"},
				PrimaryDomain: "test.example.com",
			},
			BridgeID:       "test-mta-1",
			TLSProvider:    nil, // ← no TLS provisioned
			MTABindAddr:    "127.0.0.1:0",
			MTABindAddr465: "127.0.0.1:0", // unused — listener skipped when TLS absent
			MTABindAddr587: "127.0.0.1:0", // unused — listener skipped when TLS absent
		})
	}()
	// Give Run a moment to bind port 25 and start serving.
	time.Sleep(100 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run with nil TLSProvider should bind only port 25 and idle, got: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Run did not return within 1s of ctx cancel — gate is broken or port-25 listener won't tear down")
	}
}

// TestMailEnabledFromSnapshot pins Phase C.0's explicit
// `mail_enabled` field semantics — MailEnabled reads the field
// directly, *not* a heuristic over Spam.* values. The integration-
// test fixtures in main_integration_test.go rely on this:
// `enabledConfigSnapshot()` sets MailEnabled=true, and
// `disabledConfigSnapshot()` is the all-zero struct (MailEnabled=
// false).
func TestMailEnabledFromSnapshot(t *testing.T) {
	t.Parallel()
	// Zero value: MailEnabled=false (the "mail off" path).
	off := wsrpc.ConfigSnapshot{}
	if MailEnabled(off) {
		t.Errorf("all-zero ConfigSnapshot should report mail disabled")
	}
	// Explicit MailEnabled=true is the only way to flip the bit.
	on := wsrpc.ConfigSnapshot{MailEnabled: true}
	if !MailEnabled(on) {
		t.Errorf("ConfigSnapshot{MailEnabled: true} should report mail enabled")
	}
	// Heuristic-shaped (non-zero Spam fields) but MailEnabled=false:
	// must report disabled. Pins the heuristic removal — Phase B.8
	// would have flagged this as "enabled" via the old Spam-derived
	// rule. Phase C.0's listener idles regardless of the spam policy.
	heuristicShaped := wsrpc.ConfigSnapshot{
		Spam: wsrpc.SpamPolicyThresholds{
			MaxScoreBeforeReject: 5,
			DNSBLServers:         []string{"zen.spamhaus.org"},
		},
	}
	if MailEnabled(heuristicShaped) {
		t.Errorf("heuristic-shaped snapshot with MailEnabled=false should report mail disabled (Phase C.0 removed the heuristic)")
	}
}
