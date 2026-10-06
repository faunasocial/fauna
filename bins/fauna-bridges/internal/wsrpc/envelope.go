// Package wsrpc is the bridge's WS-RPC client: WebSocket dial, bearer-
// token auth via HTTP challenge/verify, CBOR envelope encode/decode, and
// the Call(method, body, &reply) primitive.
//
// Wire format — `envelope.go` defines four frame types mirroring
// libs/fauna-protocol/src/envelope.rs::{Request, Reply, Push, Cancel}.
// The envelope is an integer-keyed CBOR map; key 0 is the type
// discriminant (0=Request, 1=Reply, 2=Push, 3=Cancel) and remaining
// keys depend on the frame type:
//
//	Request: {0: 0, 1: corr u64, 2: kind str, 3: idempkey [16]byte,
//	          4: payload, 5?: replay_forbidden bool, 6?: deadline_ms u32}
//	Reply:   {0: 1, 1: corr u64, 4: payload, 7: ok bool}
//	Push:    {0: 2, 2: kind str, 4: payload, 8: seq u64}
//	Cancel:  {0: 3, 1: corr u64}
//
// All envelope-level keys are integers 0..8 which encode in CBOR as one
// byte each, so the canonical-sort order on the wire is fixed
// regardless of which side encodes (Go's length-first sort and Rust
// ciborium's source order both emit the same byte sequence — there are
// no mixed-length keys to disagree about). This is why envelope
// byte-parity holds even though the inner payload (which uses
// string-keyed maps on the Rust side) does not; the Rust ciborium
// follow-up is tracked separately.
package wsrpc

import (
	"errors"
	"fmt"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// Frame type discriminants. Match the `Type` field's only-valid values on
// each frame struct; the integer-keyed CBOR map at key 0 carries one of
// these.
const (
	TypeRequest uint8 = 0
	TypeReply   uint8 = 1
	TypePush    uint8 = 2
	TypeCancel  uint8 = 3
)

// IdempotencyKey is the 16-byte randomly-chosen per-request key the
// envelope carries at integer key 3. Per spec § 1.1 the bytes are
// opaque to the receiver — the value's only purpose is to enable nest
// to dedupe a replayed request that crosses a reconnect.
type IdempotencyKey [16]byte

// RequestFrame is the outbound (bridge → nest) RPC request envelope.
//
// `Payload` carries the kind-specific CBOR body as a raw byte slice;
// the wsrpc layer never inspects it. Wrappers (Phase B.5) decode it
// into the right Go struct via dagcbor.Unmarshal.
//
// `cbor.RawMessage` is a `[]byte` alias — fxamacker/cbor encodes it
// verbatim on Marshal and stores the input bytes verbatim on Unmarshal.
// dagcbor's reflect-walker float-rejection (internal/dagcbor/codec.go
// line 179) treats []byte as a leaf, so this field passes through the
// walker without descent.
type RequestFrame struct {
	Type            uint8           `cbor:"0,keyasint"`
	CorrelationID   uint64          `cbor:"1,keyasint"`
	Kind            string          `cbor:"2,keyasint"`
	IdempotencyKey  IdempotencyKey  `cbor:"3,keyasint"`
	Payload         cbor.RawMessage `cbor:"4,keyasint"`
	ReplayForbidden bool            `cbor:"5,keyasint,omitempty"`
	DeadlineMs      uint32          `cbor:"6,keyasint,omitempty"`
}

// ReplyFrame is the inbound (nest → bridge) RPC response envelope. `OK`
// is true on success; on false the `Payload` contains the encoded
// RpcError per spec § 1.4 — the wrappers in Phase B.5 surface the typed
// error.
type ReplyFrame struct {
	Type          uint8           `cbor:"0,keyasint"`
	CorrelationID uint64          `cbor:"1,keyasint"`
	Payload       cbor.RawMessage `cbor:"4,keyasint"`
	OK            bool            `cbor:"7,keyasint"`
}

// PushFrame is a server-initiated unsolicited message (no correlation_id;
// classified by Kind). The wsrpc client routes pushes to an optional
// caller-supplied handler so Phase B.5 wrappers can subscribe to e.g.
// config-changed events.
type PushFrame struct {
	Type    uint8           `cbor:"0,keyasint"`
	Kind    string          `cbor:"2,keyasint"`
	Payload cbor.RawMessage `cbor:"4,keyasint"`
	Seq     uint64          `cbor:"8,keyasint"`
}

// CancelFrame is the bridge-side cancel of an in-flight request. wsrpc
// emits one on context-cancelled Call() so nest can stop work.
type CancelFrame struct {
	Type          uint8  `cbor:"0,keyasint"`
	CorrelationID uint64 `cbor:"1,keyasint"`
}

// ErrUnknownFrameType is returned by DecodeFrame when key 0 carries a
// value that is neither 0, 1, 2, nor 3.
var ErrUnknownFrameType = errors.New("wsrpc: unknown frame type discriminant")

// ErrMissingFrameType is returned by DecodeFrame when key 0 is absent
// or its value is not a uint.
var ErrMissingFrameType = errors.New("wsrpc: frame missing type discriminant (key 0)")

// EncodeRequest serializes a RequestFrame as canonical DAG-CBOR.
func EncodeRequest(r *RequestFrame) ([]byte, error) {
	if r.Type != TypeRequest {
		return nil, fmt.Errorf("wsrpc: RequestFrame.Type = %d, want %d", r.Type, TypeRequest)
	}
	return dagcbor.Marshal(r)
}

// EncodeReply serializes a ReplyFrame as canonical DAG-CBOR. Mostly
// used by tests — the bridge process never emits replies.
func EncodeReply(r *ReplyFrame) ([]byte, error) {
	if r.Type != TypeReply {
		return nil, fmt.Errorf("wsrpc: ReplyFrame.Type = %d, want %d", r.Type, TypeReply)
	}
	return dagcbor.Marshal(r)
}

// EncodePush serializes a PushFrame. Tests only — the bridge process
// never emits pushes.
func EncodePush(p *PushFrame) ([]byte, error) {
	if p.Type != TypePush {
		return nil, fmt.Errorf("wsrpc: PushFrame.Type = %d, want %d", p.Type, TypePush)
	}
	return dagcbor.Marshal(p)
}

// EncodeCancel serializes a CancelFrame. The bridge emits one on
// context-cancelled Call.
func EncodeCancel(c *CancelFrame) ([]byte, error) {
	if c.Type != TypeCancel {
		return nil, fmt.Errorf("wsrpc: CancelFrame.Type = %d, want %d", c.Type, TypeCancel)
	}
	return dagcbor.Marshal(c)
}

// DecodeFrame inspects integer key 0 of the wire bytes and decodes
// into the matching frame type. The returned `any` is one of
// *RequestFrame, *ReplyFrame, *PushFrame, or *CancelFrame.
//
// Returns ErrMissingFrameType if key 0 is absent and
// ErrUnknownFrameType if it carries an unknown discriminant.
func DecodeFrame(b []byte) (any, error) {
	// Peek at the type by decoding into a small struct that only
	// captures key 0. Anything else is ignored; if the wire is
	// malformed at the envelope level the typed decode below will
	// surface a richer error.
	var peek struct {
		Type uint8 `cbor:"0,keyasint"`
	}
	if err := cbor.Unmarshal(b, &peek); err != nil {
		// Probe whether key 0 is present at all — a generic decode
		// failure here is most often "key 0 is missing or non-integer."
		// We try a permissive decode to disambiguate.
		var m map[uint64]cbor.RawMessage
		if err2 := cbor.Unmarshal(b, &m); err2 != nil {
			return nil, fmt.Errorf("wsrpc: decode frame: %w", err)
		}
		if _, ok := m[0]; !ok {
			return nil, ErrMissingFrameType
		}
		return nil, fmt.Errorf("wsrpc: decode frame type: %w", err)
	}
	switch peek.Type {
	case TypeRequest:
		req, err := dagcbor.Unmarshal[RequestFrame](b)
		if err != nil {
			return nil, fmt.Errorf("wsrpc: decode RequestFrame: %w", err)
		}
		return &req, nil
	case TypeReply:
		rep, err := dagcbor.Unmarshal[ReplyFrame](b)
		if err != nil {
			return nil, fmt.Errorf("wsrpc: decode ReplyFrame: %w", err)
		}
		return &rep, nil
	case TypePush:
		push, err := dagcbor.Unmarshal[PushFrame](b)
		if err != nil {
			return nil, fmt.Errorf("wsrpc: decode PushFrame: %w", err)
		}
		return &push, nil
	case TypeCancel:
		c, err := dagcbor.Unmarshal[CancelFrame](b)
		if err != nil {
			return nil, fmt.Errorf("wsrpc: decode CancelFrame: %w", err)
		}
		return &c, nil
	default:
		return nil, fmt.Errorf("%w: %d", ErrUnknownFrameType, peek.Type)
	}
}
