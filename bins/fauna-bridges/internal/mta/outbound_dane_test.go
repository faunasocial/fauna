package mta

import (
	"context"
	"crypto/sha256"
	"errors"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── DANE/TLSA fake caller ─────────────────────────────────────────

// daneCaller scripts fauna.bridges.fetch_tlsa on top of mtaStsCaller's
// MTA-STS scripting (and queueCaller's queue behaviour), so the DANE table
// tests can drive secure-records / no-records / fetch-error and the
// DANE-beats-MTA-STS-enforce precedence (smtp-server.md:449).
type daneCaller struct {
	mtaStsCaller
	// tlsaRecords is returned for fetch_tlsa (empty = no DANE).
	tlsaRecords []wsrpc.TlsaRecordWire
	// tlsaErr, when non-nil, is returned for fetch_tlsa (simulating an RPC
	// transport / resolver failure — must not block delivery).
	tlsaErr error
	// nTlsa counts fetch_tlsa calls, so a test can assert the DNSSEC-MX gate
	// fires BEFORE the fetch rather than discarding its result after.
	// Guarded by the embedded queueCaller's mu.
	nTlsa int
}

// tlsaCalls reports how many times fetch_tlsa was called.
func (c *daneCaller) tlsaCalls() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.nTlsa
}

func (c *daneCaller) Call(ctx context.Context, method string, body, reply any) error {
	if method == wsrpc.MethodFetchTlsa {
		c.mu.Lock()
		c.nTlsa++
		c.mu.Unlock()
		if c.tlsaErr != nil {
			return c.tlsaErr
		}
		out := struct {
			Records []wsrpc.TlsaRecordWire `cbor:"records"`
		}{Records: c.tlsaRecords}
		repBytes, err := dagcbor.Marshal(out)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(repBytes, reply)
	}
	return c.mtaStsCaller.Call(ctx, method, body, reply)
}

func newDaneWorker(t *testing.T, c *daneCaller, mx MXResolver, sender SMTPSender) *OutboundWorker {
	t.Helper()
	cfg := OutboundWorkerConfig{
		PollInterval:   10 * time.Millisecond,
		BatchSize:      4,
		LeaseSeconds:   60,
		AttemptTimeout: time.Second,
	}
	w, err := NewOutboundWorker(c, mx, sender, "mta.example.com", cfg, discardLogger())
	if err != nil {
		t.Fatalf("NewOutboundWorker: %v", err)
	}
	return w
}

func daneEERecord() wsrpc.TlsaRecordWire {
	return wsrpc.TlsaRecordWire{Usage: 3, Selector: 0, Matching: 1, Data: make([]byte, 32)}
}

// runDaneUnit starts a worker over a single-unit queue and waits for the
// first delivered id, returning the sender's recorded calls.
func runDaneUnit(t *testing.T, c *daneCaller, sender *stsSender) []stsCall {
	t.Helper()
	w := newDaneWorker(t, c, twoHostMX(), sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		c.mu.Lock()
		defer c.mu.Unlock()
		return len(c.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()
	return sender.snapshot()
}

// TestDaneSecureRecordsPinRequired: nest returns TLSA records → the first
// host is sent with TLSDanePinned carrying those records (smtp-server.md
// § DANE).
func TestDaneSecureRecordsPinRequired(t *testing.T) {
	rec := daneEERecord()
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "not_published",
		},
		tlsaRecords: []wsrpc.TlsaRecordWire{rec},
	}
	calls := runDaneUnit(t, c, &stsSender{})
	if len(calls) != 1 || calls[0].Host != "mx-a.dest.test" {
		t.Fatalf("calls=%+v want single send to mx-a.dest.test", calls)
	}
	if calls[0].TLSPolicy.Mode != TLSDanePinned {
		t.Fatalf("tls mode = %v want TLSDanePinned", calls[0].TLSPolicy.Mode)
	}
	if len(calls[0].TLSPolicy.DaneRecords) != 1 {
		t.Fatalf("DaneRecords = %+v want the one fetched TLSA record", calls[0].TLSPolicy.DaneRecords)
	}
}

// TestDaneNoRecordsFallsThrough: no TLSA records + no MTA-STS → the host is
// sent opportunistically (no DANE pinning).
func TestDaneNoRecordsFallsThrough(t *testing.T) {
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "not_published",
		},
		tlsaRecords: nil,
	}
	calls := runDaneUnit(t, c, &stsSender{})
	if len(calls) != 1 || calls[0].TLSPolicy.Mode != TLSOpportunistic {
		t.Fatalf("calls=%+v want single opportunistic send (no DANE)", calls)
	}
}

// TestDaneFetchErrorFallsThrough: a fetch_tlsa RPC failure must NOT block
// delivery — the worker logs and proceeds with the base (opportunistic)
// posture (a TLSA fetch failure is not a delivery failure).
func TestDaneFetchErrorFallsThrough(t *testing.T) {
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "not_published",
		},
		tlsaErr: errors.New("nest unreachable"),
	}
	calls := runDaneUnit(t, c, &stsSender{})
	if len(calls) != 1 || calls[0].TLSPolicy.Mode != TLSOpportunistic {
		t.Fatalf("calls=%+v want single opportunistic send on fetch_tlsa error", calls)
	}
}

// TestDaneBeatsMtaStsEnforce: an MTA-STS enforce policy whose mx: matches
// the host (which would otherwise pin TLSRequired/WebPKI) is overridden to
// TLSDanePinned when the host also publishes TLSA records — DANE > MTA-STS
// (smtp-server.md:449).
func TestDaneBeatsMtaStsEnforce(t *testing.T) {
	rec := daneEERecord()
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "found",
			stsPolicy:   foundPolicy("enforce", "*.dest.test"), // matches mx-a
		},
		tlsaRecords: []wsrpc.TlsaRecordWire{rec},
	}
	calls := runDaneUnit(t, c, &stsSender{})
	if len(calls) != 1 || calls[0].Host != "mx-a.dest.test" {
		t.Fatalf("calls=%+v want single send to mx-a.dest.test", calls)
	}
	if calls[0].TLSPolicy.Mode != TLSDanePinned {
		t.Fatalf("tls mode = %v want TLSDanePinned (DANE beats MTA-STS enforce)", calls[0].TLSPolicy.Mode)
	}
}

// TestDaneChainMatchesFFIRoundTrip exercises the Go→UniFFI→Rust matcher used
// inside the TLS VerifyPeerCertificate callback: a selector=0 (full cert) /
// matching=1 (SHA-256) record matches a chain whose leaf hashes to its data,
// and rejects a chain that doesn't. Uses arbitrary bytes as the "cert DER"
// (selector 0 needs no X.509 parse), validating the FFI marshalling + the
// shared match logic without standing up a real TLS handshake (the pin
// mismatch → TemporaryError wiring is covered tier_3).
func TestDaneChainMatchesFFIRoundTrip(t *testing.T) {
	leaf := []byte("\x30\x82 pretend-cert-DER bytes for the leaf")
	sum := sha256.Sum256(leaf)
	match := wsrpc.TlsaRecordWire{Usage: 3, Selector: 0, Matching: 1, Data: sum[:]}
	if !daneChainMatches([]wsrpc.TlsaRecordWire{match}, [][]byte{leaf}, "mx.dest.test") {
		t.Fatal("daneChainMatches: matching DANE-EE SHA-256 record must match the leaf")
	}
	mismatch := wsrpc.TlsaRecordWire{Usage: 3, Selector: 0, Matching: 1, Data: make([]byte, 32)}
	if daneChainMatches([]wsrpc.TlsaRecordWire{mismatch}, [][]byte{leaf}, "mx.dest.test") {
		t.Fatal("daneChainMatches: a non-matching record must NOT match the leaf")
	}
	if daneChainMatches([]wsrpc.TlsaRecordWire{match}, nil, "mx.dest.test") {
		t.Fatal("daneChainMatches: an empty chain must never match")
	}

	// The usage-2 half, across the same FFI seam: a DANE-TA record is NOT
	// satisfied by a chain cert that merely hashes right. These bytes parse
	// as no certificate at all, so no leaf can chain to them -- which is
	// exactly the point, and is what the Rust side now refuses where it once
	// returned true (RFC 7672 3.1.1). The honest-chain and attacker-chain
	// cases need real minted certs and live in the Rust unit tests beside the
	// matcher; what this pins is that the mxHost argument crosses the FFI and
	// the usage-2 verdict is no longer a bare hash comparison.
	ta := wsrpc.TlsaRecordWire{Usage: 2, Selector: 0, Matching: 1, Data: sum[:]}
	if daneChainMatches([]wsrpc.TlsaRecordWire{ta}, [][]byte{leaf}, "mx.dest.test") {
		t.Fatal("daneChainMatches: a DANE-TA record must not be satisfied by a hash match alone")
	}
}

// ── DNSSEC provenance of the MX RRset ─────────────────────────────

// insecureTwoHostMX is twoHostMX's RRset with the DNSSEC provenance the
// resolver could NOT establish — an unsigned zone, a bogus signature, or a
// forged answer. Same hosts; only the provenance differs.
func insecureTwoHostMX() fakeMX {
	mx := twoHostMX()
	mx.insecure = true
	return mx
}

// runDaneUnitMX is runDaneUnit with the MX resolver spelled out, so a test
// can vary the RRset's DNSSEC provenance.
func runDaneUnitMX(t *testing.T, c *daneCaller, mx MXResolver, sender *stsSender) []stsCall {
	t.Helper()
	w := newDaneWorker(t, c, mx, sender)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg := w.Start(ctx)
	waitUntil(t, func() bool {
		c.mu.Lock()
		defer c.mu.Unlock()
		return len(c.delivered) == 1
	}, "delivered")
	cancel()
	wg.Wait()
	return sender.snapshot()
}

// TestDaneRequiresSecureMxRrset: an MX RRset that was NOT DNSSEC-validated
// must yield NO DANE pinning — even though the host's TLSA lookup would
// succeed and return usable records.
//
// Validating the TLSA leg alone authenticates a name
// the attacker picked: with a DNS-spoofing position they forge
// `MX dest.test → mx-a.dest.test` pointing at a host they control, publish a
// genuine DNSSEC-signed TLSA for that name (their own domain, signed for a
// few dollars), and the pin succeeds *honestly* against the wrong host while
// the bridge logs its strongest posture. RFC 7672 §2.2 is unambiguous that
// both legs must be secure: an SMTP client whose MX RRset is not
// DNSSEC-validated MUST NOT treat the destination as DANE-capable.
//
// The required direction is a FALLBACK, not a failure: delivery still
// happens, at the MTA-STS/opportunistic posture, exactly as an Insecure TLSA
// answer already behaves. A non-secure MX answer must never bounce mail.
func TestDaneRequiresSecureMxRrset(t *testing.T) {
	rec := daneEERecord()
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "not_published",
		},
		// nest would happily return usable records for this host — the gate
		// must not depend on the TLSA leg being empty.
		tlsaRecords: []wsrpc.TlsaRecordWire{rec},
	}
	calls := runDaneUnitMX(t, c, insecureTwoHostMX(), &stsSender{})
	if len(calls) != 1 {
		t.Fatalf("calls=%+v want exactly one send — a non-secure MX answer must fall back, not fail delivery", calls)
	}
	if calls[0].TLSPolicy.Mode != TLSOpportunistic {
		t.Fatalf("tls mode = %v want TLSOpportunistic: an unvalidated MX RRset must not be treated as DANE-capable (RFC 7672 §2.2)", calls[0].TLSPolicy.Mode)
	}
	if len(calls[0].TLSPolicy.DaneRecords) != 0 {
		t.Fatalf("DaneRecords = %+v want none pinned off an unvalidated MX RRset", calls[0].TLSPolicy.DaneRecords)
	}
	// The gate belongs BEFORE the fetch, mirroring the MTA-STS refusal gate
	// ("a refused host costs no TLSA fetch", smtp-server.md:500-502): a host
	// that can never be DANE-pinned should cost no RPC either.
	if n := c.tlsaCalls(); n != 0 {
		t.Fatalf("fetch_tlsa called %d time(s); an MX RRset that cannot support DANE must not be fetched for", n)
	}
}

// TestDaneSecureMxRrsetStillPins is the positive control for the gate: the
// SAME scripted records and the same hosts, differing only in the RRset's
// DNSSEC provenance, still pin. Without this the gate above could be
// satisfied by disabling DANE outright.
func TestDaneSecureMxRrsetStillPins(t *testing.T) {
	rec := daneEERecord()
	c := &daneCaller{
		mtaStsCaller: mtaStsCaller{
			queueCaller: queueCaller{batches: [][]wsrpc.OutboundUnit{{twoHostUnit()}, nil}},
			stsOutcome:  "not_published",
		},
		tlsaRecords: []wsrpc.TlsaRecordWire{rec},
	}
	calls := runDaneUnitMX(t, c, twoHostMX(), &stsSender{})
	if len(calls) != 1 || calls[0].TLSPolicy.Mode != TLSDanePinned {
		t.Fatalf("calls=%+v want TLSDanePinned on a DNSSEC-validated MX RRset", calls)
	}
}
