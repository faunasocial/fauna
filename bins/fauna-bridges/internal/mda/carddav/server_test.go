package carddav

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/hex"
	"fmt"
	"io"
	"log/slog"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
	"github.com/fxamacker/cbor/v2"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// Fixture parameters mirror the CalDAV server-test fixtures
// (internal/mda/caldav/server_test.go) + davauth's, which in turn mirror
// libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs, so `wrapped_msek.bin`
// unwraps under fixturePlainPassword to the fixture actor's MSEK.
var (
	fixtureActorID = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0x42
		}
		return b
	}()
	fixtureCredentialID  = "default"
	fixturePlainPassword = []byte("deterministic-pw")
	fixtureMLSPubkey     = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0xAA
		}
		return b
	}()
	fixtureIndexKey = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0xBB
		}
		return b
	}()
	// fixtureMSEK mirrors libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs:44
	// (`let msek = [0x11u8; 32]`). Lets a snapshot the test seals match the MSEK
	// the AUTH flow unwraps from `wrapped_msek.bin`.
	fixtureMSEK = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0x11
		}
		return b
	}()
	fixtureLocalPart = "alice"
	fixtureDomain    = "example.com"
)

// repoTestVectorsPath resolves the canonical fixtures directory. carddav sits at
// the same directory depth as caldav (internal/mda/<pkg>/), so the same 5-`..`
// climb reaches the repo root.
func repoTestVectorsPath(name string) string {
	return filepath.Join("..", "..", "..", "..", "..",
		"libs", "fauna-protocol", "schemas", "test_vectors", name)
}

func mustReadFixture(t *testing.T, name string) []byte {
	t.Helper()
	p := repoTestVectorsPath(name)
	b, err := os.ReadFile(p)
	if err != nil {
		t.Fatalf("read fixture %s: %v", p, err)
	}
	return b
}

// ── Programmable mock caller ─────────────────────────────────────

// mockCaller is the wsrpc.Caller fake the CardDAV server tests use. It is a
// carddav-package-private copy of the caldav/davauth pattern (auth-flow subset +
// the six CardDAV RPCs), dispatching per-method with canned replies and
// collecting every call so tests can assert counts + per-method body shape. The
// reply shapes match the wsrpc funcs' decode structs byte-for-byte.
type mockCaller struct {
	mu    sync.Mutex
	calls []recordedCall

	// Auth-flow knobs (identical to caldav/davauth).
	validateRecipientActor []byte // nil → "reject" outcome
	wrappedBlob            []byte // nil → "no blob on file"
	mlsPubkey              []byte
	// mlkemEk is the ML-KEM half served beside mlsPubkey. Unset → a valid ek
	// nothing in the test can decapsulate, which is enough for a test that
	// never re-opens what the session sealed; a round-trip test sets the half
	// that pairs with its snapshot (reportFixture.mlkemEk).
	mlkemEk         []byte
	indexKey        []byte
	mlsSnapshotBlob []byte // nil → nest has no snapshot on file

	// list_addressbooks canned replies, served in order (first call → [0], …).
	// Lets a test stage "empty → lazy-Contacts flow runs → list now has one
	// entry" without per-test state machinery.
	listAddressbooksReplies [][]wsrpc.AddressbookEntry
	listAddressbooksCalls   int
	provisionOutcome        wsrpc.ProvisionAddressbookOutcome

	// put_card_ciphertext canned reply. Default outcome "" maps to
	// PutCardCreated; card_id/etag/modseq populate on Created/Updated;
	// current_etag on PreconditionFailed.
	putCardOutcome     wsrpc.PutCardCiphertextOutcome
	putCardID          []byte
	putCardETag        string
	putCardModseq      int64
	putCardCurrentETag string
	// putCardErrCode, when set, makes put_card_ciphertext fail with a nest
	// RpcError carrying this code (e.g. wsrpc.CodeOverQuota) instead of a reply.
	putCardErrCode string

	// delete_card canned reply. Default outcome "" maps to DeleteCardDeleted.
	deleteCardOutcome     wsrpc.DeleteCardOutcome
	deleteCardID          []byte
	deleteCardModseq      int64
	deleteCardCurrentETag string

	// delete_addressbook canned reply. Default outcome "" maps to
	// DeleteAddressbookDeleted; cards_deleted populates on Deleted.
	deleteAddressbookOutcome      wsrpc.DeleteAddressbookOutcome
	deleteAddressbookCardsDeleted uint32

	// query_cards canned reply. Default outcome "" maps to QueryCardsOk.
	queryCardsOutcome       wsrpc.QueryCardsOutcome
	queryCardsCards         []wsrpc.CardEntry
	queryCardsHighestModseq int64
	queryCardsMore          bool
	// queryCardsPager, when non-nil, overrides the static reply with a paginated
	// one keyed on the after_card_id cursor + limit — drives fetchAllCards'
	// bounded-pagination loop.
	queryCardsPager func(afterCardID []byte, limit uint32) (cards []wsrpc.CardEntry, more bool)

	// sync_addressbook_since canned reply. Default outcome "" maps to
	// SyncAddressbookSinceOk.
	syncOutcome      wsrpc.SyncAddressbookSinceOutcome
	syncChanged      []wsrpc.CardEntry
	syncExpunged     []wsrpc.ExpungedCardEntry
	syncNewToken     string
	syncMore         bool
	syncServerModseq int64
	syncStale        bool
}

type recordedCall struct {
	method string
	body   []byte
}

func (m *mockCaller) Call(_ context.Context, method string, body, reply any) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	enc, err := cbor.Marshal(body)
	if err != nil {
		return fmt.Errorf("mockCaller: encode body: %w", err)
	}
	m.calls = append(m.calls, recordedCall{method: method, body: enc})

	switch method {
	case wsrpc.MethodValidateRecipient:
		if m.validateRecipientActor == nil {
			return encodeReply(map[string]any{"outcome": "reject", "reason": "unknown"}, reply)
		}
		return encodeReply(map[string]any{
			"outcome":  "resolved",
			"actor_id": m.validateRecipientActor,
		}, reply)

	case wsrpc.MethodFetchWrappedMLSBlob:
		r := struct {
			Blob *[]byte `cbor:"blob"`
		}{}
		if m.wrappedBlob != nil {
			b := m.wrappedBlob
			r.Blob = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchRecipientMLSPubkey:
		r := struct {
			Key *map[string][]byte `cbor:"key"`
		}{}
		if m.mlsPubkey != nil {
			key := wsrpctest.RecipientSealKey(m.mlsPubkey)
			if m.mlkemEk != nil {
				key["mlkem_ek"] = m.mlkemEk
			}
			r.Key = &key
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchRecipientIndexKey:
		r := struct {
			Pubkey *[]byte `cbor:"pubkey"`
		}{}
		if m.indexKey != nil {
			b := m.indexKey
			r.Pubkey = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchMLSSnapshotBlob:
		r := struct {
			Blob *[]byte `cbor:"blob"`
		}{}
		if m.mlsSnapshotBlob != nil {
			b := m.mlsSnapshotBlob
			r.Blob = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodReportAuthEvent:
		return encodeReply(map[string]any{"ok": true}, reply)

	case wsrpc.MethodListAddressbooks:
		var entries []wsrpc.AddressbookEntry
		if m.listAddressbooksCalls < len(m.listAddressbooksReplies) {
			entries = m.listAddressbooksReplies[m.listAddressbooksCalls]
		}
		m.listAddressbooksCalls++
		return encodeReply(struct {
			Addressbooks []wsrpc.AddressbookEntry `cbor:"addressbooks"`
		}{Addressbooks: entries}, reply)

	case wsrpc.MethodProvisionAddressbook:
		outcome := m.provisionOutcome
		if outcome == "" {
			outcome = wsrpc.ProvisionAddressbookCreated
		}
		return encodeReply(struct {
			Outcome string `cbor:"outcome"`
		}{Outcome: string(outcome)}, reply)

	case wsrpc.MethodPutCardCiphertext:
		if m.putCardErrCode != "" {
			payload, err := cbor.Marshal(map[string]any{"code": m.putCardErrCode})
			if err != nil {
				return err
			}
			return &wsrpc.ServerError{Payload: payload}
		}
		outcome := m.putCardOutcome
		if outcome == "" {
			outcome = wsrpc.PutCardCreated
		}
		r := struct {
			Outcome     string `cbor:"outcome"`
			CardID      []byte `cbor:"card_id,omitempty"`
			ETag        string `cbor:"etag,omitempty"`
			Modseq      int64  `cbor:"modseq,omitempty"`
			CurrentETag string `cbor:"current_etag,omitempty"`
		}{Outcome: string(outcome)}
		switch outcome {
		case wsrpc.PutCardCreated, wsrpc.PutCardUpdated:
			r.CardID = m.putCardID
			r.ETag = m.putCardETag
			r.Modseq = m.putCardModseq
		case wsrpc.PutCardPreconditionFailed:
			r.CurrentETag = m.putCardCurrentETag
		}
		return encodeReply(r, reply)

	case wsrpc.MethodDeleteCard:
		outcome := m.deleteCardOutcome
		if outcome == "" {
			outcome = wsrpc.DeleteCardDeleted
		}
		r := struct {
			Outcome     string `cbor:"outcome"`
			CardID      []byte `cbor:"card_id,omitempty"`
			Modseq      int64  `cbor:"modseq,omitempty"`
			CurrentETag string `cbor:"current_etag,omitempty"`
		}{Outcome: string(outcome)}
		switch outcome {
		case wsrpc.DeleteCardDeleted:
			r.CardID = m.deleteCardID
			r.Modseq = m.deleteCardModseq
		case wsrpc.DeleteCardPreconditionFailed:
			r.CurrentETag = m.deleteCardCurrentETag
		}
		return encodeReply(r, reply)

	case wsrpc.MethodDeleteAddressbook:
		outcome := m.deleteAddressbookOutcome
		if outcome == "" {
			outcome = wsrpc.DeleteAddressbookDeleted
		}
		r := struct {
			Outcome      string `cbor:"outcome"`
			CardsDeleted uint32 `cbor:"cards_deleted,omitempty"`
		}{Outcome: string(outcome)}
		if outcome == wsrpc.DeleteAddressbookDeleted {
			r.CardsDeleted = m.deleteAddressbookCardsDeleted
		}
		return encodeReply(r, reply)

	case wsrpc.MethodQueryCards:
		if m.queryCardsPager != nil {
			var qreq struct {
				AfterCardID *[]byte `cbor:"after_card_id"`
				Limit       uint32  `cbor:"limit"`
			}
			if err := cbor.Unmarshal(enc, &qreq); err != nil {
				return fmt.Errorf("mockCaller: decode query_cards: %w", err)
			}
			var after []byte
			if qreq.AfterCardID != nil {
				after = *qreq.AfterCardID
			}
			cards, more := m.queryCardsPager(after, qreq.Limit)
			return encodeReply(struct {
				Outcome string            `cbor:"outcome"`
				Cards   []wsrpc.CardEntry `cbor:"cards,omitempty"`
				More    bool              `cbor:"more,omitempty"`
			}{Outcome: string(wsrpc.QueryCardsOk), Cards: cards, More: more}, reply)
		}
		outcome := m.queryCardsOutcome
		if outcome == "" {
			outcome = wsrpc.QueryCardsOk
		}
		r := struct {
			Outcome       string            `cbor:"outcome"`
			Cards         []wsrpc.CardEntry `cbor:"cards,omitempty"`
			HighestModseq int64             `cbor:"highestmodseq,omitempty"`
			More          bool              `cbor:"more,omitempty"`
		}{Outcome: string(outcome)}
		if outcome == wsrpc.QueryCardsOk {
			r.Cards = m.queryCardsCards
			r.HighestModseq = m.queryCardsHighestModseq
			r.More = m.queryCardsMore
		}
		return encodeReply(r, reply)

	case wsrpc.MethodSyncAddressbookSince:
		outcome := m.syncOutcome
		if outcome == "" {
			outcome = wsrpc.SyncAddressbookSinceOk
		}
		r := struct {
			Outcome      string                    `cbor:"outcome"`
			Changed      []wsrpc.CardEntry         `cbor:"changed,omitempty"`
			Expunged     []wsrpc.ExpungedCardEntry `cbor:"expunged,omitempty"`
			NewSyncToken string                    `cbor:"new_sync_token,omitempty"`
			More         bool                      `cbor:"more,omitempty"`
			Stale        bool                      `cbor:"stale,omitempty"`
			ServerModseq int64                     `cbor:"server_modseq,omitempty"`
		}{Outcome: string(outcome)}
		switch outcome {
		case wsrpc.SyncAddressbookSinceOk:
			r.Changed = m.syncChanged
			r.Expunged = m.syncExpunged
			r.NewSyncToken = m.syncNewToken
			r.More = m.syncMore
			r.Stale = m.syncStale
		case wsrpc.SyncAddressbookSinceStale:
			r.ServerModseq = m.syncServerModseq
		}
		return encodeReply(r, reply)
	}
	return fmt.Errorf("mockCaller: unexpected method %q", method)
}

func encodeReply(value, reply any) error {
	if reply == nil {
		return nil
	}
	enc, err := cbor.Marshal(value)
	if err != nil {
		return fmt.Errorf("encode reply: %w", err)
	}
	return cbor.Unmarshal(enc, reply)
}

// equalBytes mirrors the caldav/imap suite helper.
func equalBytes(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func (m *mockCaller) callsOf(method string) []recordedCall {
	m.mu.Lock()
	defer m.mu.Unlock()
	var out []recordedCall
	for _, c := range m.calls {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

// ── TLS scaffolding ──────────────────────────────────────────────

func selfSignedCert(t *testing.T) tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("GenerateKey: %v", err)
	}
	template := x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "fauna-carddav-test"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		DNSNames:     []string{"localhost"},
		IPAddresses:  []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("CreateCertificate: %v", err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key, Leaf: &template}
}

func newTLSListener(t *testing.T, cert tls.Certificate) net.Listener {
	t.Helper()
	ln, err := tls.Listen("tcp", "127.0.0.1:0", &tls.Config{Certificates: []tls.Certificate{cert}})
	if err != nil {
		t.Fatalf("tls.Listen: %v", err)
	}
	return ln
}

// httpsClient returns a client that trusts self-signed certs (test
// transport only).
//
// Deliberately NO client Timeout, for the same reason as caldav's twin: every
// authenticated request runs the real Argon2id credential KDF server-side
// (inside UnwrapMLSBlob; see internal/mailfauna/dummy_kdf.go for its cost), so
// a fixed per-request deadline made the verdict a bet on how busy the machine
// was — a 5s one failed a DELETE at 13.85s on a loaded Windows host while the
// server answered 204. Assert state, never wall-clock (e2e convention 14). A
// genuinely hung handler still fails loudly: `go test`'s own -timeout panics
// with a goroutine dump naming the stuck test.
func httpsClient() *http.Client {
	return &http.Client{
		Transport: &http.Transport{
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		},
	}
}

// startServer wires a NewServer with the given caller, starts it on a TLS
// listener, and returns the dial-ready URL plus a cleanup func.
func startServer(t *testing.T, caller wsrpc.Caller) (string, func()) {
	t.Helper()
	ln := newTLSListener(t, selfSignedCert(t))
	srv := NewServer(ServerConfig{Logger: slog.Default()}, caller)
	go func() { _ = srv.Serve(ln) }()
	return "https://" + ln.Addr().String(), func() {
		_ = srv.Close()
		_ = ln.Close()
	}
}

// ── Integration tests ────────────────────────────────────────────

// TestServerRejectsAnonymousRequest verifies a PROPFIND with no Authorization
// header is rejected 401 with a WWW-Authenticate challenge.
func TestServerRejectsAnonymousRequest(t *testing.T) {
	url, stop := startServer(t, &mockCaller{})
	defer stop()

	req, err := http.NewRequest("PROPFIND", url+"/carddav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.Header.Set("Depth", "1")

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	if got := resp.Header.Get("WWW-Authenticate"); !strings.Contains(got, "Basic") {
		t.Fatalf("WWW-Authenticate = %q, want a Basic challenge", got)
	}
	if !strings.Contains(resp.Header.Get("WWW-Authenticate"), carddavRealm) {
		t.Fatalf("WWW-Authenticate = %q, want realm %q", resp.Header.Get("WWW-Authenticate"), carddavRealm)
	}
}

// TestServerRejectsBadCredentials verifies a valid Basic-Auth shape with a wrong
// password (AEAD-unwrap fails) returns 401 and fires report_auth_event(fail).
func TestServerRejectsBadCredentials(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
	}
	url, stop := startServer(t, caller)
	defer stop()

	req, err := http.NewRequest("PROPFIND", url+"/carddav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, "WRONG-PASSWORD")
	req.Header.Set("Depth", "1")

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times on fail, want 1", len(got))
	}
}

// TestServerBareUsernameAuthenticatesUnderPrimaryDomain drives the full server
// with a bare-username PROPFIND: with ServerConfig.PrimaryDomain set, a bare
// username must resolve under it (not 401 as "malformed username").
func TestServerBareUsernameAuthenticatesUnderPrimaryDomain(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
	}
	ln := newTLSListener(t, selfSignedCert(t))
	srv := NewServer(ServerConfig{Logger: slog.Default(), PrimaryDomain: fixtureDomain}, caller)
	go func() { _ = srv.Serve(ln) }()
	defer func() { _ = srv.Close(); _ = ln.Close() }()
	url := "https://" + ln.Addr().String()

	req, err := http.NewRequest("PROPFIND", url+"/carddav/"+fixtureLocalPart+"/", nil)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart, string(fixturePlainPassword)) // bare "alice"
	req.Header.Set("Depth", "0")

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode == http.StatusUnauthorized {
		t.Fatalf("bare username must NOT 401 when PrimaryDomain is set (got 401)")
	}
	if got := caller.callsOf(wsrpc.MethodValidateRecipient); len(got) != 1 {
		t.Fatalf("validate_recipient fired %d times, want 1 (bare username must reach AUTH)", len(got))
	}
}

// TestServerSetsRequestTimeouts pins that the CardDAV http.Server time-bounds
// the whole request + idle keep-alives, not just the header read.
func TestServerSetsRequestTimeouts(t *testing.T) {
	srv := NewServer(ServerConfig{Logger: slog.Default()}, &mockCaller{})
	defer func() { _ = srv.Close() }()
	if got := srv.inner.ReadHeaderTimeout; got != readHeaderTimeout {
		t.Errorf("ReadHeaderTimeout = %v, want %v", got, readHeaderTimeout)
	}
	if got := srv.inner.ReadTimeout; got != readTimeout {
		t.Errorf("ReadTimeout = %v, want %v", got, readTimeout)
	}
	if got := srv.inner.WriteTimeout; got != writeTimeout {
		t.Errorf("WriteTimeout = %v, want %v", got, writeTimeout)
	}
	if got := srv.inner.IdleTimeout; got != idleTimeout {
		t.Errorf("IdleTimeout = %v, want %v", got, idleTimeout)
	}
}

// TestServerHandlerServesChain proves Handler() returns the full auth+backend
// chain — the seam Slice 2d's shared-443 mux mounts under `/carddav/`. Driving
// it directly via httptest (no listener) must reject an anonymous PROPFIND 401,
// exactly like the listener-backed path.
func TestServerHandlerServesChain(t *testing.T) {
	srv := NewServer(ServerConfig{Logger: slog.Default()}, &mockCaller{})
	defer func() { _ = srv.Close() }()
	h := srv.Handler()
	if h == nil {
		t.Fatal("Handler() returned nil — Slice 2d's shared mux has nothing to mount")
	}
	req := httptest.NewRequest("PROPFIND", "/carddav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("Handler() anonymous PROPFIND status = %d, want 401", rec.Code)
	}
}

// hrefInProp extracts the first <href> nested inside the named property block
// (namespace-prefix agnostic).
func hrefInProp(xmlBody, propTag string) string {
	block := regexp.MustCompile(`(?is)<[^>]*\b` + propTag + `\b[^>]*>(.*?)</[^>]*\b` + propTag + `\b[^>]*>`).
		FindStringSubmatch(xmlBody)
	if block == nil {
		return ""
	}
	href := regexp.MustCompile(`(?is)<[^>]*\bhref\b[^>]*>([^<]+)</[^>]*\bhref\b[^>]*>`).
		FindStringSubmatch(block[1])
	if href == nil {
		return ""
	}
	return strings.TrimSpace(href[1])
}

// mustPropfind drives one authenticated PROPFIND and returns the 207 body.
func mustPropfind(t *testing.T, baseURL, path, user, propXML, depth string) string {
	t.Helper()
	body := `<?xml version="1.0" encoding="utf-8"?>` +
		`<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop>` +
		propXML + `</d:prop></d:propfind>`
	req, err := http.NewRequest("PROPFIND", baseURL+path, strings.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest %s: %v", path, err)
	}
	req.SetBasicAuth(user, string(fixturePlainPassword))
	req.Header.Set("Depth", depth)
	req.Header.Set("Content-Type", "application/xml; charset=utf-8")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do %s: %v", path, err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("PROPFIND %s: status %d (want 207); body=%s", path, resp.StatusCode, b)
	}
	return string(b)
}

// TestServerDiscoveryWalkReachesAddressBookHomeSet is the CardDAV twin of the
// CalDAV Gap-1c discovery test: current-user-principal → addressbook-home-set,
// FOLLOWING the hrefs the server returns. Pins that the single-segment principal
// shape lets emersion's own propFindUserPrincipal serve addressbook-home-set (a
// 2-segment principal would collide with the home-set depth and yield an empty
// multistatus).
func TestServerDiscoveryWalkReachesAddressBookHomeSet(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
	}
	url, stop := startServer(t, caller)
	defer stop()
	user := fixtureLocalPart + "@" + fixtureDomain

	// Hop 1: discovery root → current-user-principal href.
	rootBody := mustPropfind(t, url, "/", user, "<d:current-user-principal/>", "0")
	principal := hrefInProp(rootBody, "current-user-principal")
	if principal == "" {
		t.Fatalf("discovery hop 1: root PROPFIND returned no current-user-principal href; body=%s", rootBody)
	}

	// Hop 2: PROPFIND the principal href → addressbook-home-set.
	princBody := mustPropfind(t, url, principal, user, "<c:addressbook-home-set/>", "0")
	home := hrefInProp(princBody, "addressbook-home-set")
	if home == "" || !strings.Contains(home, "/carddav/") {
		t.Fatalf("discovery hop 2: PROPFIND of the principal %q did not yield an addressbook-home-set under /carddav/; got home=%q body=%s", principal, home, princBody)
	}
}

// TestCardDAVMetadataDecryptRoundTrip is the CardDAV-side mail-record-open
// round-trip pin (twin of caldav's TestCalDAVMetadataDecryptRoundTrip): the
// same MTA-shape seal primitive that produces addressbook metadata blobs feeds
// into the MDA's PROPFIND metadata-unseal call site; the lazy "Contacts"
// displayname must appear in the multistatus response. Exercises REAL MLS crypto
// end-to-end over HTTPS PROPFIND, including the lazy-Contacts provision flow.
func TestCardDAVMetadataDecryptRoundTrip(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	snapshotBlob, err := faunaFfi.SealMlsSnapshotBlob(
		snapshotPlaintext, fixtureActorID, fixtureMSEK,
	)
	if err != nil {
		t.Fatalf("SealMlsSnapshotBlob: %v", err)
	}

	// Seal the Contacts metadata to the freshly-generated leaf pubkey (the one
	// whose secret rides on snapshotPlaintext).
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: defaultDisplayname,
	}, leaf.Pubkey, nil) // nil ek = classical seal
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	contactsID := contactsAddressbookID()
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              leaf.Pubkey,
		mlsSnapshotBlob:        snapshotBlob,
		listAddressbooksReplies: [][]wsrpc.AddressbookEntry{
			{}, // first call: empty → triggers lazy-Contacts
			{ // second call after provision: returns Contacts
				{
					AddressbookID:     contactsID,
					EncryptedMetadata: sealed,
					CTag:              0,
					HighestModseq:     0,
					CardCount:         0,
					CreatedAt:         time.Now().Unix(),
				},
			},
		},
		provisionOutcome: wsrpc.ProvisionAddressbookCreated,
	}
	url, stop := startServer(t, caller)
	defer stop()

	body := strings.NewReader(`<?xml version="1.0" encoding="utf-8"?>
<propfind xmlns="DAV:">
  <prop>
    <displayname/>
    <resourcetype/>
  </prop>
</propfind>`)
	req, err := http.NewRequest("PROPFIND", url+"/carddav/"+fixtureLocalPart+"@"+fixtureDomain+"/", body)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	req.Header.Set("Depth", "1")
	req.Header.Set("Content-Type", "application/xml; charset=utf-8")

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	// Provision called exactly once (lazy-Contacts trigger).
	if got := caller.callsOf(wsrpc.MethodProvisionAddressbook); len(got) != 1 {
		t.Fatalf("provision_addressbook fired %d times, want 1", len(got))
	}
	// ListAddressbooks called twice: pre- and post-provision.
	if got := caller.callsOf(wsrpc.MethodListAddressbooks); len(got) != 2 {
		t.Fatalf("list_addressbooks fired %d times, want 2", len(got))
	}
	if !strings.Contains(string(respBody), defaultDisplayname) {
		t.Fatalf("response body missing %q displayname: %q", defaultDisplayname, respBody)
	}
	expectPath := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain + "/" + hex.EncodeToString(contactsID) + "/"
	if !strings.Contains(string(respBody), expectPath) {
		t.Fatalf("response body missing address-book path %q: %q", expectPath, respBody)
	}
}
