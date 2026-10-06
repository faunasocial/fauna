// Regenerating testdata/wrapped-cert.cbor: add a temporary
// `#[test] #[ignore] fn write_test_fixture_for_go_b6` to
// `libs/fauna-mls/src/wrapped_blob/mod.rs::seal_tests` that builds a
// deterministic TlsCertBundle and HPKE-Seals it to the X25519 pubkey
// derived from secret=[1u8; 32], with index
// (role="mta", bridge_id="test-bridge", domain="test.example.com").
//
// The current fixture's plaintext cert/key was generated with:
//
//   openssl req -x509 -newkey ed25519 -keyout key.pem -out cert.pem \
//       -days 3650 -nodes -subj "/CN=test.example.com"
//
// then those PEMs were pasted into the temporary Rust test verbatim.
// Run:
//
//   cargo test -p fauna-mls --lib write_test_fixture_for_go_b6 \
//       -- --ignored --nocapture
//
// then commit `testdata/wrapped-cert.cbor` and remove the temporary
// Rust test. The fixture is NOT byte-stable across regenerations
// (HPKE seals against a fresh ephemeral key each time); the Go test
// exercises decryptability and cert-parse, not byte parity.

package tls

import (
	"context"
	"crypto/x509"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// Fixed recipient X25519 secret used by every test. Matches the secret
// the Rust-side fixture-generator used to seal testdata/wrapped-cert.cbor.
var fixtureRecipientSecret = func() []byte {
	s := make([]byte, 32)
	for i := range s {
		s[i] = 0x01
	}
	return s
}()

const (
	fixtureDomain   = "test.example.com"
	fixtureRole     = "mta"
	fixtureBridgeID = "test-bridge"
	fixtureCertCN   = "test.example.com"
)

// recordingCaller mirrors the pattern in
// internal/wsrpc/methods_test.go: dagcbor-encode the caller's reply,
// cbor-decode into the reply target.
type recordingCaller struct {
	mu        sync.Mutex
	calls     int
	method    string
	replyBlob *[]byte // value for fetch_tls_cert_blob's Option<ByteBuf> reply
	err       error
}

func (r *recordingCaller) Call(_ context.Context, method string, _, reply any) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.calls++
	r.method = method
	if r.err != nil {
		return r.err
	}
	if reply == nil {
		return nil
	}
	// Mirror the wire shape of FetchTlsCertBlobReply ({"blob": Option<ByteBuf>}).
	body := struct {
		Blob *[]byte `cbor:"blob"`
	}{Blob: r.replyBlob}
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	return cbor.Unmarshal(enc, reply)
}

func (r *recordingCaller) callCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.calls
}

func (r *recordingCaller) setError(e error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.err = e
}

// loadFixture reads testdata/wrapped-cert.cbor; t.Skip if missing
// (so a fresh working tree without the fixture doesn't fail the suite —
// the regen instructions live in the file header).
func loadFixture(t *testing.T) []byte {
	t.Helper()
	path := filepath.Join("testdata", "wrapped-cert.cbor")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Skipf("fixture missing (%v); regen per the file-header instructions", err)
	}
	return data
}

// TestRefreshHappyPath: a Caller returning the known-good fixture
// produces a non-nil cached cert whose leaf has the expected CN.
func TestRefreshHappyPath(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	caller := &recordingCaller{replyBlob: &blob}

	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	if err := p.Refresh(context.Background()); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	if caller.method != wsrpc.MethodFetchTLSCertBlob {
		t.Errorf("method: got %q, want fauna.bridges.fetch_tls_cert_blob", caller.method)
	}

	cert := p.Certificate()
	if cert == nil {
		t.Fatal("Certificate() returned nil after successful Refresh")
	}
	if len(cert.Certificate) == 0 {
		t.Fatal("cert.Certificate is empty")
	}

	leaf, err := x509.ParseCertificate(cert.Certificate[0])
	if err != nil {
		t.Fatalf("parse leaf cert: %v", err)
	}
	if leaf.Subject.CommonName != fixtureCertCN {
		t.Fatalf("CN = %q, want %q", leaf.Subject.CommonName, fixtureCertCN)
	}
}

// TestRefreshErrorDoesNotInvalidateCachedCert: a successful refresh
// followed by a failing one leaves the original cert pointer in place.
func TestRefreshErrorDoesNotInvalidateCachedCert(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	caller := &recordingCaller{replyBlob: &blob}

	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := p.Refresh(context.Background()); err != nil {
		t.Fatalf("first Refresh: %v", err)
	}
	first := p.Certificate()
	if first == nil {
		t.Fatal("Certificate() nil after first Refresh")
	}

	caller.setError(errors.New("simulated nest outage"))
	if err := p.Refresh(context.Background()); err == nil {
		t.Fatal("Refresh should have failed but didn't")
	}

	got := p.Certificate()
	if got == nil {
		t.Fatal("cached cert was nil after failed Refresh — must be preserved")
	}
	if got != first {
		t.Fatal("cached cert pointer changed across failed Refresh — must be preserved")
	}
}

// TestNewRejectsWrongSecretLength: New fails fast on a non-32-byte
// secret (the FFI's length check is defense-in-depth; New should not
// even let the caller get there).
func TestNewRejectsWrongSecretLength(t *testing.T) {
	t.Parallel()
	for _, n := range []int{0, 1, 31, 33, 64} {
		secret := make([]byte, n)
		_, err := New(Config{
			Domain:       fixtureDomain,
			Role:         fixtureRole,
			BridgeID:     fixtureBridgeID,
			X25519Secret: secret,
			Caller:       &recordingCaller{},
		})
		if err == nil {
			t.Fatalf("New accepted %d-byte secret; want error", n)
		}
	}
}

// TestRefreshFailsOnWrongSecret: the FFI's HPKE-Open rejects a wrong
// recipient secret — Refresh propagates the error and leaves the cache
// untouched.
func TestRefreshFailsOnWrongSecret(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	wrong := make([]byte, 32)
	for i := range wrong {
		wrong[i] = 0x02
	}

	caller := &recordingCaller{replyBlob: &blob}
	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: wrong,
		Caller:       caller,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := p.Refresh(context.Background()); err == nil {
		t.Fatal("Refresh with wrong secret should have failed")
	}
	if p.Certificate() != nil {
		t.Fatal("cert should remain nil after failed Refresh")
	}
}

// TestRefreshFailsOnMissingBlob: nest returning a nil blob (no cert
// provisioned for this domain) surfaces as an error from Refresh.
func TestRefreshFailsOnMissingBlob(t *testing.T) {
	t.Parallel()
	caller := &recordingCaller{replyBlob: nil}
	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := p.Refresh(context.Background()); err == nil {
		t.Fatal("Refresh with nil blob should have failed")
	}
}

// TestStartRefreshesOnTimer: with a fake clock, firing a tick after
// the initial Refresh triggers another Refresh.
func TestStartRefreshesOnTimer(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	caller := &recordingCaller{replyBlob: &blob}

	tickCh := make(chan time.Time, 4)
	afterFn := func(time.Duration) <-chan time.Time { return tickCh }

	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
		AfterFn:      afterFn,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	if err := p.Refresh(context.Background()); err != nil {
		t.Fatalf("initial Refresh: %v", err)
	}
	if got := caller.callCount(); got != 1 {
		t.Fatalf("after initial Refresh, callCount = %d, want 1", got)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	p.Start(ctx)

	tickCh <- time.Now()

	if !waitFor(2*time.Second, func() bool { return caller.callCount() >= 2 }) {
		t.Fatalf("scheduled tick did not trigger refresh; callCount = %d", caller.callCount())
	}
}

// TestStartRefreshesOnTriggerRefresh: SIGHUP-equivalent
// (TriggerRefresh) triggers a refresh before the scheduled tick.
func TestStartRefreshesOnTriggerRefresh(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	caller := &recordingCaller{replyBlob: &blob}

	neverCh := make(chan time.Time)
	afterFn := func(time.Duration) <-chan time.Time { return neverCh }

	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
		AfterFn:      afterFn,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := p.Refresh(context.Background()); err != nil {
		t.Fatalf("initial Refresh: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	p.Start(ctx)

	p.TriggerRefresh()

	if !waitFor(2*time.Second, func() bool { return caller.callCount() >= 2 }) {
		t.Fatalf("TriggerRefresh did not trigger refresh; callCount = %d", caller.callCount())
	}
}

// TestRefreshLoopStartsInBackoffWhenNoCertCached pins the fresh-box readiness
// fix: when the synchronous boot refresh did not land a cert (the bridge is
// still pending approval, or its x25519 isn't yet on the nest row), the loop
// must start in the failure-backoff cadence (1m) — NOT the 12h steady-state
// interval — so 465/587/993 TLS (which errors with internal_error until a cert
// is cached) self-heals within a minute instead of stranding for up to 12h.
func TestRefreshLoopStartsInBackoffWhenNoCertCached(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	caller := &recordingCaller{replyBlob: &blob}

	// Capture the wait the loop requests before its first attempt, then block it
	// there (neverCh) so no refresh runs and the cached-nil precondition holds.
	waitCh := make(chan time.Duration, 1)
	neverCh := make(chan time.Time)
	afterFn := func(d time.Duration) <-chan time.Time {
		select {
		case waitCh <- d:
		default:
		}
		return neverCh
	}

	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
		AfterFn:      afterFn,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	// Simulate a failed/absent boot refresh: no Refresh called → no cert cached,
	// so GetCertificate errors (the internal_error handshake condition).
	if _, err := p.GetCertificate(nil); err == nil {
		t.Fatal("precondition: expected GetCertificate to error (no cert cached)")
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	p.Start(ctx)

	select {
	case d := <-waitCh:
		if d != time.Minute {
			t.Fatalf("first wait with no cert cached = %v, want 1m (backoff, not the 12h interval)", d)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("refresh loop never requested a wait")
	}
}

// TestConfigReloaderTLSApplierRefetchesCert pins the prompt-refresh-on-provision
// wiring mda.Run + mta.Run install (mail-bridge-lifecycle.md § TLS provisioning):
// TLS cert blobs are out of fetch_config's scope and the provider otherwise
// re-fetches only on its 12 h timer, so a cert that lands on a *running* bridge
// (a domain added post-claim → ACME issues a cert covering mail.<domain>; an
// admin provision_self_signed_cert) would go unserved until that timer / a
// restart. Nest fires a config_changed push (reason "tls") when a cert lands and
// both roles register a `func(_){ TLSProvider.TriggerRefresh() }` applier so the
// push re-fetches TLS. This test wires that exact applier onto a real
// ConfigReloader + a real started Provider and Apply()s a snapshot (the seam
// config_changed / reconnect both fan through), asserting the provider re-ran
// fetch_tls_cert_blob — the mechanism the two one-line registrations depend on.
func TestConfigReloaderTLSApplierRefetchesCert(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	// The provider's own caller — the fetch_tls_cert_blob count we assert on.
	certCaller := &recordingCaller{replyBlob: &blob}

	neverCh := make(chan time.Time)
	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       certCaller,
		// Never let the scheduled 12 h tick fire during the test — only the
		// config_changed-driven TriggerRefresh should drive the re-fetch.
		AfterFn: func(time.Duration) <-chan time.Time { return neverCh },
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	p.Start(ctx) // spawns refreshLoop; it blocks on select until a HUP arrives

	// A running bridge fetches its cert once synchronously at startup; before
	// any config_changed there must be no further fetch.
	if got := certCaller.callCount(); got != 0 {
		t.Fatalf("provider must not re-fetch before a config_changed; callCount = %d", got)
	}

	// A separate reloader caller (fetch_config), so its calls don't pollute the
	// provider's fetch count. Apply() bypasses the fetch entirely; the reloader
	// caller is unused here but required by the constructor.
	reloadCaller := &recordingCaller{}
	reloader := wsrpc.NewConfigReloader(reloadCaller, time.Second, slog.Default())
	// The exact applier mda.Run / mta.Run register.
	reloader.Register(func(_ wsrpc.ConfigSnapshot) { p.TriggerRefresh() })

	reloader.Apply(wsrpc.ConfigSnapshot{})

	if !waitFor(2*time.Second, func() bool { return certCaller.callCount() >= 1 }) {
		t.Fatalf("config_changed Apply did not re-fetch the TLS cert; callCount = %d", certCaller.callCount())
	}
	if certCaller.method != wsrpc.MethodFetchTLSCertBlob {
		t.Fatalf("re-fetch method: got %q, want %q", certCaller.method, wsrpc.MethodFetchTLSCertBlob)
	}
}

// TestGetCertificateUnderLoad spawns many goroutines reading the
// cached cert while Refresh runs in another; -race catches data races.
func TestGetCertificateUnderLoad(t *testing.T) {
	t.Parallel()
	blob := loadFixture(t)
	caller := &recordingCaller{replyBlob: &blob}

	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := p.Refresh(context.Background()); err != nil {
		t.Fatalf("initial Refresh: %v", err)
	}

	var stop atomic.Bool
	var wg sync.WaitGroup

	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for !stop.Load() {
				cert, err := p.GetCertificate(nil)
				if err != nil {
					t.Errorf("GetCertificate: %v", err)
					return
				}
				if cert == nil {
					t.Error("GetCertificate returned nil cert")
					return
				}
			}
		}()
	}

	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := 0; i < 50; i++ {
			if err := p.Refresh(context.Background()); err != nil {
				t.Errorf("concurrent Refresh: %v", err)
				return
			}
		}
	}()

	time.Sleep(100 * time.Millisecond)
	stop.Store(true)
	wg.Wait()
}

// TestGetCertificateBeforeRefreshReturnsError: a handshake before any
// Refresh sees an explicit error, not a panic or nil.
func TestGetCertificateBeforeRefreshReturnsError(t *testing.T) {
	t.Parallel()
	caller := &recordingCaller{}
	p, err := New(Config{
		Domain:       fixtureDomain,
		Role:         fixtureRole,
		BridgeID:     fixtureBridgeID,
		X25519Secret: fixtureRecipientSecret,
		Caller:       caller,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if _, err := p.GetCertificate(nil); err == nil {
		t.Fatal("GetCertificate before Refresh should error")
	}
}

func waitFor(deadline time.Duration, cond func() bool) bool {
	end := time.Now().Add(deadline)
	for time.Now().Before(end) {
		if cond() {
			return true
		}
		time.Sleep(5 * time.Millisecond)
	}
	return cond()
}
