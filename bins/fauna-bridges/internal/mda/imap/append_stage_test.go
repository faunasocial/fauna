package imap

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// stagingAppendCaller is the APPEND-staging fake: it answers both RPCs the
// over-budget path fans out to — mint_bulk_byte_token (so StageSealedBody can
// mint a MailBody token) and append (capturing the wire request, body_ref
// included). Kept separate from append_test.go's appendCaller, which predates
// the reference leg and captures no body_ref (per the per-file-stub convention).
type stagingAppendCaller struct {
	reply wsrpc.AppendReply
	token string

	appendCalls int
	mintCalls   int
	got         capturedStagedAppendReq
}

// capturedStagedAppendReq decodes exactly the fields the reference leg touches.
// BodyRef reuses wsrpc.MailBodyRef so the decode is the encoder's own inverse.
type capturedStagedAppendReq struct {
	EncryptedBody  []byte             `cbor:"encrypted_body"`
	CiphertextSize uint32             `cbor:"ciphertext_size"`
	BodyRef        *wsrpc.MailBodyRef `cbor:"body_ref,omitempty"`
}

func (a *stagingAppendCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodMintBulkByteToken:
		a.mintCalls++
		rep, err := dagcbor.Marshal(map[string]any{"token": a.token, "expires_at": uint64(0)})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodAppend:
		a.appendCalls++
		if err := cbor.Unmarshal(enc, &a.got); err != nil {
			return err
		}
		rep, err := dagcbor.Marshal(a.reply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("stagingAppendCaller: unexpected method " + method)
}

// chunkSink is an httptest byte-plane endpoint: it 201-accepts every chunk POST
// and echoes back the X-Content-Hash the client declared (the store key), which
// is all doUpload reads. Counts the uploads so a test can assert the body was
// actually staged, not just referenced.
func chunkSink(t *testing.T, uploads *int) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/v1/chunks" || r.Method != http.MethodPost {
			http.Error(w, "unexpected", http.StatusNotFound)
			return
		}
		*uploads++
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"hash":"` + r.Header.Get("X-Content-Hash") + `"}`))
	}))
	t.Cleanup(srv.Close)
	return srv
}

// bigMessage builds a valid RFC 5322 message whose body is `approxBytes` of
// SPACE-SEPARATED filler — a tiny unique-word set so the sealed index hint stays
// well inside the inline budget (only the body needs staging). A single giant
// unbroken word would instead balloon the hint and trip errOverInlineBudget.
func bigMessage(approxBytes int) []byte {
	const word = "lorem ipsum dolor sit amet "
	var b strings.Builder
	b.WriteString("From: Alice <alice@example.org>\r\n")
	b.WriteString("To: Bob <bob@example.com>\r\n")
	b.WriteString("Subject: big\r\n")
	b.WriteString("Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n")
	b.WriteString("\r\n")
	for b.Len() < approxBytes {
		b.WriteString(word)
	}
	b.WriteString("\r\n")
	return []byte(b.String())
}

// TestAppendStagesOverBudgetBodyByReference is the LEAD success criterion (S9):
// an APPEND whose sealed body exceeds the inline WS-RPC budget must cross on the
// bulk-byte plane by reference instead of riding inline and severing the 2 MiB
// frame. The wire request carries an empty encrypted_body and a body_ref whose
// total_bytes equals the sealed size, and a MailBody token was minted + the
// chunks uploaded (smtp-server.md § Message size limits — the MDA-APPEND leg).
func TestAppendStagesOverBudgetBodyByReference(t *testing.T) {
	var uploads int
	sink := chunkSink(t, &uploads)

	caller := &stagingAppendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 5, UIDValidity: 3},
		token: "mailbody-token",
	}
	sess := authedSession(t, &appendCaller{}, fixtureIndexKey) // reuse fixture wiring
	sess.client = caller
	sess.plane = byteplane.New(sink.URL, http.DefaultClient)

	// ~3 MiB raw → sealed body comfortably over the 2,031,616-byte inline budget,
	// so it stages rather than riding inline.
	msg := bigMessage(3 * 1024 * 1024)
	data, err := sess.Append("INBOX", newLiteralReader(msg), nil)
	if err != nil {
		t.Fatalf("over-budget APPEND must stage and succeed, got: %v", err)
	}
	if data == nil {
		t.Fatal("Append must return AppendData on success")
	}
	if caller.appendCalls != 1 {
		t.Fatalf("append fired %d times, want 1", caller.appendCalls)
	}
	if caller.mintCalls != 1 {
		t.Fatalf("mint_bulk_byte_token fired %d times, want 1 (a MailBody token per staging)", caller.mintCalls)
	}
	if uploads == 0 {
		t.Fatal("the sealed body must be uploaded to the byte plane as at least one chunk")
	}
	if len(caller.got.EncryptedBody) != 0 {
		t.Errorf("encrypted_body must be empty when the body rode by reference, got %d bytes",
			len(caller.got.EncryptedBody))
	}
	if caller.got.BodyRef == nil {
		t.Fatal("an over-budget APPEND must carry a body_ref")
	}
	if len(caller.got.BodyRef.ChunkHashes) == 0 {
		t.Error("body_ref must name at least one chunk")
	}
	// ciphertext_size stays the true sealed length even though the body left the
	// request; body_ref.total_bytes is the same sealed length by construction.
	if caller.got.CiphertextSize == 0 || uint64(caller.got.CiphertextSize) != caller.got.BodyRef.TotalBytes {
		t.Errorf("ciphertext_size (%d) must equal body_ref.total_bytes (%d), both the sealed size",
			caller.got.CiphertextSize, caller.got.BodyRef.TotalBytes)
	}
	if uint64(caller.got.CiphertextSize) <= uint64(mailfauna.InlineMailRequestBudgetBytes()) {
		t.Errorf("sealed size %d should exceed the inline budget %d (else it wouldn't stage)",
			caller.got.CiphertextSize, mailfauna.InlineMailRequestBudgetBytes())
	}
}

// TestAppendInlineBodyCarriesNoBodyRef is the byte-identical-pre-field-shape
// guarantee: the overwhelmingly common small APPEND still rides inline, with no
// body_ref and no mint — the reference leg is invisible to it. A nil plane is
// fine here precisely because the inline path never touches the byte plane.
func TestAppendInlineBodyCarriesNoBodyRef(t *testing.T) {
	caller := &stagingAppendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, &appendCaller{}, fixtureIndexKey)
	sess.client = caller
	sess.plane = nil

	if _, err := sess.Append("INBOX", newLiteralReader([]byte(sampleRFC5322)), nil); err != nil {
		t.Fatalf("small inline APPEND: %v", err)
	}
	if caller.mintCalls != 0 {
		t.Errorf("a small inline APPEND must mint no token, got %d mints", caller.mintCalls)
	}
	if caller.got.BodyRef != nil {
		t.Errorf("a small inline APPEND must carry no body_ref")
	}
	if len(caller.got.EncryptedBody) == 0 {
		t.Error("a small inline APPEND must carry the sealed body inline")
	}
	if uint64(caller.got.CiphertextSize) != uint64(len(caller.got.EncryptedBody)) {
		t.Errorf("ciphertext_size %d != len(encrypted_body) %d for an inline APPEND",
			caller.got.CiphertextSize, len(caller.got.EncryptedBody))
	}
}

// TestAppendOverProductCeilingMapsToBad pins the permanent-size branch since
// ceiling retirement: IMAP APPEND has no SMTP perimeter clamp, so nest is its
// authoritative size gate — an over-`max_message_bytes` APPEND comes back as the
// typed `fauna.email.too_large` RpcError, which the MDA maps to an IMAP BAD (a
// client upload the MUA must not retry), never a retryable NO
// (smtp-server.md § Message size limits). The mapping is body-size-independent,
// so a small literal + a nest that returns the typed error isolates it.
func TestAppendOverProductCeilingMapsToBad(t *testing.T) {
	payload, err := dagcbor.Marshal(map[string]any{
		"code": wsrpc.CodeMessageTooLarge,
		"message": map[string]any{
			"key":  "error.email.too_large",
			"args": map[string]string{},
		},
		"details": "message of 60000000 bytes exceeds the 50065536-byte ceiling",
	})
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	caller := &appendCaller{wireErr: &wsrpc.ServerError{Payload: payload}}
	sess := authedSession(t, caller, fixtureIndexKey)

	_, err = sess.Append("INBOX", newLiteralReader([]byte(sampleRFC5322)), nil)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) {
		t.Fatalf("over-ceiling APPEND must yield *imap.Error, got %T: %v", err, err)
	}
	if imapErr.Type != imap.StatusResponseTypeBad {
		t.Errorf("status type = %v, want BAD (a permanent size failure the MUA must not retry)", imapErr.Type)
	}
}

// TestAppendFormerlyOverAtRestCeilingNowStagesAndDelivers is the direct
// change-detector for ceiling retirement on the APPEND leg: a ~9 MiB body — over
// the old ~8.1 MB at-rest ceiling, which used to be refused BAD before it ever
// staged — now stages on the bulk-byte plane and reaches the append RPC, because
// StageSealedBody no longer has an upper ceiling and continuation records rest a
// body of any size. Under the shipped 50 MB `max_message_bytes` it delivers.
func TestAppendFormerlyOverAtRestCeilingNowStagesAndDelivers(t *testing.T) {
	var uploads int
	sink := chunkSink(t, &uploads)

	caller := &stagingAppendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 9, UIDValidity: 1},
		token: "mailbody-token",
	}
	sess := authedSession(t, &appendCaller{}, fixtureIndexKey)
	sess.client = caller
	sess.plane = byteplane.New(sink.URL, http.DefaultClient)

	msg := bigMessage(9 * 1024 * 1024)
	data, err := sess.Append("INBOX", newLiteralReader(msg), nil)
	if err != nil {
		t.Fatalf("a ~9 MiB APPEND must now stage and succeed (the at-rest ceiling is retired), got: %v", err)
	}
	if data == nil {
		t.Fatal("Append must return AppendData on success")
	}
	if caller.appendCalls != 1 {
		t.Errorf("append fired %d times, want 1 (the body now reaches the RPC)", caller.appendCalls)
	}
	if caller.mintCalls != 1 {
		t.Errorf("mint_bulk_byte_token fired %d times, want 1 (staged as a MailBody reference)", caller.mintCalls)
	}
	if uploads == 0 {
		t.Fatal("the sealed body must be uploaded to the byte plane as at least one chunk")
	}
	if caller.got.BodyRef == nil {
		t.Fatal("a ~9 MiB APPEND must carry a body_ref (it staged)")
	}
}
