// Tests for the fauna.bridges.atproto.* typed wrappers (atproto.go) — the
// recordingCaller pattern from methods_test.go: assert exact method strings,
// wire-shape field names matching libs/fauna-protocol/src/bridge_atproto.rs,
// and reply unpacking (including the Option<String> null cases).
package wsrpc

import (
	"bytes"
	"context"
	"testing"

	"github.com/fxamacker/cbor/v2"
)

func TestAtprotoMethodWrappers(t *testing.T) {
	ctx := context.Background()
	actorID := bytes.Repeat([]byte{0xA7}, 32)

	t.Run("FetchAtprotoIdentities", func(t *testing.T) {
		did := "did:plc:aaaabbbbccccddddeeeeffff"
		signing := "did:key:zSigning"
		caller := &recordingCaller{
			replyBody: map[string]any{
				"identities": []map[string]any{
					{
						"actor_id":                    actorID,
						"handle":                      "alice.example.com",
						"method":                      "plc",
						"status":                      "active",
						"did":                         did,
						"user_rotation_pub_did_key":   "did:key:zUser",
						"signing_pub_did_key":         signing,
						"bridge_rotation_pub_did_key": nil,
						"pds_endpoint":                "https://example.com",
					},
					{
						// A pending did:web row: every Option is null.
						"actor_id":                    actorID,
						"handle":                      "bob.example.com",
						"method":                      "web",
						"status":                      "pending",
						"did":                         nil,
						"user_rotation_pub_did_key":   "",
						"signing_pub_did_key":         nil,
						"bridge_rotation_pub_did_key": nil,
						"pds_endpoint":                "https://example.com",
					},
				},
			},
		}
		got, err := FetchAtprotoIdentities(ctx, caller)
		if err != nil {
			t.Fatalf("FetchAtprotoIdentities: %v", err)
		}
		if caller.gotMethod != MethodAtprotoFetchIdentities {
			t.Errorf("method: got %q, want fauna.bridges.atproto.fetch_identities", caller.gotMethod)
		}
		// Request body must be the empty map (zero-field struct).
		var body map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(body) != 0 {
			t.Errorf("body: got %v, want empty map", body)
		}
		if len(got) != 2 {
			t.Fatalf("identities: got %d, want 2", len(got))
		}
		a := got[0]
		if a.Handle != "alice.example.com" || a.Method != "plc" || a.Status != "active" {
			t.Errorf("row 0 unpack: %+v", a)
		}
		if a.DID == nil || *a.DID != did {
			t.Errorf("row 0 did: got %v, want %q", a.DID, did)
		}
		if a.SigningPubDIDKey == nil || *a.SigningPubDIDKey != signing {
			t.Errorf("row 0 signing_pub_did_key: got %v, want %q", a.SigningPubDIDKey, signing)
		}
		if a.BridgeRotationPubDIDKey != nil {
			t.Errorf("row 0 bridge_rotation_pub_did_key: got %v, want nil", a.BridgeRotationPubDIDKey)
		}
		if !bytes.Equal(a.ActorID, actorID) {
			t.Errorf("row 0 actor_id round-trip broken")
		}
		b := got[1]
		if b.DID != nil || b.SigningPubDIDKey != nil || b.Status != "pending" || b.UserRotationPubDIDKey != "" {
			t.Errorf("row 1 (pending web) unpack: %+v", b)
		}
	})

	t.Run("FetchAtprotoIdentityKeyBlob", func(t *testing.T) {
		blob := []byte{0xDE, 0xAD, 0xBE, 0xEF}
		caller := &recordingCaller{
			replyBody: map[string]any{
				"blob":                        blob,
				"signing_pub_did_key":         "did:key:zSigning",
				"bridge_rotation_pub_did_key": "did:key:zBridgeRot",
			},
		}
		got, err := FetchAtprotoIdentityKeyBlob(ctx, caller, actorID)
		if err != nil {
			t.Fatalf("FetchAtprotoIdentityKeyBlob: %v", err)
		}
		if caller.gotMethod != MethodAtprotoFetchIdentityKeyBlob {
			t.Errorf("method: got %q, want fauna.bridges.atproto.fetch_identity_key_blob", caller.gotMethod)
		}
		// Wire shape: {actor_id: <32-byte CBOR byte string>}.
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytes.Equal(body.ActorID, actorID) {
			t.Errorf("actor_id: got %x", body.ActorID)
		}
		var extra map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &extra); err != nil {
			t.Fatalf("decode body as map: %v", err)
		}
		if len(extra) != 1 {
			t.Errorf("request has %d keys, want exactly {actor_id} (deny_unknown_fields)", len(extra))
		}
		if !bytes.Equal(got.Blob, blob) || got.SigningPubDIDKey != "did:key:zSigning" || got.BridgeRotationPubDIDKey != "did:key:zBridgeRot" {
			t.Errorf("reply unpack: %+v", got)
		}
	})

	t.Run("RecordMintedIdentity_plc", func(t *testing.T) {
		caller := &recordingCaller{replyBody: map[string]any{}}
		cid := "bafyreib2rxk3rw6lbmm2gjfcjnsb3yh5cym3iqqkrcy7m5c"
		if err := RecordMintedIdentity(ctx, caller, actorID, "did:plc:aaaabbbbccccddddeeeeffff", &cid); err != nil {
			t.Fatalf("RecordMintedIdentity: %v", err)
		}
		if caller.gotMethod != MethodAtprotoRecordMintedIdentity {
			t.Errorf("method: got %q, want fauna.bridges.atproto.record_minted_identity", caller.gotMethod)
		}
		var body struct {
			ActorID    []byte  `cbor:"actor_id"`
			DID        string  `cbor:"did"`
			GenesisCID *string `cbor:"genesis_cid"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytes.Equal(body.ActorID, actorID) || body.DID != "did:plc:aaaabbbbccccddddeeeeffff" {
			t.Errorf("body unpack: %+v", body)
		}
		if body.GenesisCID == nil || *body.GenesisCID != cid {
			t.Errorf("genesis_cid: got %v, want %q", body.GenesisCID, cid)
		}
	})

	t.Run("RecordMintedIdentity_web_null_cid", func(t *testing.T) {
		caller := &recordingCaller{replyBody: map[string]any{}}
		if err := RecordMintedIdentity(ctx, caller, actorID, "did:web:bob.example.com", nil); err != nil {
			t.Fatalf("RecordMintedIdentity: %v", err)
		}
		// genesis_cid must ride as an explicit CBOR null (Option::None), so the
		// key is present in the map.
		var raw map[string]cbor.RawMessage
		if err := cbor.Unmarshal(caller.gotBody, &raw); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		rawCID, ok := raw["genesis_cid"]
		if !ok {
			t.Fatal("genesis_cid key absent; want explicit null")
		}
		var v any
		if err := cbor.Unmarshal(rawCID, &v); err != nil {
			t.Fatalf("decode genesis_cid: %v", err)
		}
		if v != nil {
			t.Errorf("genesis_cid = %v, want null", v)
		}
	})
}
