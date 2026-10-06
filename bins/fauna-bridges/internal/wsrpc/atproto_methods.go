// WS-RPC wrappers for the `fauna.bridges.atproto.*` F1 auth-core kinds the
// PDS bridge calls (docs/goal/behavior/atproto-pds-full.md § WS-RPC kind
// surface). Request/reply structs mirror
// libs/fauna-protocol/src/atproto_pds.rs; Option<T> fields are pointers so
// nil encodes as CBOR null (Rust None). Timestamps are epoch milliseconds
// (the nest convention).
package wsrpc

import (
	"context"
	"fmt"
)

const (
	MethodAtprotoFetchAppCredentialVerifiers = "fauna.bridges.atproto.fetch_app_credential_verifiers"
	MethodAtprotoRecordSession               = "fauna.bridges.atproto.record_session"
	MethodAtprotoRefreshSession              = "fauna.bridges.atproto.refresh_session"
	MethodAtprotoEndSession                  = "fauna.bridges.atproto.end_session"
	MethodAtprotoFetchPreferences            = "fauna.bridges.atproto.fetch_preferences"
	MethodAtprotoStorePreferences            = "fauna.bridges.atproto.store_preferences"
	MethodAtprotoIngestExternalWrite         = "fauna.bridges.atproto.ingest_external_write"
	MethodAtprotoRecordBlob                  = "fauna.bridges.atproto.record_blob"
)

// External write actions (mirror atproto_pds.rs::external_write_action). On
// the wire as lowercase strings rather than a CBOR enum so an unrecognized
// value is a handled refusal instead of a decode failure.
const (
	ExternalWriteActionCreate = "create"
	ExternalWriteActionUpdate = "update"
	ExternalWriteActionDelete = "delete"
)

// Refusal sub-types (mirror membership::RefusalSubType's serialized spelling).
// The distinction is load-bearing and travels to the XRPC error NAME: a
// `deferred` refusal must never read to a later session as permanent policy
// (atproto-pds-full.md § Problem 4).
const (
	RefusalPolicy       = "policy"
	RefusalFaunaSurface = "fauna_surface"
	RefusalDeferred     = "deferred"
)

// Batch/record caps (mirror atproto_pds.rs). The bridge pre-checks so an
// over-cap request never reaches the wire; the nest re-checks as the
// source-of-truth authority.
const (
	ExternalWriteBatchMax       = 200
	ExternalWriteRecordMaxBytes = 1024 * 1024
)

// RefreshSessionStatus values (mirror atproto_pds.rs::refresh_status).
const (
	RefreshStatusRotated       = "rotated"
	RefreshStatusReuseDetected = "reuse_detected"
	RefreshStatusInvalid       = "invalid"
	RefreshStatusDisabled      = "disabled"
)

// AppCredentialVerifier is one stored verifier row
// (atproto_pds.rs::AppCredentialVerifierRow).
type AppCredentialVerifier struct {
	CredentialID string `cbor:"credential_id"`
	Verifier     string `cbor:"verifier"`
	DmAllowed    bool   `cbor:"dm_allowed"`
}

// AppCredentialVerifiers is the resolved account view the bridge verifies
// against at createSession (atproto_pds.rs::FetchAppCredentialVerifiersReply).
// ActorID is nil when the identifier resolved to no local account — the
// caller must still fail uniformly toward the XRPC client.
type AppCredentialVerifiers struct {
	ActorID             []byte                  `cbor:"actor_id"`
	ExternalAppsEnabled bool                    `cbor:"external_apps_enabled"`
	Verifiers           []AppCredentialVerifier `cbor:"verifiers"`
	// LoginDID is the DID this account may open a session as — the account's
	// real ATProto DID, and nil when it has no ACTIVE hosted identity with a
	// minted DID (slice 4d). The nest decides that in one place; the bridge
	// never interprets identity status, exactly as it never interprets D8's
	// planes. nil is a uniform auth failure, never a DID the bridge invents.
	LoginDID *string `cbor:"login_did"`
}

type fetchAppCredentialVerifiersRequest struct {
	Identifier string `cbor:"identifier"`
}

// FetchAppCredentialVerifiers resolves a login identifier (handle or DID)
// to the account's verifier rows + external-apps flag. Argon2id
// verification happens caller-side (internal/auth.VerifyArgon2PHC) — the
// nest never sees the presented secret.
func FetchAppCredentialVerifiers(ctx context.Context, c Caller, identifier string) (AppCredentialVerifiers, error) {
	var reply AppCredentialVerifiers
	if err := c.Call(ctx, MethodAtprotoFetchAppCredentialVerifiers,
		fetchAppCredentialVerifiersRequest{Identifier: identifier}, &reply); err != nil {
		return AppCredentialVerifiers{}, fmt.Errorf("atproto.fetch_app_credential_verifiers: %w", err)
	}
	return reply, nil
}

type recordSessionRequest struct {
	ActorID      []byte  `cbor:"actor_id"`
	SessionID    []byte  `cbor:"session_id"`
	Plane        string  `cbor:"plane"`
	CredentialID *string `cbor:"credential_id"`
	ClientNote   *string `cbor:"client_note"`
	ExpiresAt    int64   `cbor:"expires_at"`
}

type recordSessionReply struct {
	OK bool `cbor:"ok"`
}

// RecordSession registers a freshly-minted session family in the nest
// registry (post-verify). sessionID is the initial refresh jti = the
// immutable family id. Refused with code
// `fauna.bridges.atproto.disabled` when the account's kill-switch is OFF.
func RecordSession(ctx context.Context, c Caller, actorID, sessionID []byte, plane, credentialID, clientNote string, expiresAt int64) error {
	req := recordSessionRequest{
		ActorID:   actorID,
		SessionID: sessionID,
		Plane:     plane,
		ExpiresAt: expiresAt,
	}
	if credentialID != "" {
		req.CredentialID = &credentialID
	}
	if clientNote != "" {
		req.ClientNote = &clientNote
	}
	var reply recordSessionReply
	if err := c.Call(ctx, MethodAtprotoRecordSession, req, &reply); err != nil {
		return fmt.Errorf("atproto.record_session: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("atproto.record_session: nest returned ok=false")
	}
	return nil
}

type refreshSessionRequest struct {
	ActorID      []byte `cbor:"actor_id"`
	SessionID    []byte `cbor:"session_id"`
	PresentedJti []byte `cbor:"presented_jti"`
	NewJti       []byte `cbor:"new_jti"`
	NewExpiresAt int64  `cbor:"new_expires_at"`
}

type refreshSessionReply struct {
	Status string `cbor:"status"`
}

// RefreshSession validates + rotates the registry row (rotate-on-use).
// Returns one of the RefreshStatus* values; RefreshStatusReuseDetected
// means the presented jti was superseded and the whole family is now dead.
func RefreshSession(ctx context.Context, c Caller, actorID, sessionID, presentedJti, newJti []byte, newExpiresAt int64) (string, error) {
	req := refreshSessionRequest{
		ActorID:      actorID,
		SessionID:    sessionID,
		PresentedJti: presentedJti,
		NewJti:       newJti,
		NewExpiresAt: newExpiresAt,
	}
	var reply refreshSessionReply
	if err := c.Call(ctx, MethodAtprotoRefreshSession, req, &reply); err != nil {
		return "", fmt.Errorf("atproto.refresh_session: %w", err)
	}
	return reply.Status, nil
}

type endSessionRequest struct {
	ActorID   []byte `cbor:"actor_id"`
	SessionID []byte `cbor:"session_id"`
}

type endSessionReply struct {
	Ended bool `cbor:"ended"`
}

// EndSession revokes the session row (deleteSession). Idempotent: a false
// return means the session was already gone.
func EndSession(ctx context.Context, c Caller, actorID, sessionID []byte) (bool, error) {
	var reply endSessionReply
	if err := c.Call(ctx, MethodAtprotoEndSession,
		endSessionRequest{ActorID: actorID, SessionID: sessionID}, &reply); err != nil {
		return false, fmt.Errorf("atproto.end_session: %w", err)
	}
	return reply.Ended, nil
}

type fetchPreferencesRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

type fetchPreferencesReply struct {
	Preferences []byte `cbor:"preferences"`
}

// FetchPreferences reads the account's opaque preferences payload from nest
// state (app.bsky.actor.getPreferences). A nil return means the account has
// never stored preferences — the caller answers an empty array. The nest
// interprets nothing: these are the exact bytes putPreferences stored (D2).
func FetchPreferences(ctx context.Context, c Caller, actorID []byte) ([]byte, error) {
	var reply fetchPreferencesReply
	if err := c.Call(ctx, MethodAtprotoFetchPreferences,
		fetchPreferencesRequest{ActorID: actorID}, &reply); err != nil {
		return nil, fmt.Errorf("atproto.fetch_preferences: %w", err)
	}
	return reply.Preferences, nil
}

type storePreferencesRequest struct {
	ActorID     []byte `cbor:"actor_id"`
	Preferences []byte `cbor:"preferences"`
}

type storePreferencesReply struct {
	OK bool `cbor:"ok"`
}

type recordBlobRequest struct {
	ActorID  []byte `cbor:"actor_id"`
	CID      string `cbor:"cid"`
	MediaRef []byte `cbor:"media_ref"`
}

type recordBlobReply struct {
	OK       bool   `cbor:"ok"`
	FaunaCID string `cbor:"fauna_cid"`
}

// RecordBlob ties an uploaded blob's ATProto CID (sha256) to the Fauna media
// ContentHash (blake3) its bytes landed under, for actorID — the account
// attribution leg of com.atproto.repo.uploadBlob.
//
// It exists separately from the byte upload because the bulk-binary route
// (POST /api/v1/blob) authenticates as the BRIDGE's service user and keeps its
// uploader field for audit only: it cannot know which account uploaded. Only the
// session-token side holds the authenticated actor.
//
// Returns mediaRef spelled as a canonical Fauna content CID — the NEST's
// spelling, deliberately not re-derived here. It is the provenance key the
// projection loop's "already published these bytes?" lookup matches against
// ContentHash::to_base32, so a Go copy drifting by one character would re-fetch
// and re-hash every image forever with nothing failing.
func RecordBlob(ctx context.Context, c Caller, actorID []byte, cid string, mediaRef []byte) (string, error) {
	var reply recordBlobReply
	if err := c.Call(ctx, MethodAtprotoRecordBlob,
		recordBlobRequest{ActorID: actorID, CID: cid, MediaRef: mediaRef}, &reply); err != nil {
		return "", fmt.Errorf("atproto.record_blob: %w", err)
	}
	if !reply.OK {
		return "", fmt.Errorf("atproto.record_blob: nest returned ok=false")
	}
	if reply.FaunaCID == "" {
		return "", fmt.Errorf("atproto.record_blob: nest answered no fauna cid")
	}
	return reply.FaunaCID, nil
}

// ExternalWrite is one write inside an ingest_external_write batch
// (atproto_pds.rs::ExternalWrite). Optional fields are pointers so nil encodes
// as CBOR null (Rust None).
//
// Rkey is optional ONLY because a round-trip create lets the nest derive the
// projected TID from the Fauna post's creation instant; the caller still sends
// its candidate, and the reply is authoritative for where the record lands.
type ExternalWrite struct {
	Collection string  `cbor:"collection"`
	Action     string  `cbor:"action"`
	Rkey       *string `cbor:"rkey"`
	Record     []byte  `cbor:"record"`
	CID        *string `cbor:"cid"`
	// ResolvedTargets maps AT-URI -> Fauna post id (lowercase-hex 32-byte
	// digest) for the records this write refers to: a reply's parent, a
	// quoted post, or — on a delete — the record's own mapping.
	//
	// The nest cannot compute these: the projected rkey derivation is one-way
	// and post_map is ours. Omitted or unresolved is not an error — the nest
	// journals the write instead of round-tripping it, which is the ratified
	// answer for a target that is not a Fauna post.
	ResolvedTargets map[string]string `cbor:"resolved_targets,omitempty"`
	// ResolvedMedia maps an ATProto blob CID -> the Fauna ContentHash (base32)
	// those bytes are, for the picture refs this write echoes back at us.
	//
	// The nest resolves a FRESH upload itself against the atproto_blobs ledger
	// it owns. An ECHO — the caller handing back the ref our own projection
	// published — it cannot: the Fauna-CID <-> ATProto-CID index is this
	// bridge's blob store, and the two address spaces are only bridged by
	// holding the bytes. So we answer about the store we own and send the
	// mapping. A ref neither resolver knows refuses the whole batch, so this is
	// resolution, never trust (atproto-pds-full.md § F2 detail).
	//
	// Populated for app.bsky.actor.profile writes only: a post's images must be
	// uploaded (F2.4 slice 2's ratified asymmetry), so they have no echo case.
	ResolvedMedia map[string]string `cbor:"resolved_media,omitempty"`
}

// ExternalWriteRefusal is a per-write refusal, sub-typed per D6.
type ExternalWriteRefusal struct {
	SubType string `cbor:"sub_type"`
	Message string `cbor:"message"`
}

// ExternalWriteResult is one write's outcome, positionally aligned with the
// request batch. Exactly one of (Rkey+AtURI) or Refusal is populated.
//
// FaunaPostID is set only when the write became a real Fauna post; it is the
// post_map key, and writing that row is what keeps the projection loop from
// re-projecting this same post over the caller's own record bytes.
// ReprojectRecord, when true, says the repo must carry the PROJECTION's own
// rendering of the state this write changed rather than the caller's bytes —
// today only for the app.bsky.actor.profile singleton, whose collection the
// projection owns and re-derives. Committing the caller's bytes there would have
// the next projection pass overwrite them, so the cid answered synchronously
// would name bytes that do not survive. The nest names the property and this
// side renders it (RepoWriter.RenderProjectedRecord → the same
// atprotorepo.Projector.RenderProfileRecord a projection pass calls), because
// two of the three answers — the picture's ATProto CID/MIME/size from the blob
// store, and whether its bytes are a publishable image — exist only here. See
// the field's owner doc on fauna_protocol::atproto_pds::ExternalWriteResult.
type ExternalWriteResult struct {
	Rkey            *string               `cbor:"rkey"`
	AtURI           *string               `cbor:"at_uri"`
	FaunaPostID     *string               `cbor:"fauna_post_id"`
	ReprojectRecord bool                  `cbor:"reproject_record"`
	Refusal         *ExternalWriteRefusal `cbor:"refusal"`
}

type ingestExternalWriteRequest struct {
	ActorID []byte          `cbor:"actor_id"`
	Writes  []ExternalWrite `cbor:"writes"`
}

type ingestExternalWriteReply struct {
	Results []ExternalWriteResult `cbor:"results"`
}

// IngestExternalWrite hands the nest one XRPC write batch: it classifies each
// write (D2/D6 membership), round-trips what Fauna can express into real Fauna
// mutations via the D10 delegated sub-key, journals what it cannot, and refuses
// with a sub-type otherwise. Results are positionally aligned with writes.
//
// A refused write does NOT fail the call — refusals are per-write DATA, so a
// mixed applyWrites reports exactly which rows the caller must fix. An error
// return means the whole batch failed (a malformed request: the bridge's bug).
func IngestExternalWrite(ctx context.Context, c Caller, actorID []byte, writes []ExternalWrite) ([]ExternalWriteResult, error) {
	var reply ingestExternalWriteReply
	if err := c.Call(ctx, MethodAtprotoIngestExternalWrite,
		ingestExternalWriteRequest{ActorID: actorID, Writes: writes}, &reply); err != nil {
		return nil, fmt.Errorf("atproto.ingest_external_write: %w", err)
	}
	if len(reply.Results) != len(writes) {
		return nil, fmt.Errorf("atproto.ingest_external_write: %d results for %d writes",
			len(reply.Results), len(writes))
	}
	return reply.Results, nil
}

// StorePreferences overwrites the account's opaque preferences payload
// (app.bsky.actor.putPreferences). The nest enforces the hard size cap as the
// source of truth; the caller pre-checks so an oversized body never reaches
// the wire.
func StorePreferences(ctx context.Context, c Caller, actorID, preferences []byte) error {
	var reply storePreferencesReply
	if err := c.Call(ctx, MethodAtprotoStorePreferences,
		storePreferencesRequest{ActorID: actorID, Preferences: preferences}, &reply); err != nil {
		return fmt.Errorf("atproto.store_preferences: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("atproto.store_preferences: nest returned ok=false")
	}
	return nil
}

// ── The permission-set request call — the bridge→nest half ───────────────────

// MethodAtprotoDeliverPermissionSet answers one
// `fauna.bridges.atproto.permission_set_requested` push: the nest's own
// `/oauth/par` asked this bridge to resolve an `include:<NSID>` through the
// chain only this bridge runs (atproto-oauth-provider.md § Implementation
// status today, the 2026-09-25 bullet), and this is the answer, correlated by
// the request id the push carried.
const MethodAtprotoDeliverPermissionSet = "fauna.bridges.atproto.deliver_permission_set"

// deliverPermissionSetRequest mirrors atproto_pds.rs::DeliverPermissionSetRequest.
//
// Record is the verified dag-cbor VERBATIM — the expander's contract — and is
// omitted, not sent empty, when the chain refused: the nest reads absence as
// "could not be resolved" and refuses the PAR at once rather than at its
// deadline. The reason is never carried; it describes this deployment's
// outbound network to whoever chose the NSID.
type deliverPermissionSetRequest struct {
	RequestID []byte `cbor:"request_id"`
	NSID      string `cbor:"nsid"`
	Record    []byte `cbor:"record,omitempty"`
}

type deliverPermissionSetReply struct {
	Accepted bool `cbor:"accepted"`
}

// DeliverAtprotoPermissionSet hands the nest the outcome of one requested
// resolution. `record == nil` delivers a refusal. The returned bool is whether
// a PAR was still waiting — false is a late answer (the nest's deadline
// already refused, or the waiter was shed), which is ordinary and only worth
// a log line.
func DeliverAtprotoPermissionSet(ctx context.Context, c Caller, requestID []byte, nsid string, record []byte) (bool, error) {
	var reply deliverPermissionSetReply
	req := deliverPermissionSetRequest{RequestID: requestID, NSID: nsid, Record: record}
	if err := c.Call(ctx, MethodAtprotoDeliverPermissionSet, req, &reply); err != nil {
		return false, fmt.Errorf("atproto.deliver_permission_set: %w", err)
	}
	return reply.Accepted, nil
}
