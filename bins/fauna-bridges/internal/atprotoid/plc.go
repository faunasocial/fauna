// Package atprotoid builds, signs, and derives did:plc v0.1 operations plus
// the did:web document for the atproto.pds bridge's S2 identity mint path
// (docs/goal/behavior/atproto-pds-bridge.md § Identity; spec facts verified
// against web.plc.directory/spec/v0.1/did-plc).
//
// The core functions are pure (no network): build an operation, dag-cbor
// encode it via indigo's atdata (the same canonical encoding the directory
// validates), sign with a K-256 rotation key, derive the did:plc identifier
// and genesis CID. The directory client (directory.go) and resolvability
// self-check (resolve.go) are the only network touchers, both behind
// injectable HTTP/DNS seams for tests.
package atprotoid

import (
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	"fmt"
	"strings"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/atdata"
	"github.com/ipfs/go-cid"
)

// OpTypeOperation is the `type` every op this package builds carries. The
// legacy v0 `create` shape is deliberately not constructible or updatable here.
const OpTypeOperation = "plc_operation"

// PlcService is one entry of a PLC operation's `services` map — for atproto
// always the `atproto_pds` entry pointing at the hosting PDS.
type PlcService struct {
	Type     string `json:"type"`
	Endpoint string `json:"endpoint"`
}

// PlcOperation is a did:plc v0.1 `plc_operation`. JSON tags carry the exact
// spec key spelling (camelCase) — the directory submit body is this struct as
// plain JSON; the signing input is the dag-cbor encoding of the same map
// WITHOUT `sig` (see UnsignedCBOR).
type PlcOperation struct {
	// Type is always "plc_operation" for the ops this package builds
	// (tombstones are deliberately not constructible here).
	Type string `json:"type"`
	// RotationKeys are did:key strings ordered by DESCENDING authority —
	// index 0 is the most senior. The ratified custody invariant places the
	// USER-custodied key at index 0 and the bridge's junior key after it.
	RotationKeys []string `json:"rotationKeys"`
	// VerificationMethods maps service id → did:key; the "atproto" entry is
	// the repo-commit signing key.
	VerificationMethods map[string]string `json:"verificationMethods"`
	// AlsoKnownAs is the priority-ordered handle list (`at://alice.example.com`).
	AlsoKnownAs []string `json:"alsoKnownAs"`
	// Services holds the `atproto_pds` service entry.
	Services map[string]PlcService `json:"services"`
	// Prev is nil for a genesis op (serialized as an explicit null), else the
	// CID string of the prior operation.
	Prev *string `json:"prev"`
	// Sig is the base64url-no-pad ECDSA-SHA256 signature (low-S, r||s) over
	// the dag-cbor of the op without this field. Empty until Sign.
	Sig string `json:"sig,omitempty"`
}

// BuildGenesisOp assembles an unsigned genesis `plc_operation` with the
// ratified rotation-key seniority: the USER-custodied rotation key at
// rotationKeys[0] (most senior — can always recover from a bridge-key
// compromise within the 72h window) and the bridge-custodied junior key at
// index 1. `prev` is nil (genesis).
func BuildGenesisOp(userRotationDIDKey, bridgeRotationDIDKey, signingDIDKey, handle, pdsEndpoint string) *PlcOperation {
	return &PlcOperation{
		Type:                OpTypeOperation,
		RotationKeys:        []string{userRotationDIDKey, bridgeRotationDIDKey},
		VerificationMethods: map[string]string{"atproto": signingDIDKey},
		AlsoKnownAs:         []string{AtHandleURI(handle)},
		Services: map[string]PlcService{
			"atproto_pds": {Type: "AtprotoPersonalDataServer", Endpoint: pdsEndpoint},
		},
		Prev: nil,
	}
}

// BuildUpdateOpFromPrev assembles an unsigned non-genesis `plc_operation` that
// changes ONLY `alsoKnownAs`, carrying every other field forward verbatim from
// `prev` — the op a handle rename signs and submits.
//
// `prev` must be the op the directory currently has at the head of the log
// (atprotoid.FetchLastOp), NOT a freshly built one: the ratified custody model
// (atproto-pds-bridge.md § State & data shape) lets the user's client rotate
// the signing key or move the PDS through the PLC log without this box, so an
// op rebuilt from nest's roster view would silently revert that. Copying
// forward makes a rename a strict alsoKnownAs delta over whatever the network
// says is true — including changes this bridge never made.
//
// The result is unsigned (`Sig` cleared): the caller signs with a rotation key
// listed in `prev`. The bridge holds the JUNIOR one, so its updates stay
// contestable by the user's senior key inside PLC's 72h window — that is the
// recovery primitive, not a limitation.
func BuildUpdateOpFromPrev(prev *PlcOperation, prevCID, handle string) *PlcOperation {
	op := *prev
	op.RotationKeys = append([]string(nil), prev.RotationKeys...)
	op.VerificationMethods = make(map[string]string, len(prev.VerificationMethods))
	for k, v := range prev.VerificationMethods {
		op.VerificationMethods[k] = v
	}
	op.Services = make(map[string]PlcService, len(prev.Services))
	for k, v := range prev.Services {
		op.Services[k] = v
	}
	op.AlsoKnownAs = []string{AtHandleURI(handle)}
	op.Prev = &prevCID
	op.Sig = ""
	return &op
}

// AtHandleURI renders a handle as the `at://` URI form `alsoKnownAs` carries.
func AtHandleURI(handle string) string { return "at://" + handle }

// PrimaryHandle returns the handle `alsoKnownAs[0]` names (the priority-ordered
// primary), with the `at://` scheme stripped, or "" when the op asserts none.
func PrimaryHandle(op *PlcOperation) string {
	if len(op.AlsoKnownAs) == 0 {
		return ""
	}
	return strings.TrimPrefix(op.AlsoKnownAs[0], "at://")
}

// asData renders the op as the generic map dag-cbor encodes. `prev` is always
// present (an explicit null on genesis, per spec); `sig` is included only for
// the signed encoding.
func (op *PlcOperation) asData(includeSig bool) map[string]any {
	services := make(map[string]any, len(op.Services))
	for k, v := range op.Services {
		services[k] = map[string]any{"type": v.Type, "endpoint": v.Endpoint}
	}
	verification := make(map[string]any, len(op.VerificationMethods))
	for k, v := range op.VerificationMethods {
		verification[k] = v
	}
	rotation := make([]any, len(op.RotationKeys))
	for i, k := range op.RotationKeys {
		rotation[i] = k
	}
	aka := make([]any, len(op.AlsoKnownAs))
	for i, h := range op.AlsoKnownAs {
		aka[i] = h
	}
	var prev any
	if op.Prev != nil {
		prev = *op.Prev
	}
	m := map[string]any{
		"type":                op.Type,
		"rotationKeys":        rotation,
		"verificationMethods": verification,
		"alsoKnownAs":         aka,
		"services":            services,
		"prev":                prev,
	}
	if includeSig {
		m["sig"] = op.Sig
	}
	return m
}

// UnsignedCBOR returns the dag-cbor encoding of the op WITHOUT `sig` — the
// exact byte string the rotation key signs.
func (op *PlcOperation) UnsignedCBOR() ([]byte, error) {
	b, err := atdata.MarshalCBOR(op.asData(false))
	if err != nil {
		return nil, fmt.Errorf("dag-cbor encode unsigned plc op: %w", err)
	}
	return b, nil
}

// SignedCBOR returns the dag-cbor encoding of the signed op (with `sig`) —
// the byte string the did:plc identifier and genesis CID derive from.
func (op *PlcOperation) SignedCBOR() ([]byte, error) {
	if op.Sig == "" {
		return nil, fmt.Errorf("plc op is unsigned (call Sign first)")
	}
	b, err := atdata.MarshalCBOR(op.asData(true))
	if err != nil {
		return nil, fmt.Errorf("dag-cbor encode signed plc op: %w", err)
	}
	return b, nil
}

// Sign computes the op's signature with a rotation private key: ECDSA-SHA256
// over the unsigned dag-cbor, low-S, fixed-size r||s, base64url no padding.
// indigo's atcrypto produces exactly that form (low-S is its documented
// signing behavior for K-256).
func (op *PlcOperation) Sign(rotationKey atcrypto.PrivateKey) error {
	msg, err := op.UnsignedCBOR()
	if err != nil {
		return err
	}
	sig, err := rotationKey.HashAndSign(msg)
	if err != nil {
		return fmt.Errorf("sign plc op: %w", err)
	}
	op.Sig = base64.RawURLEncoding.EncodeToString(sig)
	return nil
}

// VerifySig checks the op's signature against a rotation public key (e.g. the
// bridge rotation pubkey parsed from its did:key string).
func (op *PlcOperation) VerifySig(rotationPub atcrypto.PublicKey) error {
	if op.Sig == "" {
		return fmt.Errorf("plc op is unsigned")
	}
	sig, err := base64.RawURLEncoding.DecodeString(op.Sig)
	if err != nil {
		return fmt.Errorf("decode plc op sig: %w", err)
	}
	msg, err := op.UnsignedCBOR()
	if err != nil {
		return err
	}
	if err := rotationPub.HashAndVerify(msg, sig); err != nil {
		return fmt.Errorf("plc op signature does not verify: %w", err)
	}
	return nil
}

// plcDIDBase32 is the RFC 4648 base32 alphabet, lowercased, no padding — the
// encoding did:plc identifiers use (chars a-z2-7).
var plcDIDBase32 = base32.StdEncoding.WithPadding(base32.NoPadding)

// DerivePlcDid derives the did:plc identifier from a SIGNED genesis op's
// dag-cbor bytes: "did:plc:" + lowercase-base32(sha256(bytes))[:24].
func DerivePlcDid(signedOpCBOR []byte) string {
	h := sha256.Sum256(signedOpCBOR)
	return "did:plc:" + strings.ToLower(plcDIDBase32.EncodeToString(h[:]))[:24]
}

// dagCBORCID builds a CIDv1 with the dag-cbor codec (0x71) and sha2-256
// (0x12) — the content addressing every atproto/PLC block uses (the S0
// probe's builder).
var dagCBORCID = cid.V1Builder{Codec: 0x71, MhType: 0x12, MhLength: 0}

// GenesisCid returns the CID (CIDv1, dag-cbor, sha2-256) of a signed op's
// dag-cbor bytes — the value `prev` chains on and nest records as
// genesis_cid.
func GenesisCid(signedOpCBOR []byte) (string, error) {
	c, err := dagCBORCID.Sum(signedOpCBOR)
	if err != nil {
		return "", fmt.Errorf("derive plc op cid: %w", err)
	}
	return c.String(), nil
}
