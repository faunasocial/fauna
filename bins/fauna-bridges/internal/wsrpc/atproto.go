// Typed wrappers for the `fauna.bridges.atproto.*` methods the atproto.pds
// bridge calls over WS-RPC (S2 identity surface).
//
// Wire shapes mirror libs/fauna-protocol/src/bridge_atproto.rs field-for-field
// (snake_case CBOR string keys, serde_bytes byte fields as CBOR byte strings,
// `deny_unknown_fields` on the Rust side in both directions) — don't
// paraphrase from memory. Method-name constants live in methods.go's single
// declaration site (MethodAtproto*).
package wsrpc

import (
	"context"
	"fmt"
)

// The values [AtprotoIdentityView.Status] can take, mirroring the
// `atproto_identities.status` CHECK nest enforces. Spelled once here because
// the mint loop, the projection loop and the delete sweep all branch on them
// and a typo in any one of them fails silently as "some other status".
const (
	// IdentityStatusPending — mint owed; the mint loop owns the row.
	IdentityStatusPending = "pending"
	// IdentityStatusActive — DID recorded, repo served and projected.
	IdentityStatusActive = "active"
	// IdentityStatusDeactivated — layer-2 step-down (S4-D): the bridge must not
	// serve the repo or project into it, but the repo, DID and keys are all
	// RETAINED so re-entry restores the same identity.
	IdentityStatusDeactivated = "deactivated"
	// IdentityStatusDeleted — the user confirmed "Delete my Bluesky presence"
	// (S5 slice 5). The bridge sweeps every projected record, announces
	// #account(deleted) and purges the repo. The DID and sealed keys stay
	// nest-side, so this is destruction of the PRESENCE, not of the identity —
	// the terminal identity act is the separate, opt-in PLC tombstone.
	IdentityStatusDeleted = "deleted"
	// IdentityStatusTombstoned — that terminal act happened (S5 slice 5b): the
	// user's client signed a PLC tombstone with the senior rotation key it alone
	// holds, and the directory accepted it, so the DID no longer resolves. The
	// bridge can do NOTHING with such a row and must not try: the sweep that
	// destroyed the presence already ran (it is this status's precondition
	// nest-side), and any new commit would be signed for an identity no relay
	// can resolve, hence verify. It is also unreachable by us in the other
	// direction — the bridge holds only the JUNIOR rotation key, which cannot
	// end an identity.
	IdentityStatusTombstoned = "tombstoned"
)

// AtprotoIdentityView mirrors bridge_atproto.rs:AtprotoIdentityView — one
// user's ATProto identity as the bridge sees it. The handle is derived
// nest-side from the *current* Fauna handle + primary domain on every read,
// never stored.
type AtprotoIdentityView struct {
	// ActorID is the 32-byte owning actor id.
	ActorID []byte `cbor:"actor_id"`
	// Handle is the derived ATProto handle (`alice.example.com`).
	Handle string `cbor:"handle"`
	// Method is the DID method: "plc" or "web".
	Method string `cbor:"method"`
	// Status is one of the IdentityStatus* values above.
	Status string `cbor:"status"`
	// DID is the stored DID once minted/imported (DID-is-data — key
	// everything off this value, never re-derive it). Option<String> on the
	// Rust side, so *string here (nil = not yet minted).
	DID *string `cbor:"did"`
	// UserRotationPubDIDKey is the USER-custodied senior rotation key's
	// `did:key` pubkey (did:plc only; empty for did:web). The secret lives in
	// the user's client credential store and never crosses this wire.
	UserRotationPubDIDKey string `cbor:"user_rotation_pub_did_key"`
	// SigningPubDIDKey is the bridge-custodied signing key pubkey, once
	// provisioned.
	SigningPubDIDKey *string `cbor:"signing_pub_did_key"`
	// BridgeRotationPubDIDKey is the bridge-custodied junior rotation key
	// pubkey, once provisioned.
	BridgeRotationPubDIDKey *string `cbor:"bridge_rotation_pub_did_key"`
	// PDSEndpoint is the endpoint for the genesis op's `services.atproto_pds`
	// (`https://<primary-domain>`).
	PDSEndpoint string `cbor:"pds_endpoint"`
}

// fetchAtprotoIdentitiesRequest mirrors FetchAtprotoIdentitiesRequest (no
// fields; encodes as an empty CBOR map).
type fetchAtprotoIdentitiesRequest struct{}

// fetchAtprotoIdentitiesReply mirrors FetchAtprotoIdentitiesReply.
type fetchAtprotoIdentitiesReply struct {
	Identities []AtprotoIdentityView `cbor:"identities"`
}

// FetchAtprotoIdentities pulls the per-user ATProto identity roster (handles
// derived at read time). The S2 mint loop polls this and mints a DID for each
// row still "pending".
func FetchAtprotoIdentities(ctx context.Context, c Caller) ([]AtprotoIdentityView, error) {
	var reply fetchAtprotoIdentitiesReply
	if err := c.Call(ctx, MethodAtprotoFetchIdentities, fetchAtprotoIdentitiesRequest{}, &reply); err != nil {
		return nil, fmt.Errorf("atproto.fetch_identities: %w", err)
	}
	return reply.Identities, nil
}

// fetchAtprotoIdentityKeyBlobRequest mirrors FetchAtprotoIdentityKeyBlobRequest.
type fetchAtprotoIdentityKeyBlobRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// AtprotoIdentityKeyBlob is the Go-facing view of
// FetchAtprotoIdentityKeyBlobReply: the sealed key blob plus the unsealed
// public halves nest also persists as data.
type AtprotoIdentityKeyBlob struct {
	// Blob is a canonical-CBOR `AtprotoIdentityBlob` sealed to THIS bridge's
	// attested x25519 (opened via mailfauna.UnsealAtprotoIdentityBlob).
	Blob []byte
	// SigningPubDIDKey is the bridge-custodied signing key's `did:key` pubkey.
	SigningPubDIDKey string
	// BridgeRotationPubDIDKey is the bridge-custodied junior rotation key's
	// `did:key` pubkey.
	BridgeRotationPubDIDKey string
}

// fetchAtprotoIdentityKeyBlobReply mirrors FetchAtprotoIdentityKeyBlobReply.
type fetchAtprotoIdentityKeyBlobReply struct {
	Blob                    []byte `cbor:"blob"`
	SigningPubDIDKey        string `cbor:"signing_pub_did_key"`
	BridgeRotationPubDIDKey string `cbor:"bridge_rotation_pub_did_key"`
}

// FetchAtprotoIdentityKeyBlob fetches the sealed bridge-custodied identity
// keys for one actor. Nest provisions the keys on first read
// (provision-on-read), so a "pending" roster row always yields a blob.
func FetchAtprotoIdentityKeyBlob(ctx context.Context, c Caller, actorID []byte) (*AtprotoIdentityKeyBlob, error) {
	req := fetchAtprotoIdentityKeyBlobRequest{ActorID: actorID}
	var reply fetchAtprotoIdentityKeyBlobReply
	if err := c.Call(ctx, MethodAtprotoFetchIdentityKeyBlob, req, &reply); err != nil {
		return nil, fmt.Errorf("atproto.fetch_identity_key_blob: %w", err)
	}
	return &AtprotoIdentityKeyBlob{
		Blob:                    reply.Blob,
		SigningPubDIDKey:        reply.SigningPubDIDKey,
		BridgeRotationPubDIDKey: reply.BridgeRotationPubDIDKey,
	}, nil
}

// fetchAtprotoSessionSecretBlobRequest mirrors
// FetchAtprotoSessionSecretBlobRequest (empty by design: nest keys the
// secret by the CALLER's enrolled role/bridge_id — a bridge can only ever
// fetch its own).
type fetchAtprotoSessionSecretBlobRequest struct{}

// fetchAtprotoSessionSecretBlobReply mirrors FetchAtprotoSessionSecretBlobReply.
type fetchAtprotoSessionSecretBlobReply struct {
	Blob []byte `cbor:"blob"`
}

// FetchAtprotoSessionSecretBlob fetches the bridge-wide sealed HS256
// session-token secret. Nest provisions it on first read
// (provision-on-read) and always returns the same ciphertext
// thereafter — the durability the XRPC token plane rides on across bridge
// restarts. Opened via mailfauna.UnsealAtprotoSessionSecretBlob.
func FetchAtprotoSessionSecretBlob(ctx context.Context, c Caller) ([]byte, error) {
	req := fetchAtprotoSessionSecretBlobRequest{}
	var reply fetchAtprotoSessionSecretBlobReply
	if err := c.Call(ctx, MethodAtprotoFetchSessionSecretBlob, req, &reply); err != nil {
		return nil, fmt.Errorf("atproto.fetch_session_secret_blob: %w", err)
	}
	return reply.Blob, nil
}

// fetchAtprotoIssuerJWKSRequest mirrors FetchAtprotoIssuerJwksRequest (empty:
// what the caller may read is decided by its enrollment, never by a field).
type fetchAtprotoIssuerJWKSRequest struct{}

// issuerJWK mirrors IssuerJwk — one P-256 public key in the JWKS spelling the
// nest's /oauth/jwks already serves. No kty/crv/alg/use: the plane carries one
// key type, and a resource server reading those members would be re-deciding
// something the issuer has decided.
type issuerJWK struct {
	Kid string `cbor:"kid"`
	X   string `cbor:"x"`
	Y   string `cbor:"y"`
}

// fetchAtprotoIssuerJWKSReply mirrors FetchAtprotoIssuerJwksReply.
type fetchAtprotoIssuerJWKSReply struct {
	// nil while the nest has no issuer to name — see IssuerJWKS.Issuer.
	Issuer *string     `cbor:"issuer"`
	Keys   []issuerJWK `cbor:"keys"`
}

// IssuerJWKS is the nest's OAuth issuer key set as this resource server holds
// it: the public keys it may verify a nest-minted access token against, and
// the issuer identifier it must pin when it does.
//
// Issuer is empty while the nest has no issuer to name (a deployment that has
// not claimed a domain) — the nest answers that question, so this side never
// derives it. A resource server holding an empty Issuer verifies no OAuth
// access token at all.
type IssuerJWKS struct {
	Issuer string
	Keys   []IssuerKey
}

// IssuerKey is one public issuer key: its `kid` and the base64url P-256
// coordinates it is rebuilt from.
type IssuerKey struct {
	Kid string
	X   string
	Y   string
}

// FetchAtprotoIssuerJWKS fetches the nest's OAuth issuer key set — the only
// key source this resource server verifies OAuth access tokens against — and
// the issuer to pin, both halves of one teaching (authorization-server.md
// § The issuer).
//
// The reply is the SERVED set and replaces whatever this side held: the nest
// applies the retirement horizon on this read, so a key that has left the set
// must stop verifying here too. Merging sets would keep a force-rotated key
// alive at the one place the forced arm exists to reach.
func FetchAtprotoIssuerJWKS(ctx context.Context, c Caller) (*IssuerJWKS, error) {
	req := fetchAtprotoIssuerJWKSRequest{}
	var reply fetchAtprotoIssuerJWKSReply
	if err := c.Call(ctx, MethodAtprotoFetchIssuerJWKS, req, &reply); err != nil {
		return nil, fmt.Errorf("atproto.fetch_issuer_jwks: %w", err)
	}
	out := &IssuerJWKS{Keys: make([]IssuerKey, 0, len(reply.Keys))}
	if reply.Issuer != nil {
		out.Issuer = *reply.Issuer
	}
	for _, k := range reply.Keys {
		out.Keys = append(out.Keys, IssuerKey{Kid: k.Kid, X: k.X, Y: k.Y})
	}
	return out, nil
}

// recordMintedIdentityRequest mirrors RecordMintedIdentityRequest. GenesisCID
// is Option<String> on the Rust side — nil encodes as CBOR null (did:web has
// no genesis op).
type recordMintedIdentityRequest struct {
	ActorID    []byte  `cbor:"actor_id"`
	DID        string  `cbor:"did"`
	GenesisCID *string `cbor:"genesis_cid"`
}

// recordMintedIdentityReply mirrors RecordMintedIdentityReply (empty).
type recordMintedIdentityReply struct{}

// RecordMintedIdentity reports the DID the bridge minted (did:plc submitted
// to the directory, or the constructed did:web) back to nest, which records
// it as data. Idempotent on the same DID; genesisCID is nil for did:web.
func RecordMintedIdentity(ctx context.Context, c Caller, actorID []byte, did string, genesisCID *string) error {
	req := recordMintedIdentityRequest{ActorID: actorID, DID: did, GenesisCID: genesisCID}
	var reply recordMintedIdentityReply
	if err := c.Call(ctx, MethodAtprotoRecordMintedIdentity, req, &reply); err != nil {
		return fmt.Errorf("atproto.record_minted_identity: %w", err)
	}
	return nil
}

// PublicPostsCursor mirrors bridge_atproto.rs:PublicPostsCursor — the
// (created_at_micros, post_id) resume point the projection stream pages by.
// Treat it as opaque: echo a reply's NextCursor back verbatim as the next
// request's Cursor.
type PublicPostsCursor struct {
	// CreatedAtMicros is content.created_at of the last served row, epoch
	// MICROseconds (the content-table convention — not the epoch-millis of the
	// F1 session surface).
	CreatedAtMicros int64 `cbor:"created_at_micros"`
	// PostID is the lowercase-hex 32-byte content-row id of the last served
	// row (the ordering tie-break).
	PostID string `cbor:"post_id"`
}

// PublicPostsItemKind values mirror bridge_atproto.rs:public_post_item_kind
// (strings on the wire, cross-language forward-compat).
const (
	PublicPostsItemKindPost      = "post"
	PublicPostsItemKindTombstone = "tombstone"
)

// PublicPostItem mirrors bridge_atproto.rs:PublicPostItem — one item of a
// user's public projection stream.
type PublicPostItem struct {
	// PostID is the lowercase-hex 32-byte content-row id.
	PostID string `cbor:"post_id"`
	// CreatedAtMicros is the post's creation instant (post) or the delete
	// instant (tombstone), epoch microseconds — so deletes interleave at the
	// time they happened.
	CreatedAtMicros int64 `cbor:"created_at_micros"`
	// Kind is one of PublicPostsItemKind*.
	Kind string `cbor:"kind"`
	// Payload is the stored bytes verbatim (post bytes for a post; a bare
	// canonical Tombstone for a tombstone — the bridge never decodes it).
	Payload []byte `cbor:"payload"`
	// DeletedPostID is the lowercase-hex digest of the DELETED post for a
	// tombstone item (nest pre-decodes it from the payload so Go never touches
	// dag-cbor — task 4b); nil for a post item. Option<String> on the Rust side.
	DeletedPostID *string `cbor:"deleted_post_id"`
}

// fetchPublicPostsRequest mirrors FetchPublicPostsRequest.
type fetchPublicPostsRequest struct {
	ActorID []byte             `cbor:"actor_id"`
	Cursor  *PublicPostsCursor `cbor:"cursor"`
	Limit   uint32             `cbor:"limit"`
}

// fetchPublicPostsReply mirrors FetchPublicPostsReply.
type fetchPublicPostsReply struct {
	Items      []PublicPostItem   `cbor:"items"`
	NextCursor *PublicPostsCursor `cbor:"next_cursor"`
}

// FetchPublicPosts pulls one page of a user's public projection stream,
// oldest-first from cursor (exclusive), or from the beginning when cursor is
// nil. NextCursor is non-nil exactly when the page filled limit — page again
// from it; nil means the stream is exhausted until the next projection_ready
// nudge or poll.
func FetchPublicPosts(ctx context.Context, c Caller, actorID []byte, cursor *PublicPostsCursor, limit uint32) ([]PublicPostItem, *PublicPostsCursor, error) {
	req := fetchPublicPostsRequest{ActorID: actorID, Cursor: cursor, Limit: limit}
	var reply fetchPublicPostsReply
	if err := c.Call(ctx, MethodAtprotoFetchPublicPosts, req, &reply); err != nil {
		return nil, nil, fmt.Errorf("atproto.fetch_public_posts: %w", err)
	}
	return reply.Items, reply.NextCursor, nil
}

// fetchProfileRequest mirrors FetchProfileRequest.
type fetchProfileRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchProfileReply mirrors FetchProfileReply. Profile is Option<ByteBuf> on
// the Rust side — nil (CBOR null) when the user has never set a profile.
type fetchProfileReply struct {
	Profile []byte `cbor:"profile"`
}

// FetchProfile pulls the target user's current profile bytes for the
// app.bsky.actor.profile record at rkey "self". Returns nil bytes when the
// user has never set a profile (most users project posts before a profile).
func FetchProfile(ctx context.Context, c Caller, actorID []byte) ([]byte, error) {
	req := fetchProfileRequest{ActorID: actorID}
	var reply fetchProfileReply
	if err := c.Call(ctx, MethodAtprotoFetchProfile, req, &reply); err != nil {
		return nil, fmt.Errorf("atproto.fetch_profile: %w", err)
	}
	return reply.Profile, nil
}
