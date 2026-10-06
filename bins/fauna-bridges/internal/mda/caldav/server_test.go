package caldav

import (
	"bytes"
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

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
	"github.com/fxamacker/cbor/v2"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// Fixture parameters mirror libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs
// and the IMAP auth tests at internal/mda/imap/auth_test.go.
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
	// fixtureMSEK mirrors `libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs:44`
	// (`let msek = [0x11u8; 32]`). Lets the snapshot the test seals match
	// the MSEK the AUTH flow unwraps from `wrapped_msek.bin`.
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

// repoTestVectorsPath resolves the canonical fixtures directory.
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

// mockCaller is the wsrpc.Caller fake the CalDAV server tests use.
// Dispatches per-method with canned replies; collects every call so
// tests can assert call counts + per-method body shape.
type mockCaller struct {
	mu    sync.Mutex
	calls []recordedCall

	// Knobs the tests poke before driving requests.
	validateRecipientActor []byte // nil → "reject" outcome
	wrappedBlob            []byte // nil → "no blob on file"
	mlsPubkey              []byte
	// mlkemEk is the ML-KEM half served beside mlsPubkey. Unset → a valid ek
	// nothing in the test can decapsulate, which is enough for a test that
	// never re-opens what the session sealed; a round-trip test sets the half
	// that pairs with its snapshot (reportFixture.mlkemEk).
	mlkemEk  []byte
	indexKey []byte
	// mlsSnapshotBlob is the canonical-CBOR `MlsSnapshotBlob`
	// (encrypted under MSEK) `fetch_mls_snapshot_blob` returns; nil →
	// nest has no snapshot on file (user's primary client hasn't
	// provisioned one yet). AUTH still succeeds in that case so the
	// MUA can complete the well-known-URL probe.
	mlsSnapshotBlob []byte
	// listCalendarsReplies is a slice of replies the wsrpc.ListCalendars
	// stub returns in order — first call gets [0], second gets [1], etc.
	// Lets a test stage "empty list → lazy-Personal flow runs → list now
	// has one entry" without per-test state machinery.
	listCalendarsReplies [][]wsrpc.CalendarEntry
	listCalendarsCalls   int
	provisionOutcome     wsrpc.ProvisionCalendarOutcome

	// put_event_ciphertext canned reply. Default outcome "" maps to
	// PutEventCreated; ETag/EventID/Modseq are populated on the
	// Created/Updated path; CurrentETag is populated on the
	// PreconditionFailed path.
	putEventOutcome     wsrpc.PutEventCiphertextOutcome
	putEventID          []byte
	putEventETag        string
	putEventModseq      int64
	putEventCurrentETag string
	// putEventErrCode, when set, makes put_event_ciphertext fail with a nest
	// RpcError carrying this code (e.g. wsrpc.CodeOverQuota) instead of a reply.
	putEventErrCode string

	// delete_event canned reply. Default outcome "" maps to
	// DeleteEventDeleted; EventID/Modseq populate on Deleted;
	// CurrentETag populates on PreconditionFailed.
	deleteEventOutcome     wsrpc.DeleteEventOutcome
	deleteEventID          []byte
	deleteEventModseq      int64
	deleteEventCurrentETag string

	// query_events canned reply. Default outcome "" maps to
	// QueryEventsOk with the populated Events / HighestModseq / More
	// fields; QueryEventsCalendarNotFound suppresses the data fields.
	queryEventsOutcome       wsrpc.QueryEventsOutcome
	queryEventsEvents        []wsrpc.EventEntry
	queryEventsHighestModseq int64
	queryEventsMore          bool
	// queryEventsPager, when non-nil, overrides the static query_events reply
	// with a *paginated* one: it receives the request's after_event_id cursor +
	// limit and returns that page + whether more events follow. Lets a test drive
	// fetchAllEvents' bounded-pagination loop (§ B6) over a backing event list.
	queryEventsPager func(afterEventID []byte, limit uint32) (events []wsrpc.EventEntry, more bool)
	// queryEventsByCalendar, when non-nil, answers query_events PER CALENDAR
	// (key: hex calendar_id): a present key serves that calendar's events, an
	// absent one is CalendarNotFound. Lets a MOVE/COPY test give the source and
	// destination calendars different contents.
	queryEventsByCalendar map[string][]wsrpc.EventEntry

	// sync_calendar_since canned reply. Default outcome "" maps to
	// SyncCalendarSinceOk with Changed / Expunged / NewSyncToken /
	// More populated; SyncCalendarSinceCalendarNotFound clears them;
	// SyncCalendarSinceStale populates ServerModseq.
	syncCalendarSinceOutcome      wsrpc.SyncCalendarSinceOutcome
	syncCalendarSinceChanged      []wsrpc.EventEntry
	syncCalendarSinceExpunged     []wsrpc.ExpungedEntry
	syncCalendarSinceNewToken     string
	syncCalendarSinceMore         bool
	syncCalendarSinceServerModseq int64
	// syncCalendarSinceStale sets the `stale` flag on an Ok reply (token
	// behind the retention window → MDA emits DAV:valid-sync-token).
	syncCalendarSinceStale bool

	// Auto-schedule classification knobs (C4 — caldav-server.md § Server-side
	// auto-schedule). Drive the resolve_recipient → actor.by_handle →
	// keypackage.fetch chain the gateway classifies a local-domain attendee
	// with. resolveRecipientOutcome "" maps to "reject" (no mail route);
	// actorByHandleActorIDHex "" maps to fauna.actor.not_found; keypackageBytes
	// nil maps to key_package None.
	resolveRecipientOutcome string
	actorByHandleActorIDHex string
	keypackageBytes         []byte
	deliverSealedInboxID    int64
	deliverSealedSeq        int64
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

	case wsrpc.MethodListCalendars:
		var entries []wsrpc.CalendarEntry
		if m.listCalendarsCalls < len(m.listCalendarsReplies) {
			entries = m.listCalendarsReplies[m.listCalendarsCalls]
		}
		m.listCalendarsCalls++
		return encodeReply(struct {
			Calendars []wsrpc.CalendarEntry `cbor:"calendars"`
		}{Calendars: entries}, reply)

	case wsrpc.MethodProvisionCalendar:
		outcome := m.provisionOutcome
		if outcome == "" {
			outcome = wsrpc.ProvisionCalendarCreated
		}
		return encodeReply(struct {
			Outcome string `cbor:"outcome"`
		}{Outcome: string(outcome)}, reply)

	case wsrpc.MethodPutEventCiphertext:
		if m.putEventErrCode != "" {
			payload, err := cbor.Marshal(map[string]any{"code": m.putEventErrCode})
			if err != nil {
				return err
			}
			return &wsrpc.ServerError{Payload: payload}
		}
		outcome := m.putEventOutcome
		if outcome == "" {
			outcome = wsrpc.PutEventCreated
		}
		r := struct {
			Outcome     string `cbor:"outcome"`
			EventID     []byte `cbor:"event_id,omitempty"`
			ETag        string `cbor:"etag,omitempty"`
			Modseq      int64  `cbor:"modseq,omitempty"`
			CurrentETag string `cbor:"current_etag,omitempty"`
		}{Outcome: string(outcome)}
		switch outcome {
		case wsrpc.PutEventCreated, wsrpc.PutEventUpdated:
			r.EventID = m.putEventID
			r.ETag = m.putEventETag
			r.Modseq = m.putEventModseq
		case wsrpc.PutEventPreconditionFailed:
			r.CurrentETag = m.putEventCurrentETag
		}
		return encodeReply(r, reply)

	case wsrpc.MethodDeleteEvent:
		outcome := m.deleteEventOutcome
		if outcome == "" {
			outcome = wsrpc.DeleteEventDeleted
		}
		r := struct {
			Outcome     string `cbor:"outcome"`
			EventID     []byte `cbor:"event_id,omitempty"`
			Modseq      int64  `cbor:"modseq,omitempty"`
			CurrentETag string `cbor:"current_etag,omitempty"`
		}{Outcome: string(outcome)}
		switch outcome {
		case wsrpc.DeleteEventDeleted:
			r.EventID = m.deleteEventID
			r.Modseq = m.deleteEventModseq
		case wsrpc.DeleteEventPreconditionFailed:
			r.CurrentETag = m.deleteEventCurrentETag
		}
		return encodeReply(r, reply)

	case wsrpc.MethodQueryEvents:
		if m.queryEventsByCalendar != nil {
			var qreq struct {
				CalendarID []byte `cbor:"calendar_id"`
			}
			if err := cbor.Unmarshal(enc, &qreq); err != nil {
				return fmt.Errorf("mockCaller: decode query_events: %w", err)
			}
			events, ok := m.queryEventsByCalendar[hex.EncodeToString(qreq.CalendarID)]
			if !ok {
				return encodeReply(struct {
					Outcome string `cbor:"outcome"`
				}{Outcome: string(wsrpc.QueryEventsCalendarNotFound)}, reply)
			}
			return encodeReply(struct {
				Outcome string             `cbor:"outcome"`
				Events  []wsrpc.EventEntry `cbor:"events,omitempty"`
			}{Outcome: string(wsrpc.QueryEventsOk), Events: events}, reply)
		}
		if m.queryEventsPager != nil {
			var qreq struct {
				AfterEventID *[]byte `cbor:"after_event_id"`
				Limit        uint32  `cbor:"limit"`
			}
			if err := cbor.Unmarshal(enc, &qreq); err != nil {
				return fmt.Errorf("mockCaller: decode query_events: %w", err)
			}
			var after []byte
			if qreq.AfterEventID != nil {
				after = *qreq.AfterEventID
			}
			events, more := m.queryEventsPager(after, qreq.Limit)
			return encodeReply(struct {
				Outcome string             `cbor:"outcome"`
				Events  []wsrpc.EventEntry `cbor:"events,omitempty"`
				More    bool               `cbor:"more,omitempty"`
			}{Outcome: string(wsrpc.QueryEventsOk), Events: events, More: more}, reply)
		}
		outcome := m.queryEventsOutcome
		if outcome == "" {
			outcome = wsrpc.QueryEventsOk
		}
		r := struct {
			Outcome       string             `cbor:"outcome"`
			Events        []wsrpc.EventEntry `cbor:"events,omitempty"`
			HighestModseq int64              `cbor:"highestmodseq,omitempty"`
			More          bool               `cbor:"more,omitempty"`
		}{Outcome: string(outcome)}
		if outcome == wsrpc.QueryEventsOk {
			r.Events = m.queryEventsEvents
			r.HighestModseq = m.queryEventsHighestModseq
			r.More = m.queryEventsMore
		}
		return encodeReply(r, reply)

	case wsrpc.MethodSyncCalendarSince:
		outcome := m.syncCalendarSinceOutcome
		if outcome == "" {
			outcome = wsrpc.SyncCalendarSinceOk
		}
		r := struct {
			Outcome      string                `cbor:"outcome"`
			Changed      []wsrpc.EventEntry    `cbor:"changed,omitempty"`
			Expunged     []wsrpc.ExpungedEntry `cbor:"expunged,omitempty"`
			NewSyncToken string                `cbor:"new_sync_token,omitempty"`
			More         bool                  `cbor:"more,omitempty"`
			Stale        bool                  `cbor:"stale,omitempty"`
			ServerModseq int64                 `cbor:"server_modseq,omitempty"`
		}{Outcome: string(outcome)}
		switch outcome {
		case wsrpc.SyncCalendarSinceOk:
			r.Changed = m.syncCalendarSinceChanged
			r.Expunged = m.syncCalendarSinceExpunged
			r.NewSyncToken = m.syncCalendarSinceNewToken
			r.More = m.syncCalendarSinceMore
			r.Stale = m.syncCalendarSinceStale
		case wsrpc.SyncCalendarSinceStale:
			r.ServerModseq = m.syncCalendarSinceServerModseq
		}
		return encodeReply(r, reply)

	case wsrpc.MethodEnqueueOutboundMail:
		// The server-side auto-schedule gateway enqueues iMIP here. Canned
		// one-id reply; the call body is recorded for assertions.
		return encodeReply(map[string]any{"ids": []int64{1}}, reply)

	case wsrpc.MethodResolveRecipient:
		outcome := m.resolveRecipientOutcome
		if outcome == "" {
			outcome = "reject"
		}
		r := map[string]any{"outcome": outcome}
		switch outcome {
		case "resolved":
			r["actor_id"] = bytes.Repeat([]byte{0xa1}, 32)
		case "forward":
			r["forward_target"] = "fwd@example.org"
			r["forwarder_actor_id"] = bytes.Repeat([]byte{0xa2}, 32)
		default: // reject — no mail route
			r["smtp_code"] = uint16(550)
			r["reason"] = "User unknown"
		}
		return encodeReply(r, reply)

	case wsrpc.MethodActorByHandle:
		if m.actorByHandleActorIDHex == "" {
			// fauna.actor.not_found — a *ServerError so the wrapper's
			// RpcErrorCode maps it to ErrActorNotFound.
			payload, err := cbor.Marshal(map[string]any{"code": "fauna.actor.not_found"})
			if err != nil {
				return err
			}
			return &wsrpc.ServerError{Payload: payload}
		}
		return encodeReply(map[string]any{
			"actor_id":    m.actorByHandleActorIDHex,
			"handle":      "attendee",
			"domain":      "fauna.test",
			"addressable": true,
		}, reply)

	case wsrpc.MethodKeypackageFetch:
		r := struct {
			KeyPackage *[]byte `cbor:"key_package,omitempty"`
		}{}
		if m.keypackageBytes != nil {
			b := m.keypackageBytes
			r.KeyPackage = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodDeliverSealedScheduling:
		return encodeReply(map[string]any{
			"inbox_id": m.deliverSealedInboxID,
			"seq":      m.deliverSealedSeq,
		}, reply)
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

// equalBytes is a small helper mirroring the IMAP suite's helper so
// the snapshot-cache test reads symmetrically across the two packages.
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
		Subject:      pkix.Name{CommonName: "fauna-caldav-test"},
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
// Deliberately NO client Timeout. Every authenticated request runs the real
// Argon2id credential KDF server-side (inside UnwrapMLSBlob; see
// internal/mailfauna/dummy_kdf.go for its cost), and an auto-schedule PUT adds
// decrypt/seal work on top, so a fixed per-request deadline made the verdict a
// bet on how busy the machine was: a 5s one failed these tests at 9-15s on a
// loaded Windows host while the server was answering correctly. Assert state,
// never wall-clock (e2e convention 14). A genuinely hung handler still fails
// loudly: `go test`'s own -timeout panics with a goroutine dump naming the
// stuck test.
func httpsClient() *http.Client {
	return &http.Client{
		Transport: &http.Transport{
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		},
	}
}

// startServer wires a NewServer with the given caller, starts it on
// a TLS listener, and returns the dial-ready URL plus a cleanup
// func.
func startServer(t *testing.T, caller wsrpc.Caller) (string, func()) {
	t.Helper()
	ln := newTLSListener(t, selfSignedCert(t))
	// MailEnabled: true — the gateway/PUT tests exercise the email-enabled-nest
	// path (the common deployment); the email-disabled-nest sealed-rail routing is
	// pinned by autoschedule_test.go::emailDisabledNestLocalToSealedRail.
	// Stub the off-box cross-nest discovery to "email": the PUT/CANCEL harness
	// tests invite genuinely external attendees (example.com, not a Fauna nest),
	// so this keeps them on the email rail without a real anon-TLS probe. The
	// dedicated cross-nest sealed-rail routing is pinned by
	// autoschedule_test.go::TestClassifyAutoScheduleRecipientsCrossNest.
	srv := NewServer(ServerConfig{
		Logger:      slog.Default(),
		MailEnabled: true,
		ClassifyTransport: func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "email"}, nil
		},
	}, caller)
	go func() { _ = srv.Serve(ln) }()
	return "https://" + ln.Addr().String(), func() {
		_ = srv.Close()
		_ = ln.Close()
	}
}

// startServerPlaintext is startServer for a plaintext-mode deployment.
// Since Phase-3 D1 the server carries NO storage-mode knob at all (sealed
// both modes, one strict open at serve) — this helper is identical
// to startServer and is kept so the `*PlaintextModeSealsToo` pin reads as a
// deployment-mode statement.
func startServerPlaintext(t *testing.T, caller wsrpc.Caller) (string, func()) {
	t.Helper()
	ln := newTLSListener(t, selfSignedCert(t))
	srv := NewServer(ServerConfig{
		Logger:      slog.Default(),
		MailEnabled: true,
		ClassifyTransport: func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "email"}, nil
		},
	}, caller)
	go func() { _ = srv.Serve(ln) }()
	return "https://" + ln.Addr().String(), func() {
		_ = srv.Close()
		_ = ln.Close()
	}
}

// ── Integration tests ────────────────────────────────────────────

// TestServerRejectsAnonymousRequest verifies a PROPFIND with no
// Authorization header is rejected 401 with a WWW-Authenticate
// challenge per `caldav-server.md` § Authentication.
func TestServerRejectsAnonymousRequest(t *testing.T) {
	url, stop := startServer(t, &mockCaller{})
	defer stop()

	req, err := http.NewRequest("PROPFIND", url+"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
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
}

// TestServerRejectsBadCredentials verifies that valid Basic-Auth
// shape with a wrong password (AEAD-unwrap fails) returns 401.
// Per the goal doc the wrong-pubkey / wrong-username case all
// collapse to the same opaque 401.
func TestServerRejectsBadCredentials(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
	}
	url, stop := startServer(t, caller)
	defer stop()

	req, err := http.NewRequest("PROPFIND", url+"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
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
	// report_auth_event(fail) MUST fire so nest can rate-limit.
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times on fail, want 1", len(got))
	}
}

// TestServerBareUsernameAuthenticatesUnderPrimaryDomain drives the FULL
// server (TLS listener + the whole middleware chain) with a bare-username
// PROPFIND — the in-process reproduction of the macOS Calendar.app scenario
// (CalendarAgent sends only the local part as the Basic-auth username). With
// ServerConfig.PrimaryDomain set, that bare username must NOT 401 as
// "malformed username" (the live "Connecting…" hang); it must resolve under
// the primary domain. Exercises the ServerConfig → NewServer → auth-middleware
// threading the middleware-level test bypasses.
func TestServerBareUsernameAuthenticatesUnderPrimaryDomain(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
	}
	ln := newTLSListener(t, selfSignedCert(t))
	srv := NewServer(ServerConfig{Logger: slog.Default(), PrimaryDomain: fixtureDomain, MailEnabled: true}, caller)
	go func() { _ = srv.Serve(ln) }()
	defer func() { _ = srv.Close(); _ = ln.Close() }()
	url := "https://" + ln.Addr().String()

	req, err := http.NewRequest("PROPFIND", url+"/caldav/"+fixtureLocalPart+"/", nil)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart, string(fixturePlainPassword)) // bare "alice", no @domain
	req.Header.Set("Depth", "0")

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode == http.StatusUnauthorized {
		t.Fatalf("bare username must NOT 401 when PrimaryDomain is set (got 401 — the macOS Calendar hang)")
	}
	// AUTH ran: validate_recipient resolved under the defaulted domain.
	if got := caller.callsOf(wsrpc.MethodValidateRecipient); len(got) != 1 {
		t.Fatalf("validate_recipient fired %d times, want 1 (bare username must reach AUTH)", len(got))
	}
}

// TestServerSetsRequestTimeouts pins § B6: the CalDAV http.Server must
// time-bound the whole request (slow-loris body) and idle keep-alives, not just
// the header read — a slow-body PUT/REPORT otherwise holds a goroutine + buffer
// (size-bounded at 16 MiB, but not time-bounded) indefinitely.
func TestServerSetsRequestTimeouts(t *testing.T) {
	srv := NewServer(ServerConfig{Logger: slog.Default(), MailEnabled: true}, &mockCaller{})
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
// chain — the seam the shared-443 mux mounts under `/caldav/`. Driving it
// directly via httptest (no listener) must reject an anonymous PROPFIND 401,
// exactly like the listener-backed path. Twin of the CardDAV terminator's
// TestServerHandlerServesChain.
func TestServerHandlerServesChain(t *testing.T) {
	srv := NewServer(ServerConfig{Logger: slog.Default(), MailEnabled: true}, &mockCaller{})
	defer func() { _ = srv.Close() }()
	h := srv.Handler()
	if h == nil {
		t.Fatal("Handler() returned nil — the shared mux has nothing to mount")
	}
	req := httptest.NewRequest("PROPFIND", "/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("Handler() anonymous PROPFIND status = %d, want 401", rec.Code)
	}
}

// hrefInProp extracts the first <href> nested inside the named property
// block (namespace-prefix agnostic), mirroring the Python reproduction's
// `_href_in` in `tests/e2e-unified/tests/test_caldav_discovery_sequence.py`.
// Returns "" if the prop or its href is absent.
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

// mustPropfind drives one authenticated PROPFIND against the full server and
// returns the 207 multistatus body (failing the test on any non-207).
func mustPropfind(t *testing.T, baseURL, path, user, propXML, depth string) string {
	t.Helper()
	// `card` is declared unconditionally (harmless when unused) so a caller can
	// request a CardDAV-namespace prop like `<card:addressbook-home-set/>` on the
	// unified principal (see schedule_test.go).
	body := `<?xml version="1.0" encoding="utf-8"?>` +
		`<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop>` +
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

// TestServerDiscoveryWalkReachesCalendarHomeSet is the in-process Go twin of
// `test_caldav_discovery_sequence.py` (Gap 1c): the Apple-style RFC-6764 +
// RFC-5397 discovery walk — current-user-principal → calendar-home-set — that
// real CalDAV clients (macOS Calendar.app) perform and that every other test
// here SKIPS by pointing straight at `/caldav/{user}/`. It FOLLOWS the hrefs the
// server returns (does not hardcode them), so it pins the discovery *contract*
// independent of the exact principal-path shape.
//
// RED before the fix: emersion/go-webdav routes by path-segment DEPTH
// (`resourceTypeAtPath`, caldav/server.go), so a principal returned at a 2-segment
// path (`/principals/{u}@{d}/`) collides with the calendar-home-set's depth and a
// PROPFIND of it yields an EMPTY multistatus — the macOS-Calendar "Connecting…"
// stall. The fix returns the principal at a single-segment path so emersion's own
// `propFindUserPrincipal` (which serves calendar-home-set) fires.
func TestServerDiscoveryWalkReachesCalendarHomeSet(t *testing.T) {
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

	// Hop 2: PROPFIND the principal href → calendar-home-set. THE regression:
	// the depth-routing collision made this an empty multistatus, so a real
	// client could never retrieve the calendar home and stalled in discovery.
	princBody := mustPropfind(t, url, principal, user, "<c:calendar-home-set/>", "0")
	home := hrefInProp(princBody, "calendar-home-set")
	if home == "" || !strings.Contains(home, "/caldav/") {
		t.Fatalf("discovery hop 2: PROPFIND of the principal %q did not yield a calendar-home-set under /caldav/; got home=%q body=%s", principal, home, princBody)
	}
}

// TestAuthCachesMLSSnapshotPlaintext pins the I5 Phase F addition: the
// CalDAV auth middleware fetches the encrypted `MlsSnapshotBlob` via
// `fauna.bridges.fetch_mls_snapshot_blob`, AEAD-unwraps it under MSEK
// via the existing `MlsCapability.Decrypt`, and caches the plaintext
// bytes on the per-request Session so per-collection metadata-unseal
// (and the per-event open path in I5 Phase E.3) can open records
// without re-fetching.
//
// The snapshot must be a REAL canonical MlsSnapshotPlaintext (not the
// arbitrary-bytes `mls_snapshot.bin` vector): since Phase-3 S2 the AUTH
// flow parses it into the per-session record opener, and an
// unparseable snapshot fails AUTH (pinned by davauth's
// TestResolveUnparseableSnapshotFailsAuth). The opener is the only
// session-lifetime holder of the snapshot secrets (the raw-bytes copy
// + its MLSSnapshotBytes accessor were dropped 2026-07-13), so the
// assertion is opener-presence, not byte round-trip.
//
// We drive the middleware directly via httptest so the assertion can
// peek at the Session before the defer-Close path clears it.
func TestAuthCachesMLSSnapshotPlaintext(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	fx := newReportFixture(t)
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        fx.snapshotBlob,
	}

	var (
		gotOpener  bool
		gotActorID []byte
	)
	probe := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sess := davauth.SessionFromContext(r.Context())
		if sess == nil {
			t.Fatal("middleware did not attach Session to request context")
		}
		gotOpener = sess.RecordOpener() != nil
		gotActorID = sess.ActorID()
		w.WriteHeader(http.StatusOK)
	})
	mw := davauth.NewMiddleware(caldavRealm, probe, caller, slog.Default(), nil, nil)

	req := httptest.NewRequest("PROPFIND",
		"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want %d (middleware short-circuited the probe)", rec.Code, http.StatusOK)
	}
	if !equalBytes(gotActorID, fixtureActorID) {
		t.Fatalf("Session.ActorID = %x, want %x", gotActorID, fixtureActorID)
	}
	if !gotOpener {
		t.Fatal("Session.RecordOpener = nil, want the opener built from the sealed snapshot's plaintext at AUTH")
	}
	if got := caller.callsOf(wsrpc.MethodFetchMLSSnapshotBlob); len(got) != 1 {
		t.Fatalf("fetch_mls_snapshot_blob fired %d times, want 1", len(got))
	}
}

// TestAuthSucceedsWithoutMLSSnapshotProvisioned mirrors the IMAP gap
// test: the user's primary client hasn't provisioned an
// `MlsSnapshotBlob` yet, nest returns None. AUTH must still succeed
// so MUA discovery (PROPFIND against the principal URL, well-known
// URI rewrites) works; per-collection metadata-unseal surfaces the
// missing-snapshot error at call time instead of failing AUTH.
func TestAuthSucceedsWithoutMLSSnapshotProvisioned(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        nil, // not yet provisioned
	}

	var (
		probed     bool
		gotOpener  bool
		gotActorID []byte
	)
	probe := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sess := davauth.SessionFromContext(r.Context())
		if sess == nil {
			t.Fatal("middleware did not attach Session to request context")
		}
		probed = true
		gotOpener = sess.RecordOpener() != nil
		gotActorID = sess.ActorID()
		w.WriteHeader(http.StatusOK)
	})
	mw := davauth.NewMiddleware(caldavRealm, probe, caller, slog.Default(), nil, nil)

	req := httptest.NewRequest("PROPFIND",
		"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want %d (AUTH must succeed without a snapshot)", rec.Code, http.StatusOK)
	}
	if !probed {
		t.Fatal("downstream handler never ran — middleware short-circuited")
	}
	if !equalBytes(gotActorID, fixtureActorID) {
		t.Fatalf("Session.ActorID = %x, want %x (AUTH must complete)", gotActorID, fixtureActorID)
	}
	if gotOpener {
		t.Fatal("Session.RecordOpener must be nil when nest returns None")
	}
	if got := caller.callsOf(wsrpc.MethodFetchMLSSnapshotBlob); len(got) != 1 {
		t.Fatalf("fetch_mls_snapshot_blob fired %d times on missing-snapshot, want 1", len(got))
	}
}

// TestCalDAVMetadataDecryptRoundTrip is the CalDAV-side mail-record-
// open round-trip pin: the same MTA-shape seal primitive that
// produces calendar metadata blobs (`mailfauna.EncryptToRecipient`
// via `SealCollectionMetadata`) feeds into the MDA's PROPFIND
// metadata-unseal call site (`UnsealCollectionMetadata`); the
// "Personal" displayname must appear in the multistatus response.
//
// Mirrors the IMAP equivalent at
// `bins/fauna-bridges/internal/mda/imap/decrypt_e2e_test.go`
// (TestIMAPBodyDecryptRoundTrip), pinning the user-visible symptom
// (calendar missing from multistatus) end-to-end over HTTPS PROPFIND
// rather than the minimal `cap.OpenMailRecord` round-trip the IMAP
// sibling carries. Per `caldav-server.md` § Lazy "Personal" calendar.
func TestCalDAVMetadataDecryptRoundTrip(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	// Fresh leaf init keypair — same primitives the IMAP sibling test
	// uses. The MlsSnapshotPlaintext carrying only this keypair gets
	// served back by the mock caller via fetch_mls_snapshot_blob; the
	// AUTH middleware decrypts it via cap.Decrypt(snapshotBlob) and
	// caches the plaintext on the Session so UnsealCollectionMetadata
	// can hand it to OpenMailRecord.
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	// Seal the snapshot plaintext under the same MSEK the wrapped-
	// MSEK fixture wraps, so the AUTH flow's cap.Decrypt round-trips.
	// The MSEK is fixed by the gen example
	// (libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs:44).
	snapshotBlob, err := faunaFfi.SealMlsSnapshotBlob(
		snapshotPlaintext,
		fixtureActorID,
		fixtureMSEK,
	)
	if err != nil {
		t.Fatalf("SealMlsSnapshotBlob: %v", err)
	}

	// First ListCalendars returns empty; the lazy-Personal flow
	// then calls ProvisionCalendar; the second ListCalendars
	// returns the freshly-provisioned entry. The entry's
	// encrypted_metadata is sealed to the freshly-generated leaf
	// pubkey (the one whose secret rides on snapshotPlaintext).
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: defaultDisplayname,
		Color:       defaultColor,
	}, leaf.Pubkey, nil) // nil ek = classical seal
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	personalID := personalCalendarID()
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              leaf.Pubkey,
		mlsSnapshotBlob:        snapshotBlob,
		listCalendarsReplies: [][]wsrpc.CalendarEntry{
			{}, // first call: empty → triggers lazy-Personal
			{ // second call after provision: returns Personal
				{
					CalendarID:        personalID,
					EncryptedMetadata: sealed,
					CTag:              0,
					HighestModseq:     0,
					EventCount:        0,
					CreatedAt:         time.Now().Unix(),
				},
			},
		},
		provisionOutcome: wsrpc.ProvisionCalendarCreated,
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
	req, err := http.NewRequest("PROPFIND", url+"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", body)
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
	// Provision called exactly once (lazy-Personal trigger).
	if got := caller.callsOf(wsrpc.MethodProvisionCalendar); len(got) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(got))
	}
	// ListCalendars called twice: pre- and post-provision.
	if got := caller.callsOf(wsrpc.MethodListCalendars); len(got) != 2 {
		t.Fatalf("list_calendars fired %d times, want 2", len(got))
	}
	// Response body MUST mention the Personal calendar display
	// name so the MUA can render it.
	if !strings.Contains(string(respBody), defaultDisplayname) {
		t.Fatalf("response body missing %q displayname: %q", defaultDisplayname, respBody)
	}
	// And it MUST reference the calendar path under the user's
	// home set.
	expectPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" + hex.EncodeToString(personalID) + "/"
	if !strings.Contains(string(respBody), expectPath) {
		t.Fatalf("response body missing calendar path %q: %q", expectPath, respBody)
	}
}
