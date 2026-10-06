package davauth

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
)

// ── Fixtures ─────────────────────────────────────────────────────
//
// These mirror the CalDAV server-test fixtures (internal/mda/caldav/server_test.go)
// and the IMAP auth tests (internal/mda/imap/auth_test.go), which in turn mirror
// libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs, so the `wrapped_msek.bin`
// test vector unwraps under fixturePlainPassword to the fixture actor's MSEK.
var (
	fixtureActorID = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0x42
		}
		return b
	}()
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
	fixtureLocalPart = "alice"
	fixtureDomain    = "example.com"
)

// repoTestVectorsPath resolves the canonical fixtures directory. davauth sits at
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

// ── Programmable mock caller (auth-flow subset) ──────────────────
//
// mockCaller is the wsrpc.Caller fake the davauth auth tests use. It answers
// only the six RPCs the auth flow drives (validate_recipient,
// fetch_wrapped_mls_blob, fetch_recipient_mls_pubkey, fetch_recipient_index_key,
// fetch_mls_snapshot_blob, report_auth_event) — the caldav handler tests keep
// their own broader mock in caldav/server_test.go for the calendar RPCs. Reply
// shapes here match that mock byte-for-byte so the AUTH round-trip is identical.
// Collects every call so tests can assert counts + per-method body shape.
type mockCaller struct {
	mu    sync.Mutex
	calls []recordedCall

	// Knobs the tests poke before driving requests.
	validateRecipientActor []byte // nil → "reject" outcome
	wrappedBlob            []byte // nil → "no blob on file"
	mlsPubkey              []byte
	indexKey               []byte
	// mlsSnapshotBlob is the canonical-CBOR `MlsSnapshotBlob` (encrypted under
	// MSEK) `fetch_mls_snapshot_blob` returns; nil → nest has no snapshot on
	// file. AUTH still succeeds in that case.
	mlsSnapshotBlob []byte
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
