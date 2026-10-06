package imap

import (
	"bytes"
	"context"
	"crypto/mlkem"
	"errors"
	"io"
	"log/slog"
	"strings"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
)

// appendCaller is the per-file fake for the APPEND path. Dispatches
// the single RPC Session.Append fans out to:
//
//   - fauna.bridges.append: the encrypted-body / encrypted-hint upload
//     plus mailbox / flags / timestamp / sender_domain metadata.
//
// By convention, every IMAP-layer test file declares its
// own caller stub so future drift on one file's fake doesn't take the
// rest of the suite down with it.
type appendCaller struct {
	// fixtures
	reply   wsrpc.AppendReply
	wireErr error
	// sealPubkey, when set, is served — with freshMlkemEk as its ML-KEM
	// half — for the per-APPEND fetch_recipient_mls_pubkey
	// (mail_new_ingest=true) seal-key fetch. Unset → the method errors and
	// Append degrades to the session-cached standing key (the never-bounce
	// fallback most tests ride).
	sealPubkey []byte

	// captured state
	calls              int
	gotActor           []byte
	gotMailbox         string
	gotFlags           []string
	gotBody            []byte
	gotHint            []byte
	gotTimestamp       int64
	gotCSize           uint32
	gotDomain          string
	gotSealFetchIngest *bool
}

type capturedAppendReq struct {
	ActorID            []byte   `cbor:"actor_id"`
	Mailbox            string   `cbor:"mailbox"`
	Flags              []string `cbor:"flags"`
	EncryptedBody      []byte   `cbor:"encrypted_body"`
	EncryptedIndexHint []byte   `cbor:"encrypted_index_hint"`
	Timestamp          int64    `cbor:"timestamp"`
	CiphertextSize     uint32   `cbor:"ciphertext_size"`
	SenderDomain       string   `cbor:"sender_domain"`
}

func (a *appendCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodFetchRecipientMLSPubkey:
		var req struct {
			ActorID       []byte `cbor:"actor_id"`
			MailNewIngest bool   `cbor:"mail_new_ingest"`
		}
		if err := cbor.Unmarshal(enc, &req); err != nil {
			return err
		}
		a.gotSealFetchIngest = &req.MailNewIngest
		if a.sealPubkey == nil {
			return errors.New("appendCaller: no seal pubkey fixture (degrade to cached)")
		}
		rep, err := dagcbor.Marshal(struct {
			Key map[string][]byte `cbor:"key"`
		}{Key: map[string][]byte{"mls_pubkey": a.sealPubkey, "mlkem_ek": freshMlkemEk}})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodAppend:
		a.calls++
		var req capturedAppendReq
		if err := cbor.Unmarshal(enc, &req); err != nil {
			return err
		}
		a.gotActor = req.ActorID
		a.gotMailbox = req.Mailbox
		a.gotFlags = req.Flags
		a.gotBody = req.EncryptedBody
		a.gotHint = req.EncryptedIndexHint
		a.gotTimestamp = req.Timestamp
		a.gotCSize = req.CiphertextSize
		a.gotDomain = req.SenderDomain
		if a.wireErr != nil {
			return a.wireErr
		}
		rep, err := dagcbor.Marshal(a.reply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("appendCaller: unexpected method " + method)
}

// The two ML-KEM halves the APPEND tests tell apart: fixtureMlkemEk is cached
// on the session at AUTH beside fixtureMLSPubkey, freshMlkemEk rides the
// per-APPEND fetch beside sealPubkey. Both are valid encapsulation keys, so
// the real seal behind the recorder seals X-Wing to them.
var (
	fixtureMlkemEk = wsrpctest.RecipientMlkemEk()
	freshMlkemEk   = func() []byte {
		dk, err := mlkem.GenerateKey768()
		if err != nil {
			panic(err)
		}
		return dk.EncapsulationKey().Bytes()
	}()
)

// staticLiteralReader satisfies imap.LiteralReader from an in-memory
// byte slice. emersion calls Read until EOF and reports Size() in
// CAPABILITY responses; the test path doesn't exercise the latter for
// real but the interface requires it.
type staticLiteralReader struct {
	r io.Reader
	n int64
}

func newLiteralReader(b []byte) *staticLiteralReader {
	return &staticLiteralReader{r: strings.NewReader(string(b)), n: int64(len(b))}
}

func (s *staticLiteralReader) Read(p []byte) (int, error) { return s.r.Read(p) }
func (s *staticLiteralReader) Size() int64                { return s.n }

// authedSession builds a Session with all post-AUTH state populated.
// Caller picks whether to provide an index key (nil → Phase E gap).
func authedSession(t *testing.T, caller *appendCaller, indexKey []byte) *Session {
	t.Helper()
	cap := mustUnwrapPlainFixture(t)
	return &Session{
		client:         caller,
		actorID:        fixtureActorID,
		credentialID:   fixtureCredentialID,
		mlsUnwrap:      cap,
		actorMLSPubkey: fixtureMLSPubkey,
		actorMlkemEk:   fixtureMlkemEk,
		actorIndexKey:  indexKey,
		logger:         slog.Default(),
	}
}

// sampleRFC5322 is a minimal RFC 5322 message exercising the From-
// header → sender_domain extraction path.
const sampleRFC5322 = "From: Alice <alice@example.org>\r\n" +
	"To: Bob <bob@example.com>\r\n" +
	"Subject: hello\r\n" +
	"Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n" +
	"\r\n" +
	"hello world from APPEND test\r\n"

// ── Tests ─────────────────────────────────────────────────────────

// installSealRecorder swaps the package-level sealToRecipient with a
// recording wrapper that captures every (plaintext, pubkey) call while
// still delegating to the real seal so the wire-side ciphertext stays
// realistic.  Returns a pointer to the captured calls.  Tests use
// t.Cleanup to restore the original.
type sealCall struct {
	plaintext []byte
	pubkey    []byte
	mlkemEk   []byte
}

func installSealRecorder(t *testing.T) *[]sealCall {
	t.Helper()
	calls := &[]sealCall{}
	prev := sealToRecipient
	sealToRecipient = func(plaintext, pubkey, mlkemEk []byte) ([]byte, error) {
		// Defensive copies so post-call mutation by Session.Append can't
		// retroactively change recorded values.
		ptCopy := make([]byte, len(plaintext))
		copy(ptCopy, plaintext)
		pkCopy := make([]byte, len(pubkey))
		copy(pkCopy, pubkey)
		var ekCopy []byte
		if mlkemEk != nil {
			ekCopy = make([]byte, len(mlkemEk))
			copy(ekCopy, mlkemEk)
		}
		*calls = append(*calls, sealCall{plaintext: ptCopy, pubkey: pkCopy, mlkemEk: ekCopy})
		return prev(plaintext, pubkey, mlkemEk)
	}
	t.Cleanup(func() { sealToRecipient = prev })
	return calls
}

// TestAppendSealsBodyToActorMLSPubkey pins the seal-to-self contract
// for the body: the encrypted body lands at nest after sealing to the
// AUTH'd actor's own MLS pubkey (cached on the Session at AUTH time).
// Verified via a seal-recorder seam — the FFI's HPKE-open counterpart
// is not exposed in the Go bindings (see `unseal_mail_record` in
// `libs/fauna-mls/src/wrapped_blob/mod.rs`), so a round-trip-via-Go
// decrypt isn't available; the seam captures the exact (plaintext,
// pubkey) tuple the seal was called with.
func TestAppendSealsBodyToActorMLSPubkey(t *testing.T) {
	seals := installSealRecorder(t)
	caller := &appendCaller{
		reply: wsrpc.AppendReply{
			MessageID:   make([]byte, 32),
			UID:         42,
			UIDValidity: 7,
		},
	}
	sess := authedSession(t, caller, fixtureIndexKey)

	data, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)),
		&imap.AppendOptions{Flags: []imap.Flag{imap.FlagSeen, imap.FlagDraft}},
	)
	if err != nil {
		t.Fatalf("Append: %v", err)
	}
	if data == nil {
		t.Fatal("Append must return AppendData on success")
	}
	if caller.calls != 1 {
		t.Fatalf("fauna.bridges.append fired %d times, want 1", caller.calls)
	}
	// Wire shape sanity.
	if !equalBytes(caller.gotActor, fixtureActorID) {
		t.Errorf("actor_id: %x", caller.gotActor)
	}
	if caller.gotMailbox != "INBOX" {
		t.Errorf("mailbox: %q", caller.gotMailbox)
	}
	if caller.gotCSize != uint32(len(caller.gotBody)) {
		t.Errorf("ciphertext_size %d != len(encrypted_body) %d",
			caller.gotCSize, len(caller.gotBody))
	}
	if len(caller.gotBody) == 0 {
		t.Fatal("encrypted_body must not be empty")
	}
	if !equalBytes(caller.gotBody, []byte(sampleRFC5322)) == false {
		// `equalBytes` returns true on match — we expect ciphertext
		// to DIFFER from plaintext.
		t.Fatal("encrypted_body must not equal the plaintext literal")
	}
	// Seam: first seal is for the body, plaintext == the raw RFC 5322
	// literal, pubkey == actor's MLS pubkey.
	if len(*seals) < 1 {
		t.Fatalf("expected at least one seal call, got %d", len(*seals))
	}
	body := (*seals)[0]
	if string(body.plaintext) != sampleRFC5322 {
		t.Errorf("body seal plaintext = %q (len=%d), want sampleRFC5322 (len=%d)",
			body.plaintext, len(body.plaintext), len(sampleRFC5322))
	}
	if !equalBytes(body.pubkey, fixtureMLSPubkey) {
		t.Errorf("body seal pubkey = %x, want fixtureMLSPubkey = %x",
			body.pubkey, fixtureMLSPubkey)
	}
	// The body seals to both halves of the actor's key: the ek cached beside
	// the MLS pubkey reaches the seal.
	if !equalBytes(body.mlkemEk, fixtureMlkemEk) {
		t.Errorf("body seal must receive the actor's ML-KEM ek, got len=%d", len(body.mlkemEk))
	}
}

// TestAppendSealsBodyToXWingWhenEkPublished pins the leg-D2b plumbing: when
// the AUTH'd actor has a published ML-KEM ek on the session, APPEND threads it
// to the BODY seal (selecting X-Wing). The index hint here stays classical
// because this session has a DEDICATED index key (fixtureIndexKey ≠ the MLS
// pubkey) whose own ML-KEM ek is unpublished (Phase E) — so IndexHintMlkemEk
// withholds the body's ek (it would not pair with the dedicated index key).
// The fallback case (index key == MLS pubkey → hint also seals X-Wing) is
// TestAppendSealsHintHybridOnFallbackWhenEkPublished. Verified via the
// seal-recorder seam, which captures the (plaintext, pubkey, ek) tuple each
// seal was called with BEFORE delegating — so it pins the threading independent
// of the ek's crypto validity (an arbitrary ek degrades to classical inside
// EncryptToRecipientHybrid (PQ-4b), but the ek still reached the body seal,
// which is what this test asserts).
func TestAppendSealsBodyToXWingWhenEkPublished(t *testing.T) {
	seals := installSealRecorder(t)
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, fixtureIndexKey)
	ek := make([]byte, 1184)
	for i := range ek {
		ek[i] = byte(i)
	}
	sess.actorMlkemEk = ek

	if _, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if len(*seals) != 2 {
		t.Fatalf("expected 2 seal calls (body + hint), got %d", len(*seals))
	}
	body := (*seals)[0]
	if !equalBytes(body.pubkey, fixtureMLSPubkey) {
		t.Errorf("body seal pubkey = %x, want fixtureMLSPubkey", body.pubkey)
	}
	if !equalBytes(body.mlkemEk, ek) {
		t.Errorf("body seal must receive the published 1184-byte ek (got len=%d)", len(body.mlkemEk))
	}
	hint := (*seals)[1]
	if hint.mlkemEk != nil {
		t.Errorf("index-hint seal to a DEDICATED index key must stay classical "+
			"(its own ek is unpublished until Phase E), got len=%d", len(hint.mlkemEk))
	}
}

// TestAppendSealsHintHybridOnFallbackWhenEkPublished pins PQ-6 on the leg-D2b
// APPEND path: on the Phase-E fallback (no dedicated index key → the hint seals
// to the actor's MLS pubkey), a published ML-KEM ek threads to BOTH the body AND
// the index-hint seal, so the hint seals X-Wing too and no longer leaks the
// plaintext subject+body word-set under HNDL. The ek pairs with the sealed-to
// key because the fallback key IS the MLS pubkey (IndexHintMlkemEk). Counterpart
// of the dedicated-key case in TestAppendSealsBodyToXWingWhenEkPublished (which
// keeps the hint classical). On the pre-PQ-6 code the hint seal received a
// literal nil ek, so the hint.mlkemEk assertion here was RED.
func TestAppendSealsHintHybridOnFallbackWhenEkPublished(t *testing.T) {
	seals := installSealRecorder(t)
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, nil) // Phase E gap → hint key falls back to the MLS pubkey
	ek := make([]byte, 1184)
	for i := range ek {
		ek[i] = byte(i)
	}
	sess.actorMlkemEk = ek

	if _, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if len(*seals) != 2 {
		t.Fatalf("expected 2 seal calls (body + hint), got %d", len(*seals))
	}
	body, hint := (*seals)[0], (*seals)[1]
	// Both seal to the MLS pubkey (the body's standing key == the hint's fallback key)…
	if !equalBytes(body.pubkey, fixtureMLSPubkey) {
		t.Errorf("body seal pubkey = %x, want fixtureMLSPubkey", body.pubkey)
	}
	if !equalBytes(hint.pubkey, fixtureMLSPubkey) {
		t.Errorf("fallback hint seal pubkey = %x, want fixtureMLSPubkey", hint.pubkey)
	}
	// …and BOTH receive the published ek, so both seal X-Wing (PQ-6).
	if !equalBytes(body.mlkemEk, ek) {
		t.Errorf("body seal must receive the published ek (got len=%d)", len(body.mlkemEk))
	}
	if !equalBytes(hint.mlkemEk, ek) {
		t.Errorf("PQ-6: fallback hint seal must receive the published ek (hybrid), got len=%d",
			len(hint.mlkemEk))
	}
}

// TestAppendReturnsAppendDataFromReply pins the UIDPLUS path: the nest
// reply's (uid_validity, uid) flow through to imap.AppendData so
// emersion emits `OK [APPENDUID <validity> <uid>] APPEND completed`
// per RFC 4315.
func TestAppendReturnsAppendDataFromReply(t *testing.T) {
	caller := &appendCaller{
		reply: wsrpc.AppendReply{
			MessageID:   []byte{1, 2, 3, 4},
			UID:         101,
			UIDValidity: 9001,
		},
	}
	sess := authedSession(t, caller, fixtureIndexKey)

	data, err := sess.Append("Drafts",
		newLiteralReader([]byte(sampleRFC5322)),
		nil,
	)
	if err != nil {
		t.Fatalf("Append: %v", err)
	}
	if data == nil {
		t.Fatal("Append must return AppendData on success")
	}
	if uint32(data.UID) != 101 {
		t.Errorf("AppendData.UID = %d, want 101", data.UID)
	}
	if data.UIDValidity != 9001 {
		t.Errorf("AppendData.UIDValidity = %d, want 9001", data.UIDValidity)
	}
}

// countingLiteralReader announces a size and counts the bytes read from it,
// serving a zero byte per read so a test can tell "refused unread" apart
// from "buffered, then refused".
type countingLiteralReader struct {
	announced int64
	read      int64
}

func (c *countingLiteralReader) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	p[0] = 0
	c.read++
	return 1, nil
}
func (c *countingLiteralReader) Size() int64 { return c.announced }

// TestAppendRefusesALiteralOverTheCeilingUnread pins the repeat-walk fix's adjacent
// check: a literal announcing more than MaxMessageBytesCeiling is refused
// with BAD before a byte of it is buffered, so a MUA cannot make the MDA
// hold an arbitrarily large literal in memory ahead of nest's size gate.
func TestAppendRefusesALiteralOverTheCeilingUnread(t *testing.T) {
	caller := &appendCaller{}
	sess := authedSession(t, caller, fixtureIndexKey)
	lit := &countingLiteralReader{announced: int64(mailfauna.MaxMessageBytesCeiling()) + 1}

	_, err := sess.Append("INBOX", lit, nil)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) || imapErr.Type != imap.StatusResponseTypeBad {
		t.Fatalf("Append over the ceiling = %v, want an IMAP BAD", err)
	}
	if lit.read != 0 {
		t.Errorf("read %d bytes of an over-ceiling literal, want 0", lit.read)
	}
}

// TestAppendEncryptsHintToIndexKeyWhenProvisioned pins the seal-to-
// self hint contract: when actorIndexKey is set, the hint ciphertext
// is sealed to the index pubkey (NOT the MLS pubkey).  Verified via
// the seal-recorder seam.
func TestAppendEncryptsHintToIndexKeyWhenProvisioned(t *testing.T) {
	seals := installSealRecorder(t)
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, fixtureIndexKey)

	if _, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if len(*seals) != 2 {
		t.Fatalf("expected 2 seal calls (body + hint), got %d", len(*seals))
	}
	hint := (*seals)[1]
	if !equalBytes(hint.pubkey, fixtureIndexKey) {
		t.Errorf("hint seal pubkey = %x, want fixtureIndexKey = %x",
			hint.pubkey, fixtureIndexKey)
	}
	if equalBytes(hint.pubkey, fixtureMLSPubkey) {
		t.Errorf("hint seal pubkey must NOT equal MLS pubkey when index key is provisioned")
	}
}

// TestAppendFallsBackToMLSPubkeyForHintWhenIndexKeyNil pins the
// Phase E gap fallback: production index-pubkey provisioning is a
// Phase E concern.  Until then the MDA, like the MTA's inbound path,
// reuses the actor's MLS pubkey for the hint envelope so APPEND
// works end-to-end today.  Per `mta/server.go::ingestForRecipient`'s
// fallback shape (the canonical pattern this mirrors).
func TestAppendFallsBackToMLSPubkeyForHintWhenIndexKeyNil(t *testing.T) {
	seals := installSealRecorder(t)
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, nil) // Phase E gap

	if _, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if len(*seals) != 2 {
		t.Fatalf("expected 2 seal calls (body + hint), got %d", len(*seals))
	}
	// Fallback path: hint sealed to the SAME pubkey as the body
	// (the MLS pubkey).
	body, hint := (*seals)[0], (*seals)[1]
	if !equalBytes(body.pubkey, fixtureMLSPubkey) {
		t.Errorf("body seal pubkey = %x, want fixtureMLSPubkey = %x",
			body.pubkey, fixtureMLSPubkey)
	}
	if !equalBytes(hint.pubkey, fixtureMLSPubkey) {
		t.Errorf("fallback hint seal pubkey = %x, want fixtureMLSPubkey = %x",
			hint.pubkey, fixtureMLSPubkey)
	}
}

// TestAppendExtractsSenderDomainFromFromHeader pins the sender_domain
// floor metadata: APPEND has no SMTP envelope, so the sender domain
// comes from the parsed RFC 5322 From: header.
func TestAppendExtractsSenderDomainFromFromHeader(t *testing.T) {
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, fixtureIndexKey)

	if _, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if caller.gotDomain != "example.org" {
		t.Errorf("sender_domain = %q, want %q", caller.gotDomain, "example.org")
	}
}

// TestAppendFilesAMessageWithTwoFromFields pins APPEND's side of the From-field
// rule (smtp-server.md § Architectural rules): APPEND files the account's own
// bytes into its own mailbox and no authentication verdict is derived from
// their From, so unlike the sending and receiving doors it does not refuse on
// the count — refusing would strand mail a user is importing. Its
// sender_domain is the LAST From field's, the same field every app displays.
func TestAppendFilesAMessageWithTwoFromFields(t *testing.T) {
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, fixtureIndexKey)
	raw := "From: Alice <alice@example.org>\r\n" +
		"From: Carol <carol@example.net>\r\n" +
		"To: Bob <bob@example.com>\r\n" +
		"Subject: two From fields\r\n" +
		"\r\n" +
		"filed as the account's own bytes\r\n"

	if _, err := sess.Append("INBOX", newLiteralReader([]byte(raw)), nil); err != nil {
		t.Fatalf("Append must file a message with two From fields; got %v", err)
	}
	if caller.gotDomain != "example.net" {
		t.Errorf("sender_domain = %q, want %q (the last From field)", caller.gotDomain, "example.net")
	}
}

// TestAppendStripsForgedFaunaStampsBeforeSealing pins the APPEND door of
// smtp-server.md § Architectural rules → The X-Fauna-* namespace: the sealed
// plaintext (observed through the seal-recorder seam) carries no reserved
// delivery stamp a MUA supplied — the SELECT-time scorer would otherwise read
// the forged threshold as this message's Junk tier — while the forward-loop
// trace and a substring look-alike survive. The sender domain still comes
// from the From field of the stripped literal.
func TestAppendStripsForgedFaunaStampsBeforeSealing(t *testing.T) {
	seals := installSealRecorder(t)
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, fixtureIndexKey)
	raw := "X-Fauna-Spam-Threshold: 0\r\n" +
		"x-fauna-address-suffix: forged\r\n\tcontinued\r\n" +
		"X-Fauna-Forwarded-By: actor=peer; t=1; rule=forward-all\r\n" +
		"X-Not-Fauna: keepme\r\n" +
		"From: Alice <alice@example.org>\r\n" +
		"To: Bob <bob@example.com>\r\n" +
		"Subject: reserved stamps\r\n" +
		"\r\n" +
		"filed as the account's own bytes\r\n"

	if _, err := sess.Append("INBOX", newLiteralReader([]byte(raw)), nil); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if len(*seals) < 1 {
		t.Fatalf("expected at least one seal call, got %d", len(*seals))
	}
	sealed := (*seals)[0].plaintext
	for _, forged := range []string{"X-Fauna-Spam-Threshold", "x-fauna-address-suffix", "forged", "continued"} {
		if bytes.Contains(sealed, []byte(forged)) {
			t.Errorf("sealed plaintext still carries %q (forged X-Fauna-* stamp not stripped):\n%s", forged, sealed)
		}
	}
	for _, keep := range []string{"X-Fauna-Forwarded-By: actor=peer", "X-Not-Fauna: keepme", "filed as the account's own bytes"} {
		if !bytes.Contains(sealed, []byte(keep)) {
			t.Errorf("sealed plaintext dropped %q (over-stripped):\n%s", keep, sealed)
		}
	}
	if caller.gotDomain != "example.org" {
		t.Errorf("sender_domain = %q, want %q", caller.gotDomain, "example.org")
	}
}

// TestAppendTranslatesFlags pins the emersion Flag → []string mapping:
// `\Seen` rides as "\\Seen" on the wire (the bridge_routing.rs
// AppendMessageRequest.flags shape is an exact mirror of IMAP flag
// atoms).
func TestAppendTranslatesFlags(t *testing.T) {
	caller := &appendCaller{
		reply: wsrpc.AppendReply{MessageID: make([]byte, 32), UID: 1, UIDValidity: 1},
	}
	sess := authedSession(t, caller, fixtureIndexKey)

	if _, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)),
		&imap.AppendOptions{Flags: []imap.Flag{imap.FlagSeen, imap.FlagFlagged}},
	); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if len(caller.gotFlags) != 2 ||
		caller.gotFlags[0] != "\\Seen" ||
		caller.gotFlags[1] != "\\Flagged" {
		t.Errorf("flags: %v, want [\\Seen \\Flagged]", caller.gotFlags)
	}
}

// TestAppendPropagatesUnderlyingError pins the failure-passthrough:
// a transport / nest-side error must surface to emersion so the IMAP
// wire response is a tagged NO (or BAD for malformed) rather than a
// silent OK.
func TestAppendPropagatesUnderlyingError(t *testing.T) {
	wantErr := errors.New("synthetic-append-fail")
	caller := &appendCaller{wireErr: wantErr}
	sess := authedSession(t, caller, fixtureIndexKey)

	_, err := sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	)
	if err == nil || !strings.Contains(err.Error(), "synthetic-append-fail") {
		t.Fatalf("err: %v", err)
	}
}

// TestAppendOverQuotaMapsToNoOverQuota pins the RFC 9208 enforcement
// translation: when the nest append handler returns the typed
// `fauna.bridges.over_quota` RpcError, the MDA must surface it as an
// IMAP `NO [OVERQUOTA]` status response (not a bare NO), per
// imap-server.md § Quota enforcement points.
func TestAppendOverQuotaMapsToNoOverQuota(t *testing.T) {
	// A realistic ok=false payload: the RpcError map the nest emits.
	payload, err := dagcbor.Marshal(map[string]any{
		"code": wsrpc.CodeOverQuota,
		"message": map[string]any{
			"key":  "error.bridges.over_quota",
			"args": map[string]string{},
		},
		"details": "storage quota exceeded",
	})
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	caller := &appendCaller{wireErr: &wsrpc.ServerError{Payload: payload}}
	sess := authedSession(t, caller, fixtureIndexKey)

	_, err = sess.Append("INBOX",
		newLiteralReader([]byte(sampleRFC5322)), nil,
	)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) {
		t.Fatalf("over-quota append must yield *imap.Error, got %T: %v", err, err)
	}
	if imapErr.Type != imap.StatusResponseTypeNo {
		t.Errorf("status type = %v, want NO", imapErr.Type)
	}
	if imapErr.Code != imap.ResponseCodeOverQuota {
		t.Errorf("response code = %q, want OVERQUOTA", imapErr.Code)
	}
}

// TestAppendSealsToPerSealFetchedPubkey pins the flip-checklist line-4
// contract (content-sealing-epochs): APPEND's seal key
// comes from a per-APPEND fetch_recipient_mls_pubkey with
// mail_new_ingest=true — NOT the session cache, whose AUTH-time fetch is
// deliberately mail_new_ingest=false for the DAV/spam-reseal sites. Both
// body and hint (Phase-E fallback) must seal to the freshly fetched key —
// both halves of it, never the fresh pubkey paired with the cached ek.
func TestAppendSealsToPerSealFetchedPubkey(t *testing.T) {
	fresh := bytes.Repeat([]byte{0x5A}, 32)
	caller := &appendCaller{sealPubkey: fresh}
	sess := authedSession(t, caller, nil)
	calls := installSealRecorder(t)

	if _, err := sess.Append("INBOX", newLiteralReader([]byte(sampleRFC5322)), nil); err != nil {
		t.Fatalf("Append: %v", err)
	}
	if caller.gotSealFetchIngest == nil || !*caller.gotSealFetchIngest {
		t.Fatal("APPEND must fetch its seal pubkey with mail_new_ingest=true")
	}
	if len(*calls) != 2 {
		t.Fatalf("seal calls = %d, want 2 (body + hint)", len(*calls))
	}
	for i, c := range *calls {
		if !bytes.Equal(c.pubkey, fresh) {
			t.Fatalf("seal[%d] used pubkey %x, want the per-seal-fetched key %x", i, c.pubkey, fresh)
		}
		if !bytes.Equal(c.mlkemEk, freshMlkemEk) {
			t.Fatalf("seal[%d] did not use the per-seal-fetched ML-KEM ek (got len=%d)", i, len(c.mlkemEk))
		}
	}
}

// TestAppendDegradesToCachedPubkeyWhenSealFetchFails pins the never-bounce
// half: a transient per-seal fetch failure must not bounce the APPEND —
// it seals under the session-cached standing pubkey instead (readable by
// every epoch-aware opener's standing arm).
func TestAppendDegradesToCachedPubkeyWhenSealFetchFails(t *testing.T) {
	caller := &appendCaller{} // no sealPubkey fixture → fetch errors
	sess := authedSession(t, caller, nil)
	calls := installSealRecorder(t)

	if _, err := sess.Append("INBOX", newLiteralReader([]byte(sampleRFC5322)), nil); err != nil {
		t.Fatalf("Append must not bounce on a seal-fetch failure: %v", err)
	}
	if len(*calls) != 2 {
		t.Fatalf("seal calls = %d, want 2 (body + hint)", len(*calls))
	}
	for i, c := range *calls {
		if !bytes.Equal(c.pubkey, fixtureMLSPubkey) {
			t.Fatalf("seal[%d] used pubkey %x, want the cached standing key", i, c.pubkey)
		}
		if !bytes.Equal(c.mlkemEk, fixtureMlkemEk) {
			t.Fatalf("seal[%d] did not use the cached standing ML-KEM ek (got len=%d)", i, len(c.mlkemEk))
		}
	}
}
