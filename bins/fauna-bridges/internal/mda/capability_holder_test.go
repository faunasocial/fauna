package mda

import (
	"context"
	"io"
	"log/slog"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// capDiscardLogger silences the holder's structured logs during tests.
func capDiscardLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

// neverFiresCap is the test AfterFn: the scheduled-refresh channel never
// delivers, so the only refreshes are the synchronous initial one + any
// SIGHUP/config_changed poke — no real 12 h timer lingers and no fired timer
// races the assertions.
func neverFiresCap(time.Duration) <-chan time.Time { return make(chan time.Time) }

// cannedGrant stands in for the FFI HPKE-Open (proven in Slice 1/4): the wiring
// test only asserts the holder fetched + cached a grant, not the crypto.
func cannedGrant() *mailfauna.CapabilityGrant {
	kind := "mail"
	return &mailfauna.CapabilityGrant{
		OwnerActorId: []byte{0x01},
		GrantId:      []byte{0x02},
		EpochStart:   1,
		EpochEnd:     2,
		Keys: []mailfauna.CapabilityScopeKey{
			{Class: "content.read", Kind: &kind, Key: make([]byte, 32)},
		},
	}
}

// TestStartCapabilityHolderNoSecretIsNoop pins the pre-enrollment / localhost
// no-op: with no (or a short) enrolled holder secret the MDA serves mail
// without holding capabilities (design § 2.4).
func TestStartCapabilityHolderNoSecretIsNoop(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	reg, err := startCapabilityHolder(ctx, capHolderDeps{secret: nil, logger: capDiscardLogger()})
	if err != nil {
		t.Fatalf("startCapabilityHolder(nil secret): unexpected err %v", err)
	}
	if reg != nil {
		t.Fatalf("startCapabilityHolder(nil secret) = %v, want nil (no-op)", reg)
	}

	reg2, err := startCapabilityHolder(ctx, capHolderDeps{secret: make([]byte, 16), logger: capDiscardLogger()})
	if err != nil {
		t.Fatalf("startCapabilityHolder(16-byte secret): unexpected err %v", err)
	}
	if reg2 != nil {
		t.Fatalf("startCapabilityHolder(16-byte secret) = %v, want nil (no-op)", reg2)
	}
}

// TestStartCapabilityHolderInitialFetchPopulatesGrants proves the wiring runs a
// best-effort synchronous initial fetch so grants are held promptly (before any
// timer/hup): a valid holder secret yields a started Registry whose Current()
// already reflects the grant the (faked) nest served.
func TestStartCapabilityHolderInitialFetchPopulatesGrants(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	fetchFn := func(_ context.Context) ([][]byte, error) { return [][]byte{[]byte("A")}, nil }
	unsealFn := func(_ []byte, _ []byte, _ []byte) (*mailfauna.CapabilityGrant, error) { return cannedGrant(), nil }

	reg, err := startCapabilityHolder(ctx, capHolderDeps{
		secret:   make([]byte, 32),
		logger:   capDiscardLogger(),
		fetchFn:  fetchFn,
		unsealFn: unsealFn,
		afterFn:  neverFiresCap,
	})
	if err != nil {
		t.Fatalf("startCapabilityHolder: %v", err)
	}
	if reg == nil {
		t.Fatal("startCapabilityHolder(valid secret) = nil, want a started holder")
	}
	// Close blocks until the loop exits, so cancel FIRST (the production
	// teardown ordering Close now enforces).
	defer func() { cancel(); reg.Close() }()

	if got := reg.Current().Len(); got != 1 {
		t.Fatalf("Current().Len() after initial refresh = %d, want 1 (the served grant)", got)
	}
}

// TestStartCapabilityHolderConfigChangedTriggersRefresh is the load-bearing
// wiring assertion: a config_changed push re-fetches the grants so a nest-side
// mint/revoke bites at use-time, not only after the 12 h backstop (design § 2.3
// honest-box revocation). The holder must register a TriggerRefresh on the
// config reloader.
func TestStartCapabilityHolderConfigChangedTriggersRefresh(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	fetched := make(chan struct{}, 8)
	fetchFn := func(_ context.Context) ([][]byte, error) {
		fetched <- struct{}{}
		return nil, nil // empty authoritative set — unseal never runs
	}
	reloader := wsrpc.NewConfigReloader(nil, time.Second, capDiscardLogger())

	reg, err := startCapabilityHolder(ctx, capHolderDeps{
		secret:   make([]byte, 32),
		logger:   capDiscardLogger(),
		fetchFn:  fetchFn,
		afterFn:  neverFiresCap,
		reloader: reloader,
	})
	if err != nil {
		t.Fatalf("startCapabilityHolder: %v", err)
	}
	if reg == nil {
		t.Fatal("startCapabilityHolder(valid secret) = nil, want a started holder")
	}
	// Close blocks until the loop exits, so cancel FIRST (the production
	// teardown ordering Close now enforces).
	defer func() { cancel(); reg.Close() }()

	// The synchronous initial refresh fires exactly one fetch.
	select {
	case <-fetched:
	case <-time.After(3 * time.Second):
		t.Fatal("expected a synchronous initial capability-grant fetch")
	}

	// A config_changed push must poke the holder to re-fetch.
	reloader.Apply(wsrpc.ConfigSnapshot{})
	select {
	case <-fetched:
	case <-time.After(3 * time.Second):
		t.Fatal("config_changed did not trigger a capability-grant refresh (revoke would not bite until the 12 h backstop)")
	}
}
