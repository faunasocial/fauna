// Envelope tests pin the wire shape of Request/Reply/Push/Cancel frames
// against the Rust libs/fauna-protocol/src/envelope.rs canonical encoder.
//
// Both the integer-keyed envelope AND inner string-keyed payloads are
// byte-for-byte identical between the two encoders since the Rust side
// migrated to fauna-cbor (canonical IPLD-dag-cbor via serde_ipld_dagcbor):
// envelope keys 0..8 are one-byte CBOR uints (canonical-sort fixed), and
// inner payload keys are emitted in length-first-then-bytewise order
// regardless of insertion order. We still decode the inner payload at the
// value level rather than the byte level for forward-compat against
// future field additions, but the fixture itself is canonical.
//
// FIXTURE REGEN: testdata/request-validate-recipient-frame.cbor (the FRAMED
// envelope fixture — distinct from the bare request-body request-<kebab>.cbor
// fixtures consumed by wsrpc_request_cross_language.rs) is regenerated
// by the Rust example
// libs/fauna-protocol/examples/regen_go_wsrpc_fixture.rs. From the
// workspace root:
//
//	cargo run -p fauna-protocol --example regen_go_wsrpc_fixture
//
// then commit the updated fixture.
package wsrpc

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// TestRequestFrameByteForByteParity verifies the Go decode of the
// Rust-generated fixture produces the expected envelope fields, and
// asserts the inner string-keyed payload round-trips at the value
// level (NOT byte level, per the file comment above).
func TestRequestFrameByteForByteParity(t *testing.T) {
	t.Parallel()
	b, err := os.ReadFile(filepath.Join("testdata", "request-validate-recipient-frame.cbor"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	any, err := DecodeFrame(b)
	if err != nil {
		t.Fatalf("DecodeFrame: %v", err)
	}
	req, ok := any.(*RequestFrame)
	if !ok {
		t.Fatalf("DecodeFrame returned %T, want *RequestFrame", any)
	}
	if req.Type != TypeRequest {
		t.Errorf("Type = %d, want %d", req.Type, TypeRequest)
	}
	if req.CorrelationID != 42 {
		t.Errorf("CorrelationID = %d, want 42", req.CorrelationID)
	}
	if req.Kind != MethodValidateRecipient {
		t.Errorf("Kind = %q, want fauna.bridges.validate_recipient", req.Kind)
	}
	for i, v := range req.IdempotencyKey {
		if v != 0 {
			t.Errorf("IdempotencyKey[%d] = %#x, want 0", i, v)
		}
	}
	if req.ReplayForbidden {
		t.Errorf("ReplayForbidden = true, want false (Rust side omits key 5)")
	}
	if req.DeadlineMs != 0 {
		t.Errorf("DeadlineMs = %d, want 0 (Rust side omits key 6)", req.DeadlineMs)
	}

	// Payload is a CBOR map with two string keys. Decode it as
	// map[string]string and assert the values — NOT the byte layout.
	var payload map[string]string
	if err := cbor.Unmarshal(req.Payload, &payload); err != nil {
		t.Fatalf("decode payload: %v", err)
	}
	if got := payload["local_part"]; got != "alice" {
		t.Errorf("payload[local_part] = %q, want alice", got)
	}
	if got := payload["domain"]; got != "example.com" {
		t.Errorf("payload[domain] = %q, want example.com", got)
	}

	// Sanity-check the envelope's first byte: 0xA5 = CBOR major-type 5
	// (map) with 5 entries (the five mandatory Request fields; the
	// fixture omits the two optional ones).
	if b[0] != 0xA5 {
		t.Errorf("first byte = %#x, want 0xA5 (5-entry map)", b[0])
	}
}

// TestRequestRoundTrip exercises the Go encode → Go decode path on a
// fully-populated Request (with both optional fields set).
func TestRequestRoundTrip(t *testing.T) {
	t.Parallel()
	// Encode an inner payload via the same dagcbor path the real Call()
	// uses (client.go marshals the body with dagcbor.Marshal). dagcbor
	// emits map keys in canonical length-first-then-bytewise order, so
	// the embedded payload round-trips through DecodeFrame's strict
	// canonical validation deterministically. (Plain cbor.Marshal here
	// would emit keys in Go map-iteration order — non-canonical ~half the
	// time — and DecodeFrame, which recurses into the cbor.RawMessage
	// Payload, would reject it: a per-process-seed flake.) Use uint64 —
	// dagcbor.Marshal rejects floats so we steer clear of those.
	inner := map[string]uint64{"a": 1, "b": 2}
	payload, err := dagcbor.Marshal(inner)
	if err != nil {
		t.Fatalf("encode payload: %v", err)
	}

	in := &RequestFrame{
		Type:            TypeRequest,
		CorrelationID:   17,
		Kind:            MethodFetchConfig,
		IdempotencyKey:  IdempotencyKey{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16},
		Payload:         payload,
		ReplayForbidden: true,
		DeadlineMs:      5000,
	}
	wire, err := EncodeRequest(in)
	if err != nil {
		t.Fatalf("EncodeRequest: %v", err)
	}
	got, err := DecodeFrame(wire)
	if err != nil {
		t.Fatalf("DecodeFrame: %v", err)
	}
	out, ok := got.(*RequestFrame)
	if !ok {
		t.Fatalf("DecodeFrame returned %T, want *RequestFrame", got)
	}
	if out.CorrelationID != in.CorrelationID {
		t.Errorf("CorrelationID: got %d want %d", out.CorrelationID, in.CorrelationID)
	}
	if out.Kind != in.Kind {
		t.Errorf("Kind: got %q want %q", out.Kind, in.Kind)
	}
	if out.IdempotencyKey != in.IdempotencyKey {
		t.Errorf("IdempotencyKey: got %x want %x", out.IdempotencyKey, in.IdempotencyKey)
	}
	if !out.ReplayForbidden {
		t.Errorf("ReplayForbidden: got false want true")
	}
	if out.DeadlineMs != in.DeadlineMs {
		t.Errorf("DeadlineMs: got %d want %d", out.DeadlineMs, in.DeadlineMs)
	}
	// Payload bytes should round-trip (we're the canonical encoder).
	if !bytes.Equal(out.Payload, in.Payload) {
		t.Errorf("Payload: got %x want %x", out.Payload, in.Payload)
	}
}

// TestReplyAndPushAndCancelRoundTrip exercises the other three frame
// types end-to-end through Encode → DecodeFrame.
func TestReplyAndPushAndCancelRoundTrip(t *testing.T) {
	t.Parallel()

	// Reply (ok=true, payload = a tiny CBOR-encoded struct).
	replyPayload, err := cbor.Marshal(map[string]string{"role": "mta"})
	if err != nil {
		t.Fatalf("encode reply payload: %v", err)
	}
	rep := &ReplyFrame{Type: TypeReply, CorrelationID: 17, Payload: replyPayload, OK: true}
	repWire, err := EncodeReply(rep)
	if err != nil {
		t.Fatalf("EncodeReply: %v", err)
	}
	gotRep, err := DecodeFrame(repWire)
	if err != nil {
		t.Fatalf("DecodeFrame(reply): %v", err)
	}
	rep2, ok := gotRep.(*ReplyFrame)
	if !ok || rep2.CorrelationID != 17 || !rep2.OK {
		t.Errorf("reply decode mismatch: %#v", gotRep)
	}

	// Push.
	pushPayload, _ := cbor.Marshal(map[string]uint64{"seq": 99})
	push := &PushFrame{Type: TypePush, Kind: "fauna.sync.changed", Payload: pushPayload, Seq: 7}
	pushWire, err := EncodePush(push)
	if err != nil {
		t.Fatalf("EncodePush: %v", err)
	}
	gotPush, err := DecodeFrame(pushWire)
	if err != nil {
		t.Fatalf("DecodeFrame(push): %v", err)
	}
	push2, ok := gotPush.(*PushFrame)
	if !ok || push2.Kind != "fauna.sync.changed" || push2.Seq != 7 {
		t.Errorf("push decode mismatch: %#v", gotPush)
	}

	// Cancel.
	c := &CancelFrame{Type: TypeCancel, CorrelationID: 17}
	cWire, err := EncodeCancel(c)
	if err != nil {
		t.Fatalf("EncodeCancel: %v", err)
	}
	gotC, err := DecodeFrame(cWire)
	if err != nil {
		t.Fatalf("DecodeFrame(cancel): %v", err)
	}
	c2, ok := gotC.(*CancelFrame)
	if !ok || c2.CorrelationID != 17 {
		t.Errorf("cancel decode mismatch: %#v", gotC)
	}
}

// TestUnknownFrameType ensures DecodeFrame surfaces a typed error when
// key 0 carries a discriminant outside 0..3.
func TestUnknownFrameType(t *testing.T) {
	t.Parallel()
	// Build a wire frame {0: 99}. 0xA1 = 1-entry map; 0x00 = key 0;
	// 0x18 0x63 = small-int extension byte for 99.
	wire := []byte{0xA1, 0x00, 0x18, 0x63}
	_, err := DecodeFrame(wire)
	if err == nil {
		t.Fatal("DecodeFrame accepted unknown type; want error")
	}
	if !errors.Is(err, ErrUnknownFrameType) {
		t.Errorf("err = %v, want wraps ErrUnknownFrameType", err)
	}
}
