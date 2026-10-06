// Package wsrpc — typed wrappers for the MTA-mode `fauna.bridges.*`
// methods the Go bridge calls over WS-RPC.
//
// These are thin: each wrapper builds a Go struct mirroring the
// libs/fauna-protocol Rust serde shape (snake_case CBOR string keys,
// serde_bytes-flagged byte fields encoded as CBOR byte strings),
// dispatches via Caller.Call(ctx, "<method>", body, &reply), and
// unpacks the typed reply.
//
// The wrappers take Caller (interface), not *Client (concrete), so
// methods_test.go can substitute a fake Caller without standing up a
// WS server. *Client satisfies Caller; production code passes the
// dialed client.
//
// Body shapes are read off libs/fauna-protocol/src/bridge_routing.rs
// and libs/fauna-protocol/src/wrapped_blob.rs — don't paraphrase from
// memory. When the Rust side uses `#[serde(with = "serde_bytes")]` on
// a Vec<u8>, the Go side must use `[]byte` (cbor encodes []byte as a
// byte-string, matching Rust's serde_bytes); using `[]uint8` without
// the cbor tag would emit an array-of-uint8 instead and the Rust
// decoder would reject it.
//
// Every wrapper here targets a kind the nest serves today; the wire
// shapes are pinned against the real handlers by the conformance tests.
package wsrpc

import (
	"context"
	"errors"
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/confinement"
	"github.com/fxamacker/cbor/v2"
)

// ── WS-RPC method names ─────────────────────────────────────────
//
// Every nest-side `fauna.bridges.*` method this package can dispatch
// is declared here as an exported string constant. The typed
// wrappers below reference the constants instead of literal strings,
// and test fakes implementing `Caller` switch on them too — one
// declaration site, no per-fake typo risk if a method name changes.
//
// Ordering matches the wrapper declarations below (Whoami first,
// then bridge bootstrap, then bridge ↔ nest routing, then MDA
// IMAP/storage, then outbound queue, then mailbox admin). Group
// boundaries marked with blank lines so a future addition slots in
// alongside its siblings.
const (
	// MethodRequestEnrollment is the pre-identity (anonymous-WS) zero-touch
	// self-enrollment kind — the bridge's first contact with nest, before it
	// has an enrollment row and thus before it can authenticate. Loopback-gated
	// nest-side. See RequestEnrollment / PollEnrollmentUntilApproved.
	MethodRequestEnrollment = "fauna.bridges.request_enrollment"

	MethodWhoami              = "fauna.bridges.whoami"
	MethodRegisterServiceUser = "fauna.bridges.register_service_user"
	MethodFetchTLSCertBlob    = "fauna.bridges.fetch_tls_cert_blob"
	MethodFetchBridgePubkey   = "fauna.bridges.fetch_bridge_pubkey"

	MethodFetchConfig                 = "fauna.bridges.fetch_config"
	MethodReportAuthEvent             = "fauna.bridges.report_auth_event"
	MethodValidateRecipient           = "fauna.bridges.validate_recipient"
	MethodResolveRecipient            = "fauna.bridges.resolve_recipient"
	MethodCheckGreylist               = "fauna.bridges.check_greylist"
	MethodFetchWrappedSubmissionToken = "fauna.bridges.fetch_wrapped_submission_token"
	MethodCheckSubmissionQuota        = "fauna.bridges.check_submission_quota"

	MethodIngestInboundMail       = "fauna.bridges.ingest_inbound_mail"
	MethodSubmitInboundMail       = "fauna.bridges.submit_inbound_mail"
	MethodReportRejectedScan      = "fauna.bridges.report_rejected_scan"
	MethodFetchRecipientMLSPubkey = "fauna.bridges.fetch_recipient_mls_pubkey"
	MethodFetchRecipientIndexKey  = "fauna.bridges.fetch_recipient_index_key"
	MethodFetchWrappedMLSBlob     = "fauna.bridges.fetch_wrapped_mls_blob"
	MethodFetchMLSSnapshotBlob    = "fauna.bridges.fetch_mls_snapshot_blob"
	MethodReportSessionClose      = "fauna.bridges.report_session_close"

	MethodListMailboxes           = "fauna.bridges.list_mailboxes"
	MethodSelectMailbox           = "fauna.bridges.select_mailbox"
	MethodListMessages            = "fauna.bridges.list_messages"
	MethodFetchMessageMetadata    = "fauna.bridges.fetch_message_metadata"
	MethodFetchMessageCiphertext  = "fauna.bridges.fetch_message_ciphertext"
	MethodFetchIndexSegmentsSince = "fauna.bridges.fetch_index_segments_since"
	// The `__index` rail's bridge plane — tantivy segments, NOT the per-message
	// hints `fetch_index_segments_since` above returns. See the section near
	// BridgeIndexList for why the two share a word and nothing else.
	MethodBridgeIndexList = "fauna.bridges.index_list"
	MethodSearchMessages  = "fauna.bridges.search_messages"
	MethodGetQuota        = "fauna.bridges.get_quota"
	MethodStoreFlags      = "fauna.bridges.store_flags"
	MethodCopy            = "fauna.bridges.copy"
	MethodMove            = "fauna.bridges.move"
	MethodExpunge         = "fauna.bridges.expunge"
	MethodAppend          = "fauna.bridges.append"
	MethodFetchSpamModel  = "fauna.bridges.fetch_spam_model"
	MethodPutSpamModel    = "fauna.bridges.put_spam_model"

	MethodFetchOutboundDue      = "fauna.bridges.fetch_outbound_due"
	MethodMarkOutboundDelivered = "fauna.bridges.mark_outbound_delivered"
	MethodMarkOutboundFailed    = "fauna.bridges.mark_outbound_failed"
	MethodMarkOutboundBounced   = "fauna.bridges.mark_outbound_bounced"
	MethodEnqueueOutboundMail   = "fauna.bridges.enqueue_outbound_mail"
	MethodFetchMtaStsPolicy     = "fauna.bridges.fetch_mta_sts_policy"
	MethodFetchTlsa             = "fauna.bridges.fetch_tlsa"
	MethodResolveMx             = "fauna.bridges.resolve_mx"
	MethodReportTlsAttempt      = "fauna.bridges.report_tls_attempt"

	MethodFetchRecipientForwardConfig = "fauna.bridges.fetch_recipient_forward_config"
	MethodFetchRecipientFilters       = "fauna.bridges.fetch_recipient_filters"
	MethodForwardMessage              = "fauna.bridges.forward_message"
	MethodSendAutoReply               = "fauna.bridges.send_auto_reply"
	MethodDecodeSrsBounce             = "fauna.bridges.decode_srs_bounce"

	MethodCreateMailbox = "fauna.bridges.create_mailbox"
	MethodDeleteMailbox = "fauna.bridges.delete_mailbox"
	MethodRenameMailbox = "fauna.bridges.rename_mailbox"

	// ATProto PDS bridge (`fauna.bridges.atproto.*` — the dotted sub-namespace,
	// the `fauna.bridges.feeds.*` precedent). Allowlisted nest-side for
	// CallerClass::BridgeAtprotoPds only. Typed wrappers live in atproto.go;
	// wire shapes mirror libs/fauna-protocol/src/bridge_atproto.rs.
	MethodAtprotoFetchIdentities        = "fauna.bridges.atproto.fetch_identities"
	MethodAtprotoFetchIdentityKeyBlob   = "fauna.bridges.atproto.fetch_identity_key_blob"
	MethodAtprotoFetchSessionSecretBlob = "fauna.bridges.atproto.fetch_session_secret_blob"
	MethodAtprotoFetchIssuerJWKS        = "fauna.bridges.atproto.fetch_issuer_jwks"
	MethodAtprotoRecordMintedIdentity   = "fauna.bridges.atproto.record_minted_identity"
	MethodAtprotoFetchPublicPosts       = "fauna.bridges.atproto.fetch_public_posts"
	MethodAtprotoFetchProfile           = "fauna.bridges.atproto.fetch_profile"

	MethodProvisionCalendar  = "fauna.bridges.provision_calendar"
	MethodPlaceInboundInvite = "fauna.bridges.place_inbound_invite"
	MethodListCalendars      = "fauna.bridges.list_calendars"
	MethodPutEventCiphertext = "fauna.bridges.put_event_ciphertext"
	MethodDeleteEvent        = "fauna.bridges.delete_event"
	MethodQueryEvents        = "fauna.bridges.query_events"
	MethodSyncCalendarSince  = "fauna.bridges.sync_calendar_since"

	// CardDAV (`bridge_carddav_*`) — the contacts twin of the CalDAV block
	// above: calendar→addressbook, event→card. Allowlisted server-side for
	// BridgeMda only (see bins/fauna-nest/src/bridge_method_allowlist.rs), same
	// as their CalDAV siblings.
	MethodProvisionAddressbook = "fauna.bridges.provision_addressbook"
	MethodListAddressbooks     = "fauna.bridges.list_addressbooks"
	MethodPutCardCiphertext    = "fauna.bridges.put_card_ciphertext"
	MethodDeleteCard           = "fauna.bridges.delete_card"
	MethodQueryCards           = "fauna.bridges.query_cards"
	MethodSyncAddressbookSince = "fauna.bridges.sync_addressbook_since"
	MethodDeleteAddressbook    = "fauna.bridges.delete_addressbook"

	// MDA server-side auto-schedule — the mailbox-less attendee rail
	// (caldav-server.md § Server-side auto-schedule, C4). The gateway classifies
	// each attendee, and for a mailbox-less Fauna recipient seals the iMIP itself
	// and ships it over the WS-RPC scheduling rail instead of email.
	// `deliver_sealed_scheduling` is the BridgeMda-only delivery RPC;
	// `actor.by_handle` maps a local-domain attendee to its actor_id (a public
	// discovery read); `keypackage.fetch` (widened to BridgeMda in C3) fetches
	// the recipient KP the one-off welcome seals against.
	MethodDeliverSealedScheduling = "fauna.bridges.deliver_sealed_scheduling"
	MethodActorByHandle           = "fauna.actor.by_handle"
	MethodKeypackageFetch         = "fauna.conversations.keypackage.fetch"

	// Capability grants (fauna.capabilities.*) — the user-minted, scope-limited,
	// revocable content-processing capabilities (design § Phase 2 Step 2 § 2.3).
	// A holder (any approved service-user) calls `fetch` to pull the grants
	// sealed to its own enrolled x25519; mint/renew/revoke are owner-side (client
	// RPCs), not bridge-issued, so only `fetch` has a bridge wrapper here.
	MethodFetchCapabilityGrants = "fauna.capabilities.fetch"

	// The re-score drain plane (design § 2.5 step 4): a holder asks the nest
	// which of its granted owners' content is behind on which factor
	// (`rescore_worklist`, content-free metadata), unseals + re-runs the
	// co-resident scorer off the nest, then writes the re-computed rows back
	// (`submit_scores`, authz'd per row against its `content.label-write`
	// grant, fail-closed per batch).
	MethodRescoreWorklist = "fauna.capabilities.rescore_worklist"
	MethodSubmitScores    = "fauna.capabilities.submit_scores"

	// The spam-baseline publish drain (mail-spam.md § Encrypted-mode
	// interaction, ratified 2026-07-13) — the third holder-pull drain
	// instance beside the re-score plane. A `publish_spam_baseline` push
	// (carrying a run_id) prompts the holder to pull the run's grant-gated
	// sealed-copy worklist (`spam_baseline_worklist`), unseal-merge it OFF-BOX
	// with its OWN service-user key (never any key of a user's), and submit its
	// merged half + contributor count back (`submit_spam_baseline`).
	MethodSpamBaselineWorklist = "fauna.capabilities.spam_baseline_worklist"
	MethodSubmitSpamBaseline   = "fauna.capabilities.submit_spam_baseline"

	// MethodInspectLabeler fetches one community-labeler's full record — the
	// signed metadata blob + the WASM module bytes. The holder calls it while
	// draining a `labeler:<id>` re-score obligation to get the exact module to
	// run `label()` over (Slice 3b). The nest gates it to `User | BridgeMda |
	// ContentProcessor` (the artifact is public/transparent — inspect-before-
	// subscribe — so no owner-scoped secret crosses; the holder re-verifies
	// sig+hash before instantiation, security review B1).
	MethodInspectLabeler = "fauna.labelers.inspect"

	// WebDAV (`bridge webdav_*`) — the files terminator's data plane
	// (webdav-server.md § MDA↔nest WS-RPC contract). All BridgeMda-only
	// (allowlisted server-side in bins/fauna-nest/src/bridge_method_allowlist.rs).
	// `fetch_webdav_keys_blob` rides the AUTH path (the MSEK-sealed served-set
	// content keys); `mint_bulk_byte_token` scopes the chunk/manifest byte routes;
	// the three `webdav_*` kinds enumerate served sets, list a set's files, and
	// record a WebDAV write as an ordinary folder change. WebDAV rides the
	// existing CalDAV listener/port; ConfigSnapshot.WebDAVEnabled gates the mount.
	MethodFetchWebdavKeysBlob = "fauna.bridges.fetch_webdav_keys_blob"
	MethodMintBulkByteToken   = "fauna.bridges.mint_bulk_byte_token"
	MethodWebdavListFolders   = "fauna.bridges.webdav_list_folders"
	MethodWebdavListFiles     = "fauna.bridges.webdav_list_files"
	MethodWebdavQuota         = "fauna.bridges.webdav_quota"
	MethodWebdavRecordChange  = "fauna.bridges.webdav_record_change"
	// MethodWebdavAdmitPrincipal is the bearer door's admission relay
	// (webdav-server.md § Key model → *A principal's read*, (6)): the MDA hands
	// the nest a principal's DPoP token, proofs, method and URL; the nest runs
	// its one principal admission and answers the scopes, `exp` and each
	// `folder:read` folder's grant liveness and served state — or the refusal's
	// challenge, which the MDA relays verbatim.
	MethodWebdavAdmitPrincipal = "fauna.bridges.webdav_admit_principal"

	// MethodReportLogEvents is the sidecar log plane's enrolled-bridge leg: a
	// batch of allowlisted, admin-meaningful events from this bridge into
	// nest's remote log ring, rendered on the `admin-logs` page
	// (`observability.md` § The sidecar log plane). Gated nest-side to
	// `BridgeMta | BridgeMda | ContentProcessor | BridgeAtprotoPds` and denied
	// to every client class — a source may never speak as another source or as
	// the nest, which is also why the batch carries no source identity: nest
	// derives it from the authenticated identity. See internal/logplane for the
	// catalogue and the queue/flush machinery that feeds this wrapper.
	MethodReportLogEvents = "fauna.bridges.report_log_events"
)

// ── request_enrollment — zero-touch self-enrollment (pre-identity) ──

// requestEnrollmentRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:RequestEnrollmentRequest.
// `ed25519_pubkey` is serde_bytes on the Rust side → []byte here (CBOR
// byte-string). `role_hint` must be "mta"/"mda" (nest rejects anything
// else with fauna.protocol.malformed); `bridge_id` may be empty or the
// first-boot "unresolved-bridge" placeholder — nest then synthesizes a
// readable id from the role + pubkey prefix.
type requestEnrollmentRequest struct {
	Ed25519Pubkey []byte `cbor:"ed25519_pubkey"`
	RoleHint      string `cbor:"role_hint"`
	BridgeID      string `cbor:"bridge_id"`
	// X25519Pubkey + EnrollmentSig are the slice-2 proof-of-possession fields. Both `omitempty` mirror the Rust
	// side's `skip_serializing_if = Option::is_none`: ABSENT on a dev/binary-only
	// bridge with no artifact keypair (nest then takes the lenient
	// registry-absent path), PRESENT on the image path where the deployment
	// artifact minted a blessed keypair. EnrollmentSig is the Ed25519 signature
	// over EnrollmentSignedMessage(role_hint, ed25519_pubkey, x25519_pubkey),
	// proving possession of the artifact-blessed key nest has on record for the
	// role. security.md § Enrollment proof-of-possession contract.
	X25519Pubkey  []byte `cbor:"x25519_pubkey,omitempty"`
	EnrollmentSig []byte `cbor:"enrollment_sig,omitempty"`
}

// requestEnrollmentReply mirrors RequestEnrollmentReply — the current
// enrollment status (StatusPending / StatusApproved / StatusRevoked).
type requestEnrollmentReply struct {
	Status string `cbor:"status"`
}

// RequestEnrollment announces the bridge's Ed25519 pubkey to nest over the
// anonymous (pre-identity) WS so it surfaces in the admin's pending-approval
// list, and returns the row's current status. Idempotent: re-calling returns
// the existing row's status without re-creating it, so it doubles as the
// pre-approval poll surface (PollEnrollmentUntilApproved). This is the
// bridge's first contact at cold boot — before it has an enrollment row it
// cannot authenticate, so it cannot use the authed Whoami; enrollment runs on
// the token-less anonymous connection from DialAnonymous.
//
// mail-bridge-lifecycle.md § Cold boot steps 3–4 / § Pending approval.
//
// id.X25519Pub / id.EnrollmentSig are the slice-2 proof-of-possession fields:
// non-nil on the image path (artifact-minted blessed keypair) and emitted as
// the wire `x25519_pubkey` / `enrollment_sig`; nil on a dev/binary-only bridge, where
// `omitempty` drops them and nest takes the lenient (registry-absent) path.
func RequestEnrollment(ctx context.Context, c Caller, id EnrollmentIdentity) (status string, err error) {
	req := requestEnrollmentRequest{
		Ed25519Pubkey: id.Ed25519Pub,
		RoleHint:      id.RoleHint,
		BridgeID:      id.BridgeID,
		X25519Pubkey:  id.X25519Pub,
		EnrollmentSig: id.EnrollmentSig,
	}
	var reply requestEnrollmentReply
	if err := c.Call(ctx, MethodRequestEnrollment, req, &reply); err != nil {
		return "", fmt.Errorf("request_enrollment: %w", err)
	}
	return reply.Status, nil
}

// ── whoami — role discovery ──────────────────────────────────────

// WhoamiReply mirrors libs/fauna-protocol/src/bridge_routing.rs
// WhoamiReply. Field tags match the Rust serde defaults (snake_case).
//
// Per-bridge domain is NOT on this reply — see
// docs/goal/behavior/mail-bridge-lifecycle.md § Wire shapes. The
// bridge learns its accept-RCPT-for-these-domains list from
// fauna.bridges.fetch_config's `local_domains` projection of the
// `mail_domains` table; the single-domain anchor for TLS / EHLO lives
// in the same reply's `primary_domain` field.
type WhoamiReply struct {
	Role             string `cbor:"role"`
	BridgeID         string `cbor:"bridge_id"`
	Status           string `cbor:"status"`
	Ed25519PubkeyHex string `cbor:"ed25519_pubkey_hex"`
	X25519PubkeyHex  string `cbor:"x25519_pubkey_hex"`
	// NodeMode is the nest's NAT axis ("public" / "private"), sourced
	// from the nest's deployment-topology `[nest] mode`. The MDA reads it
	// at cold boot to pick its IMAP/CalDAV listener bind default: a
	// "private" nest defaults the bind to loopback rather than
	// all-interfaces, so a private-paired plaintext deployment never
	// silently exposes IMAP/CalDAV on a public interface
	// (deployment-home-with-public-relay.md § Plaintext-mode behavior).
	// The nest always sends it; Go treats only "private" specially (main.go
	// resolveMDAListenAddrs), so anything else keeps the all-interfaces
	// behavior public deployments rely on.
	NodeMode string `cbor:"node_mode"`
}

// whoamiRequest is the empty request body. Encodes to a CBOR map of
// size 0 ({}), matching the Rust `WhoamiRequest {}` unit-struct form.
type whoamiRequest struct{}

// Whoami resolves the calling bridge's role and identity from nest.
// REPLACES the retired --mode flag — B.8 calls this after Dial to
// decide whether to run mta.Run or mda.Run.
func Whoami(ctx context.Context, c Caller) (WhoamiReply, error) {
	var reply WhoamiReply
	if err := c.Call(ctx, MethodWhoami, whoamiRequest{}, &reply); err != nil {
		return WhoamiReply{}, fmt.Errorf("whoami: %w", err)
	}
	return reply, nil
}

// ── register_service_user ────────────────────────────────────────

// registerServiceUserRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:RegisterServiceUserRequest.
//
// MlkemEk (PQ-CAP-2) is the bridge's published 1184-byte ML-KEM-768 encapsulation
// key, derived from the Ed25519 keyfile seed. Optional by role, not by version:
// the mail bridge always sends one, the atproto bridge — a cgo-free binary that
// holds no hybrid-sealed blob — never does, and `omitempty` leaves the field off
// its request (matching the Rust mirror's `skip_serializing_if`).
//
// Confinement is the bridge's startup self-probe of its own sandbox
// (security.md § Co-resident process trust boundary → Confinement self-probe) —
// provisioning diagnostics, never an attestation. `omitempty` keeps a
// non-probing bridge's request byte-identical to the pre-probe wire, same
// discipline as MlkemEk.
type registerServiceUserRequest struct {
	Ed25519Pubkey []byte              `cbor:"ed25519_pubkey"`
	X25519Pubkey  []byte              `cbor:"x25519_pubkey"`
	MlkemEk       *[]byte             `cbor:"mlkem_ek,omitempty"`
	Role          string              `cbor:"role"`
	BridgeID      string              `cbor:"bridge_id"`
	Confinement   *confinement.Report `cbor:"confinement,omitempty"`
}

// registerServiceUserReply mirrors RegisterServiceUserReply.
type registerServiceUserReply struct {
	EnrollmentRequestID string `cbor:"enrollment_request_id"`
}

// RegisterServiceUser registers the bridge's keys on first connect.
// Returns the diagnostic enrollment_request_id (see the spec note —
// "enrollment-<hex(actor_id)>-<status>", not a resubmittable token).
func RegisterServiceUser(ctx context.Context, c Caller, ed25519Pub, x25519Pub, mlkemEk []byte, role, bridgeID string, conf *confinement.Report) (enrollmentRequestID string, err error) {
	req := registerServiceUserRequest{
		Ed25519Pubkey: ed25519Pub,
		X25519Pubkey:  x25519Pub,
		Role:          role,
		BridgeID:      bridgeID,
		Confinement:   conf,
	}
	// PQ-CAP-2: publish the ML-KEM ek only when the bridge derived one (a
	// classical-only bridge passes nil → the field is omitted).
	if len(mlkemEk) > 0 {
		ek := mlkemEk
		req.MlkemEk = &ek
	}
	var reply registerServiceUserReply
	if err := c.Call(ctx, MethodRegisterServiceUser, req, &reply); err != nil {
		return "", fmt.Errorf("register_service_user: %w", err)
	}
	return reply.EnrollmentRequestID, nil
}

// ── fetch_tls_cert_blob ──────────────────────────────────────────

// fetchTLSCertBlobRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:FetchTlsCertBlobRequest.
type fetchTLSCertBlobRequest struct {
	BridgeRole string `cbor:"bridge_role"`
	BridgeID   string `cbor:"bridge_id"`
	Domain     string `cbor:"domain"`
}

// fetchTLSCertBlobReply mirrors FetchTlsCertBlobReply — the blob
// field is Option<ByteBuf> on the Rust side; on the Go side we
// declare it as a pointer-to-byte-slice. Nil means "no blob"; a
// non-nil empty slice means "empty blob" (caller-visible distinction
// matches the Rust None/Some(empty) distinction).
type fetchTLSCertBlobReply struct {
	Blob *[]byte `cbor:"blob"`
}

// FetchTLSCertBlob fetches the wrapped TLS cert blob for a domain.
// Returns the raw wrapped bytes (nil if nest has no blob on file);
// unwrap happens in B.6 against the bridge's X25519 private key.
func FetchTLSCertBlob(ctx context.Context, c Caller, role, bridgeID, domain string) ([]byte, error) {
	req := fetchTLSCertBlobRequest{
		BridgeRole: role,
		BridgeID:   bridgeID,
		Domain:     domain,
	}
	var reply fetchTLSCertBlobReply
	if err := c.Call(ctx, MethodFetchTLSCertBlob, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_tls_cert_blob: %w", err)
	}
	if reply.Blob == nil {
		return nil, nil
	}
	return *reply.Blob, nil
}

// ── fetch_bridge_pubkey ──────────────────────────────────────────

// fetchBridgePubkeyRequest mirrors FetchBridgePubkeyRequest.
type fetchBridgePubkeyRequest struct {
	BridgeRole string `cbor:"bridge_role"`
	BridgeID   string `cbor:"bridge_id"`
}

// fetchBridgePubkeyReply mirrors FetchBridgePubkeyReply.
type fetchBridgePubkeyReply struct {
	Ed25519Pubkey []byte `cbor:"ed25519_pubkey"`
	X25519Pubkey  []byte `cbor:"x25519_pubkey"`
}

// FetchBridgePubkey returns another bridge's Ed25519 + X25519 pubkey
// pair. Used when one bridge needs to wrap a blob targeted at
// another (e.g. MTA wrapping a submission token for the MDA).
func FetchBridgePubkey(ctx context.Context, c Caller, role, bridgeID string) (ed25519Pub, x25519Pub []byte, err error) {
	req := fetchBridgePubkeyRequest{BridgeRole: role, BridgeID: bridgeID}
	var reply fetchBridgePubkeyReply
	if err := c.Call(ctx, MethodFetchBridgePubkey, req, &reply); err != nil {
		return nil, nil, fmt.Errorf("fetch_bridge_pubkey: %w", err)
	}
	return reply.Ed25519Pubkey, reply.X25519Pubkey, nil
}

// ── fetch_config ─────────────────────────────────────────────────

// SpamPolicyThresholds mirrors
// libs/fauna-protocol/src/bridge_routing.rs:SpamPolicyThresholds.
//
// The inbound-hardening fields (MaxConnPerMin, FCrDNSMode,
// HELOIdentityRequired, RejectFCrDNSFail, MaxMessageBytes) feed C.2's
// connection-time policy gate (`internal/mta/policy.go`) and C.4's
// `Session.Data` pre-parser size guard. Catalog rows in
// `docs/goal/behavior/mail-policy-config.md` § Inbound hardening.
type SpamPolicyThresholds struct {
	// MaxScoreBeforeSpamFolder / MaxScoreBeforeReject are the two
	// combined-score tier thresholds (0–15 points) the shared Rust scorer
	// in libs/fauna-mail/src/spam/mod.rs consumes via fauna_mail::SpamPolicy.
	// **`0 = disabled`** for a tier. The permissive default is 5/0
	// (auto-Junk on; reject off — admin opt-in); when both are non-zero,
	// spam_folder < reject must hold (scorer debug_asserts, nest validates
	// at write time). The bridge maps these onto SpamPolicy at
	// Session.Data time and drives the combined-score disposition.
	MaxScoreBeforeSpamFolder uint32   `cbor:"max_score_before_spam_folder"`
	MaxScoreBeforeReject     uint32   `cbor:"max_score_before_reject"`
	DNSBLServers             []string `cbor:"dnsbl_servers"`
	RejectNoRdns             bool     `cbor:"reject_no_rdns"`
	GreylistEnabled          bool     `cbor:"greylist_enabled"`
	GreylistDelaySecs        uint32   `cbor:"greylist_delay_secs"`
	MaxConnPerMin            uint32   `cbor:"max_conn_per_min"`
	// FCrDNSMode carries one of the FCrDNSModeWire constants below (declared
	// near the other wire vocabularies); ../mta.ParseFCrDNSMode reads it.
	FCrDNSMode           string `cbor:"fcrdns_mode"`
	HELOIdentityRequired bool   `cbor:"helo_identity_required"`
	RejectFCrDNSFail     bool   `cbor:"reject_fcrdns_fail"`
	MaxMessageBytes      uint32 `cbor:"max_message_bytes"`
	// BayesianWeightMilli / BayesianMinSamples / BayesianFullConfidenceSamples
	// are the Tier-2 per-user combined-score-formula knobs (mail-spam.md
	// § Combined-score formula). The MDA's SELECT-time scoring pass
	// (spam_score.go) feeds them to the shared scorer
	// (`mailfauna.BayesianKnobsFromSnapshot` → `WeightedBayesianMilliForModel`)
	// — the per-user term the MTA perimeter hard-codes to 0. Catalog rows in
	// `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training).
	BayesianWeightMilli           uint32 `cbor:"bayesian_weight_milli"`
	BayesianMinSamples            uint32 `cbor:"bayesian_min_samples"`
	BayesianFullConfidenceSamples uint32 `cbor:"bayesian_full_confidence_samples"`
	// TrainingHistoryRetentionDays is nest-consumed only (nest's daily
	// `spam_training_history` GC) — the bridge never reads it, but the field
	// is mirrored so the Rust→Go reply round-trip stays byte-exact.
	TrainingHistoryRetentionDays uint32 `cbor:"training_history_retention_days"`
	// UnlistedRecipientPenalty (points, default 0 = off) is added to a
	// catch-all recipient's combined score at the MTA per-recipient delivery
	// loop — the recipient-whitelist "everything unlisted → spam" term
	// (`mail-spam.md` § Unlisted-recipient penalty). Unlike the bayesian
	// knobs, the MTA DOES read this one.
	UnlistedRecipientPenalty uint32 `cbor:"unlisted_recipient_penalty"`
	// BaselineStandingPublish (default false) is nest-consumed only — the
	// standing publish of the deployment spam baseline (`mail-spam.md` § Cold
	// start Path 2 → *Standing publish*). The bridge never reads it; mirrored
	// so the Rust→Go reply round-trip stays byte-exact.
	BaselineStandingPublish bool `cbor:"baseline_standing_publish"`
}

// AuthPolicy mirrors bridge_routing.rs:AuthPolicy.
type AuthPolicy struct {
	EnforceDmarc           bool `cbor:"enforce_dmarc"`
	EnforceDmarcQuarantine bool `cbor:"enforce_dmarc_quarantine"`
	EnforceSpfHardfail     bool `cbor:"enforce_spf_hardfail"`
	// EnforceDkim: when true, DKIM-fail messages are rejected at DATA with
	// 550 5.7.20 unless DMARC has already decided (Pass / Fail{Quarantine|
	// Reject}). Off by default — many legitimate hobbyist senders ship
	// unsigned mail. See docs/goal/behavior/smtp-server.md § DKIM verdict
	// pipeline and internal/mta/dkim_gate.go.
	EnforceDkim bool `cbor:"enforce_dkim"`
	LogOnly     bool `cbor:"log_only"`
	// MaxAuthFailuresPerMinute is the D.7 per-(credential_id, source_IP)
	// submission AUTH-failure ceiling per 1-minute fixed window. The
	// 31st (default) attempt against the same (credential, IP) within
	// the window returns SMTP 421 4.7.0 before fetch_wrapped_submission_token
	// runs — denying the attacker an AEAD-timing oracle. The bridge
	// reads this at startup (and on snapshot push); zero disables the
	// gate (test fixtures opt out).
	MaxAuthFailuresPerMinute uint32 `cbor:"max_auth_failures_per_minute"`
	// MaxConnPerIP is the per-source-IP CONCURRENT-connection ceiling on the
	// authenticated submission (465/587), IMAP (993/143) and CalDAV (443)
	// listeners (catalog mail.auth.per_ip_max_concurrent_conn). Once a single
	// source IP holds this many simultaneous connections it is shed (closed at
	// accept) until one frees; loopback is exempt; 0 disables the gate. The
	// bridge wraps each authenticated listener in internal/connlimit's per-IP
	// limiter (the Go analogue of the Rust fauna_conn_limit::PerIpConnLimit the
	// nest TLS loop + SNI router use), keyed on the PROXY-v2-restored real
	// client IP and hot-reloaded on config_changed. Orthogonal to
	// MaxAuthFailuresPerMinute (which caps failed-AUTH *rate*; this caps
	// connection *simultaneity*). See docs/goal/behavior/smtp-server.md
	// § Connection-time limits + mail-policy-config.md § Submission policy.
	MaxConnPerIP uint32 `cbor:"max_conn_per_ip"`
}

// SubmissionPolicyThresholds mirrors
// bridge_routing.rs:SubmissionPolicyThresholds.
type SubmissionPolicyThresholds struct {
	MaxPerDay               uint32 `cbor:"max_per_day"`
	MaxRecipientsPerMessage uint32 `cbor:"max_recipients_per_message"`
}

// ImapPolicy mirrors bridge_routing.rs:ImapPolicy. Catalog rows are in
// docs/goal/behavior/mail-policy-config.md § IMAP server policy.
type ImapPolicy struct {
	IdleTimeoutSecs        uint32 `cbor:"idle_timeout_secs"`
	TombstoneRetentionDays uint32 `cbor:"tombstone_retention_days"`
	DeleteNonempty         string `cbor:"delete_nonempty"`
	BodyStructureCacheMax  uint32 `cbor:"bodystructure_cache_max"`
	// StorageBytesDefault is the per-actor STORAGE quota ceiling in
	// bytes (RFC 9208 RES-STORAGE; default 1 GiB). The MDA divides by
	// 1024 to publish KiB on the IMAP wire per RFC 9208 §3.2.
	StorageBytesDefault uint64 `cbor:"storage_bytes_default"`
	// MessageCountDefault is the per-actor MESSAGE quota ceiling
	// (RFC 9208 RES-MESSAGE; default 50_000).
	MessageCountDefault uint32 `cbor:"message_count_default"`
}

// OutboundPolicy mirrors bridge_routing.rs:OutboundPolicy. Catalog
// rows in docs/goal/behavior/mail-policy-config.md § Outbound delivery;
// the bridge reads this once at fetch_config and re-applies on a
// fauna.bridges.config_changed push (no CLI / env-var paths).
type OutboundPolicy struct {
	RetryScheduleSeconds         []uint64 `cbor:"retry_schedule_seconds"`
	PermanentFailureTimeoutHours uint32   `cbor:"permanent_failure_timeout_hours"`
	DelayWarningAtHours          uint32   `cbor:"delay_warning_at_hours"`
	NDRRateLimitDays             uint32   `cbor:"ndr_rate_limit_days"`
	SuppressNDRSPFHardfail       bool     `cbor:"suppress_ndr_spf_hardfail"`
	SuppressNDRDMARCReject       bool     `cbor:"suppress_ndr_dmarc_reject"`
	PostmasterCCBounces          bool     `cbor:"postmaster_cc_bounces"`
	TLSRPTSendReports            bool     `cbor:"tlsrpt_send_reports"`
	IPv6Enabled                  bool     `cbor:"ipv6_enabled"`
	Treat5xxAsTransient          []string `cbor:"treat_5xx_as_transient"`
}

// ConfigSnapshot mirrors FetchConfigReply. Phase B.1 owns the
// operator-hatch + subscribe-loop on top of this primitive.
//
// MailEnabled reflects the admin's "enable mail" toggle on nest:
// false → the bridge's listener idles instead of binding; true →
// listen. Production path is "always true while the bridge is up"
// (supervisor gates process lifetime on the same signal); the field
// exists so the bridge handles a mid-transition snapshot gracefully
// and so test fixtures can deterministic-ally exercise the idle path.
type ConfigSnapshot struct {
	MailEnabled bool `cbor:"mail_enabled"`
	// CalDAVEnabled reflects the admin's "enable CalDAV" toggle on nest —
	// the calendar twin of MailEnabled. The MDA binds its CalDAV (443→:8444)
	// listener iff this is true, independently of MailEnabled (which gates
	// the IMAP 993/143 listeners): one MDA bridge serves both protocols from
	// one TLS termination, each listener gated by its own flag, so a
	// deployment can run calendar without email or email without calendar
	// (docs/goal/behavior/caldav-server.md § Independent enablement). nest
	// falls back to mail_enabled when the admin never set it explicitly, so an
	// admin who never touched the toggle gets the unified behavior.
	CalDAVEnabled bool `cbor:"caldav_enabled"`
	// CalDAVPort is the admin-set CalDAV listener port (default
	// [DefaultCalDAVPort] = 8443). A port a human picks is a client UI setting
	// backed by nest state, never a
	// config file/env — so the admin chooses it from any client, nest stores it,
	// and the MDA binds it directly (`<host>:<port>`) on a bare-IP / desktop /
	// domainless box that has no SNI router (caldav-server.md § Network
	// exposure). The operator-hatch `caldav_listen_https` (the loopback IPC port
	// the SNI router targets on a domain box, or the desktop supervisor's
	// `<iface>:<port>`) still wins when present — this port drives only the
	// no-hatch direct listener. An absent or zero field decodes to 0 (the
	// nest always sends a non-zero port); [EffectiveCalDAVPort] normalizes
	// 0 → [DefaultCalDAVPort] so a non-conforming snapshot never binds port 0. Changing it sends a `config_changed`
	// (`config_change_reason::CALDAV_PORT`) → the MDA exits 0 for a supervisor
	// rebind on the new port (the in-process listener can't be re-bound live).
	CalDAVPort uint16 `cbor:"caldav_port"`
	// CardDAVEnabled reflects the admin's "enable CardDAV" toggle on nest — the
	// contacts twin of CalDAVEnabled. The MDA serves its CardDAV path handler
	// (`/carddav/{user}/…`, beside `/caldav/…` on the SAME DAV listener —
	// CardDAV rides the existing CalDAVPort, there is NO separate CardDAV port)
	// iff this is true, independently of MailEnabled and CalDAVEnabled: one MDA
	// bridge serves mail + calendar + contacts from one TLS termination, each
	// listener/handler gated by its own flag, so a deployment can run contacts
	// without email or calendar (see docs/goal/behavior/carddav-server.md).
	// nest falls back to mail_enabled when the admin never set it
	// explicitly, so a real-domain deployment gets a contacts surface out of
	// the box (CardDAV needs no MX/DKIM, only the HTTPS surface). An absent
	// field decodes to false (Rust `#[serde(default)]`; the nest always sends it).
	CardDAVEnabled bool `cbor:"carddav_enabled"`
	// WebDAVEnabled reflects the admin's "enable WebDAV" toggle on nest — the
	// files twin of CardDAVEnabled. The MDA serves its WebDAV path handler
	// (`/webdav/{user}/…`, beside `/caldav/…` + `/carddav/…` on the SAME DAV
	// listener — WebDAV rides the existing CalDAVPort, there is NO separate
	// WebDAV port) iff this is true, independently of MailEnabled, CalDAVEnabled,
	// and CardDAVEnabled: one MDA bridge serves mail + calendar + contacts +
	// files from one TLS termination, each listener/handler gated by its own
	// flag, so a deployment can run files without email/calendar/contacts
	// (docs/goal/behavior/webdav-server.md § Independent enablement). nest falls
	// back to mail_enabled when the admin never set it explicitly, so a
	// real-domain deployment gets a files surface out of the box — harmless-on,
	// since nothing is served until a set is individually flagged
	// (folders.webdav_enabled). An absent field decodes to false
	// (Rust `#[serde(default)]`; the nest always sends it).
	WebDAVEnabled bool `cbor:"webdav_enabled"`
	// LocalDomains — derived projection of `mail_domains` active rows
	// per docs/goal/behavior/mail-multidomain.md § The `mail_domains`
	// model. The MTA bridge accepts RCPT TO for any address whose
	// domain is in this list and rejects others (550 5.7.1 Relay
	// denied); the MDA scopes IMAP namespace lookups to the list.
	// Empty list ⇒ bridge idles (no domains configured).
	LocalDomains []string `cbor:"local_domains"`
	// PrimaryDomain — the `is_primary = true` row's name; empty until
	// first-domain-claim. Anchors the MX target, TLSRPT/DMARC
	// processor inboxes, MTA-STS/DKIM/ACME cert chain per
	// mail-multidomain.md § The primary domain. The bridge's single-
	// domain TLS provider + EHLO host consume this field.
	PrimaryDomain string `cbor:"primary_domain"`
	// DkimSelectors — one entry per active mail_domains row (its name + the
	// `s=` tag / `<selector>._domainkey.<domain>` DNS label), projected by
	// nest (defaulting to "default" when the column is NULL). The nest holds
	// every DKIM key and signs at the outbound hand-out; the bridge reads
	// only whether the list is non-empty — the deployment signs outbound
	// mail, so the submission door holds the From: header to a local domain
	// (`550 5.7.7 From: domain not local`, mail-multidomain.md
	// § Signing-key selection).
	DkimSelectors []DomainDkimSelector       `cbor:"dkim_selectors"`
	Spam          SpamPolicyThresholds       `cbor:"spam"`
	Auth          AuthPolicy                 `cbor:"auth"`
	Submission    SubmissionPolicyThresholds `cbor:"submission"`
	IMAP          ImapPolicy                 `cbor:"imap"`
	Outbound      OutboundPolicy             `cbor:"outbound"`
	// Bridge carries the `mail.bridge.*` lifecycle policy — today just the
	// graceful-shutdown drain budget. Mirrors fauna-protocol's BridgePolicy.
	Bridge BridgePolicy `cbor:"bridge"`
	// MassMailing carries the `mail.outbound.list_*` mailing-list ceilings
	// (RFC 8058 list-mode submission). Mirrors fauna-protocol's
	// MassMailingPolicy (docs/goal/behavior/mail-mass-mailing.md § Per-list
	// rate accounting). The list-mode submission discriminator that consumes
	// these is a later item of follow-up work.
	MassMailing MassMailingPolicy `cbor:"mass_mailing"`
}

// DomainDkimSelector mirrors fauna-protocol's DomainDkimSelector — one
// active local domain's DKIM selector, projected to the MTA bridge in
// ConfigSnapshot.DkimSelectors.
type DomainDkimSelector struct {
	Domain   string `cbor:"domain"`
	Selector string `cbor:"selector"`
}

// BridgePolicy mirrors fauna-protocol's BridgePolicy (the `mail.bridge.*`
// catalog namespace). ShutdownGraceSeconds is the graceful-shutdown drain
// budget the bridge sizes its grace timer from on SIGTERM (see
// docs/goal/behavior/mail-bridge-lifecycle.md § Shutting down).
type BridgePolicy struct {
	ShutdownGraceSeconds uint32 `cbor:"shutdown_grace_seconds"`
}

// MassMailingPolicy mirrors fauna-protocol's MassMailingPolicy (the
// `mail.outbound.list_*` catalog namespace, Tier-2 admin ceilings). The MTA
// enforces these on RFC 8058 list-mode submissions: the per-send recipient
// cap (552 over-cap, no auto-chunking), the per-account and per-deployment
// per-day recipient ceilings, and the batch-import ceiling
// (docs/goal/behavior/mail-mass-mailing.md § Per-list rate accounting).
type MassMailingPolicy struct {
	ListRecipientsPerSendCeiling             uint64 `cbor:"list_recipients_per_send_ceiling"`
	ListRecipientsPerAccountPerDayCeiling    uint64 `cbor:"list_recipients_per_account_per_day_ceiling"`
	ListRecipientsPerDeploymentPerDayCeiling uint64 `cbor:"list_recipients_per_deployment_per_day_ceiling"`
	ListMaxImportPerBatch                    uint32 `cbor:"list_max_import_per_batch"`
}

// ── Catalog defaults ─────────────────────────────────────────────
//
// Mirror the Rust `Default` impls on the corresponding wire types in
// `libs/fauna-protocol/src/bridge_routing.rs` (sourced from
// `docs/goal/behavior/mail-policy-config.md`). The Rust side is the
// authority — when a catalog value changes there, update here in the
// same commit. Fresh struct per call: each contains its own slice, so
// callers can mutate without poisoning other callers.

// DefaultSpamPolicyThresholds — mirrors SpamPolicyThresholds::default().
func DefaultSpamPolicyThresholds() SpamPolicyThresholds {
	return SpamPolicyThresholds{
		MaxScoreBeforeSpamFolder: 5,
		MaxScoreBeforeReject:     0,
		DNSBLServers:             []string{"zen.spamhaus.org"},
		RejectNoRdns:             false,
		GreylistEnabled:          true,
		GreylistDelaySecs:        60,
		MaxConnPerMin:            10,
		FCrDNSMode:               "score_signal",
		HELOIdentityRequired:     true,
		RejectFCrDNSFail:         false,
		MaxMessageBytes:          50_000_000,
		// Catalog defaults — mirror BayesianKnobs::default() (700/50/200) +
		// the 30-day training-history retention (mail-spam.md § Combined-score
		// formula + § Training-sample retention).
		BayesianWeightMilli:           700,
		BayesianMinSamples:            50,
		BayesianFullConfidenceSamples: 200,
		TrainingHistoryRetentionDays:  30,
		UnlistedRecipientPenalty:      0,
	}
}

// DefaultAuthPolicy — mirrors AuthPolicy::default().
func DefaultAuthPolicy() AuthPolicy {
	return AuthPolicy{
		EnforceDmarc:             true,
		EnforceDmarcQuarantine:   true,
		EnforceSpfHardfail:       true,
		EnforceDkim:              false,
		LogOnly:                  false,
		MaxAuthFailuresPerMinute: 30,
		MaxConnPerIP:             256,
	}
}

// DefaultSubmissionPolicyThresholds — mirrors
// SubmissionPolicyThresholds::default().
func DefaultSubmissionPolicyThresholds() SubmissionPolicyThresholds {
	return SubmissionPolicyThresholds{
		MaxPerDay:               1000,
		MaxRecipientsPerMessage: 100,
	}
}

// DefaultImapPolicy — mirrors ImapPolicy::default().
func DefaultImapPolicy() ImapPolicy {
	return ImapPolicy{
		IdleTimeoutSecs:        1740,
		TombstoneRetentionDays: 30,
		DeleteNonempty:         "forbidden",
		BodyStructureCacheMax:  4096,
		StorageBytesDefault:    1 << 30, // 1 GiB
		MessageCountDefault:    50_000,
	}
}

// DefaultOutboundPolicy — mirrors OutboundPolicy::default().
func DefaultOutboundPolicy() OutboundPolicy {
	return OutboundPolicy{
		RetryScheduleSeconds:         []uint64{0, 300, 900, 3600, 14400, 43200, 86400, 86400, 86400, 86400},
		PermanentFailureTimeoutHours: 120,
		DelayWarningAtHours:          4,
		NDRRateLimitDays:             7,
		SuppressNDRSPFHardfail:       true,
		SuppressNDRDMARCReject:       true,
		PostmasterCCBounces:          false,
		TLSRPTSendReports:            true,
		IPv6Enabled:                  true,
		Treat5xxAsTransient:          []string{},
	}
}

// DefaultBridgePolicy — mirrors BridgePolicy::default().
func DefaultBridgePolicy() BridgePolicy {
	return BridgePolicy{
		ShutdownGraceSeconds: 30,
	}
}

// DefaultMassMailingPolicy — mirrors MassMailingPolicy::default().
func DefaultMassMailingPolicy() MassMailingPolicy {
	return MassMailingPolicy{
		ListRecipientsPerSendCeiling:             5_000,
		ListRecipientsPerAccountPerDayCeiling:    50_000,
		ListRecipientsPerDeploymentPerDayCeiling: 500_000,
		ListMaxImportPerBatch:                    10_000,
	}
}

// DefaultCalDAVPort mirrors Rust `bridge_routing::DEFAULT_CALDAV_PORT` — the
// admin-settable CalDAV listener port's default (caldav-server.md § Network
// exposure). The MDA binds this on a bare-IP / domainless box when no
// operator-hatch pins the listener. When the Rust constant changes, update here
// in the same commit (the cross-language fixture test does not pin the default
// itself, only the wire shape).
const DefaultCalDAVPort uint16 = 8443

// EffectiveCalDAVPort normalizes a snapshot's CalDAVPort: the Go twin of
// the Rust serde default. The nest always sends a non-zero port (from
// get_caldav_port, else 8443); a zero or non-conforming snapshot would bind an
// ephemeral port, so treat 0 as [DefaultCalDAVPort] — never bind 0.
// Both the bind path (main.go) and the port-change applier (mda.go) route
// through this so they agree on the effective port (a 0 reads as 8443 on both
// sides → no spurious rebind).
func EffectiveCalDAVPort(p uint16) uint16 {
	if p == 0 {
		return DefaultCalDAVPort
	}
	return p
}

// DefaultConfigSnapshot — mirrors FetchConfigReply::default(). Tests
// that need the production fetch_config shape should call this and
// then override only the fields they care about; new wire fields
// land in one place (the per-policy default funcs above) instead of
// fanning out to every fixture.
func DefaultConfigSnapshot() ConfigSnapshot {
	return ConfigSnapshot{
		MailEnabled:   true,
		CalDAVEnabled: true,
		CalDAVPort:    DefaultCalDAVPort,
		LocalDomains:  []string{},
		PrimaryDomain: "",
		Spam:          DefaultSpamPolicyThresholds(),
		Auth:          DefaultAuthPolicy(),
		Submission:    DefaultSubmissionPolicyThresholds(),
		IMAP:          DefaultImapPolicy(),
		Outbound:      DefaultOutboundPolicy(),
		Bridge:        DefaultBridgePolicy(),
		MassMailing:   DefaultMassMailingPolicy(),
	}
}

// fetchConfigRequest mirrors FetchConfigRequest. The scope selector
// is forward-compatible — Slice 1 of the nest implementation only
// accepts `"all"`; other scopes return a typed error.
type fetchConfigRequest struct {
	Scope string `cbor:"scope"`
}

// FetchConfig returns the per-role config snapshot. Phase B.1 owns
// the operator-hatch + subscribe-loop on top of this primitive.
func FetchConfig(ctx context.Context, c Caller, scope string) (ConfigSnapshot, error) {
	var reply ConfigSnapshot
	if err := c.Call(ctx, MethodFetchConfig, fetchConfigRequest{Scope: scope}, &reply); err != nil {
		return ConfigSnapshot{}, fmt.Errorf("fetch_config: %w", err)
	}
	return reply, nil
}

// ── report_auth_event ────────────────────────────────────────────

// reportAuthEventRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:ReportAuthEventRequest.
// Reason is Option<String> on the Rust side — `*string` here so a nil
// pointer encodes as CBOR null (matching Rust None), and a non-nil
// pointer encodes the string (matching Some(s)).
type reportAuthEventRequest struct {
	ActorID      []byte  `cbor:"actor_id"`
	CredentialID string  `cbor:"credential_id"`
	Result       string  `cbor:"result"`
	SourceIP     string  `cbor:"source_ip"`
	OccurredAt   uint64  `cbor:"occurred_at"`
	Reason       *string `cbor:"reason"`
}

// reportAuthEventReply mirrors ReportAuthEventReply.
type reportAuthEventReply struct {
	OK bool `cbor:"ok"`
}

// ReportAuthEvent logs an authentication event to nest. `occurredAt`
// is supplied by the bridge in epoch milliseconds; if the caller
// passes 0, nest will reject the request. Empty `reason` is encoded
// as None (no diagnostic text); a non-empty reason rides as Some.
func ReportAuthEvent(ctx context.Context, c Caller, actorID []byte, credentialID, result, sourceIP, reason string, occurredAt uint64) error {
	req := reportAuthEventRequest{
		ActorID:      actorID,
		CredentialID: credentialID,
		Result:       result,
		SourceIP:     sourceIP,
		OccurredAt:   occurredAt,
	}
	if reason != "" {
		req.Reason = &reason
	}
	var reply reportAuthEventReply
	if err := c.Call(ctx, MethodReportAuthEvent, req, &reply); err != nil {
		return fmt.Errorf("report_auth_event: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("report_auth_event: nest returned ok=false")
	}
	return nil
}

// ── fetch capability grants ──────────────────────────────────────

// fetchCapabilityGrantsRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:FetchGrantsRequest — a body with
// NO fields (the holder identity is the authenticated caller; there is
// deliberately nothing to spoof). Encodes to an empty CBOR map, matching the
// Rust struct whose only field is a `#[serde(flatten)]` catch-all.
type fetchCapabilityGrantsRequest struct{}

// fetchCapabilityGrantsReply mirrors FetchGrantsReply. `grants` is 0..n
// canonical-encoded `GrantBlob`s, each opened by unseal_capability_grant.
type fetchCapabilityGrantsReply struct {
	Grants [][]byte `cbor:"grants"`
}

// FetchCapabilityGrants pulls the capability grants nest has sealed to THIS
// bridge's enrolled x25519 (holder-pubkey-self-scoped, expiry-filtered). The
// nest omits revoked/expired grants — a successful call returning fewer (or
// zero) grants than a prior call is how honest-box revocation bites, and the
// capability holder loop distinguishes that from a transport error (which it
// treats as transient and keeps the cached set). Design § Phase 2 Step 2 § 2.3.
//
// Returns the raw canonical `GrantBlob` bytes; the caller unwraps each via
// mailfauna.UnsealCapabilityGrant with the bridge's holder secret.
func FetchCapabilityGrants(ctx context.Context, c Caller) ([][]byte, error) {
	var reply fetchCapabilityGrantsReply
	if err := c.Call(ctx, MethodFetchCapabilityGrants, fetchCapabilityGrantsRequest{}, &reply); err != nil {
		return nil, fmt.Errorf("fetch_capability_grants: %w", err)
	}
	return reply.Grants, nil
}

// ── The re-score drain plane (fauna.capabilities.{rescore_worklist,submit_scores}) ──

// RescoreUnit mirrors libs/fauna-protocol/src/wrapped_blob.rs:RescoreUnit —
// one unit of re-score work: an item whose `content_scores.scorer_version`
// for Factor is behind the current model version, scoped to an owner the
// holder holds a `content.read{kind}` grant for. The drain maps ContentID →
// the sealed record (for mail it IS the fetch_message_ciphertext message_id),
// unseals it under the grant key, re-runs the Factor scorer, and submits a
// fresh row stamped at ToVersion.
type RescoreUnit struct {
	ContentID    []byte `cbor:"content_id"`
	ContentKind  string `cbor:"content_kind"`
	OwnerActorID []byte `cbor:"owner_actor_id"`
	Factor       string `cbor:"factor"`
	FromVersion  uint32 `cbor:"from_version"`
	ToVersion    uint32 `cbor:"to_version"`
}

// rescoreWorklistRequest mirrors RescoreWorklistRequest. Limit 0 → the server
// default cap (512).
type rescoreWorklistRequest struct {
	Limit uint32 `cbor:"limit"`
}

// rescoreWorklistReply mirrors RescoreWorklistReply.
type rescoreWorklistReply struct {
	Units []RescoreUnit `cbor:"units"`
}

// RescoreWorklist asks the nest "what re-processing do I owe?" — the holder
// identity is the authenticated caller; the nest intersects this holder's
// `content.read{kind}` grants with the per-factor obligation gap and returns
// only work for owners the holder can actually unseal (a metadata-
// confidentiality boundary). A full batch (len == limit) means more remain;
// the drain loops until an empty (or shorter) reply. Content-free.
func RescoreWorklist(ctx context.Context, c Caller, limit uint32) ([]RescoreUnit, error) {
	var reply rescoreWorklistReply
	if err := c.Call(ctx, MethodRescoreWorklist, rescoreWorklistRequest{Limit: limit}, &reply); err != nil {
		return nil, fmt.Errorf("rescore_worklist: %w", err)
	}
	return reply.Units, nil
}

// SubmitScoreRow mirrors SubmitScoreRow — one re-scored item written back;
// Entries replace the `(content_id, factor)` rows in `content_scores`, each
// stamped with the worklist's ToVersion (closing the obligation gap). The
// nest authz's each row against the holder's `content.label-write` grant for
// OwnerActorID and rejects the whole batch on any unauthorized row
// (fail-closed) — so the drain batches rows per owner.
type SubmitScoreRow struct {
	ContentID    []byte       `cbor:"content_id"`
	ContentKind  string       `cbor:"content_kind"`
	OwnerActorID []byte       `cbor:"owner_actor_id"`
	ScoredAt     uint64       `cbor:"scored_at"`
	Entries      []ScoreEntry `cbor:"entries"`
}

// submitScoresRequest mirrors SubmitScoresRequest.
type submitScoresRequest struct {
	Rows []SubmitScoreRow `cbor:"rows"`
}

// submitScoresReply mirrors SubmitScoresReply.
type submitScoresReply struct {
	Written uint32 `cbor:"written"`
	Ok      bool   `cbor:"ok"`
}

// SubmitScores writes re-computed score rows back after draining a worklist.
// Content-free — only ScoreEntry metadata crosses; the nest never sees the
// plaintext the scores were derived from. Returns the count of
// `(content_id, factor)` rows written.
func SubmitScores(ctx context.Context, c Caller, rows []SubmitScoreRow) (uint32, error) {
	var reply submitScoresReply
	if err := c.Call(ctx, MethodSubmitScores, submitScoresRequest{Rows: rows}, &reply); err != nil {
		return 0, fmt.Errorf("submit_scores: %w", err)
	}
	if !reply.Ok {
		return reply.Written, fmt.Errorf("submit_scores: nest reported ok=false")
	}
	return reply.Written, nil
}

// ── fauna.capabilities.{spam_baseline_worklist,submit_spam_baseline} ──────

// SpamBaselineCopy mirrors libs/fauna-protocol/src/wrapped_blob.rs:
// SpamBaselineCopy — one sealed-to-holder model copy on a publish-run
// worklist: a `spam_model_holder_copies` row whose owner is opted in, whose
// paired keyless `content.read{spam-model}` grant to this holder stands, and
// whose current model row is client-sealed. OwnerActorID is audit attribution
// (cross-checked against the blob's own AAD-bound owner at open time);
// SealedCopy is the canonical SpamModelCopyBlob bytes the holder opens with its
// OWN service-user key.
type SpamBaselineCopy struct {
	OwnerActorID []byte `cbor:"owner_actor_id"`
	SealedCopy   []byte `cbor:"sealed_copy"`
}

// spamBaselineWorklistRequest mirrors SpamBaselineWorklistRequest. run_id is the
// 16-byte pending-run id from the `spam_baseline_publish` push (serde ByteBuf →
// CBOR byte-string).
type spamBaselineWorklistRequest struct {
	RunID []byte `cbor:"run_id"`
}

// spamBaselineWorklistReply mirrors SpamBaselineWorklistReply.
type spamBaselineWorklistReply struct {
	Copies []SpamBaselineCopy `cbor:"copies"`
}

// SpamBaselineWorklist pulls the grant-gated sealed copies the holder may
// unseal + merge for one publish run — valid only while the named run is
// pending (an unknown/expired run id is a typed error from the nest). The
// holder identity is the authenticated caller (no field to spoof). An empty
// reply means nothing mergeable; the holder submits an empty half.
func SpamBaselineWorklist(ctx context.Context, c Caller, runID []byte) ([]SpamBaselineCopy, error) {
	var reply spamBaselineWorklistReply
	if err := c.Call(ctx, MethodSpamBaselineWorklist, spamBaselineWorklistRequest{RunID: runID}, &reply); err != nil {
		return nil, fmt.Errorf("spam_baseline_worklist: %w", err)
	}
	return reply.Copies, nil
}

// submitSpamBaselineRequest mirrors SubmitSpamBaselineRequest. merged_model is
// the additively-merged plaintext SpamModel bytes (an aggregate over ≥1
// contributors, never a single user's model) — empty ⇒ nothing merged
// (serde_bytes → CBOR byte-string). contributors is counted toward the
// k-anonymity floor nest-side (defensively clamped there to the served
// population); unreadable is advisory diagnostics for the publish reply's
// erosion count. merged_contributors names the worklist owners whose copies
// actually merged (additive 2026-09-27), so the nest records exactly those as
// summed and a skipped contributor's later departure withdraws nothing
// (mail-spam.md § Cold start Path 2 → A contributor's departure withdraws the
// baseline). The codec encodes a nil slice as an empty array, never CBOR null.
type submitSpamBaselineRequest struct {
	RunID              []byte   `cbor:"run_id"`
	MergedModel        []byte   `cbor:"merged_model"`
	Contributors       uint32   `cbor:"contributors"`
	Unreadable         uint32   `cbor:"unreadable"`
	MergedContributors [][]byte `cbor:"merged_contributors"`
}

// submitSpamBaselineReply mirrors SubmitSpamBaselineReply.
type submitSpamBaselineReply struct {
	Ok bool `cbor:"ok"`
}

// SubmitSpamBaseline writes the holder's off-box merge back for a pending
// publish run. Idempotent against an unknown/expired run: the nest replies
// ok=false (never an error — the publish may have timed out while the holder
// merged, which is exactly the "holder too slow" outcome the bounded await
// already priced in), so a false return is reported but not fatal to the
// drain. Returns whether the submission reached the pending run.
func SubmitSpamBaseline(ctx context.Context, c Caller, runID, mergedModel []byte, contributors, unreadable uint32, mergedContributors [][]byte) (bool, error) {
	var reply submitSpamBaselineReply
	req := submitSpamBaselineRequest{
		RunID:              runID,
		MergedModel:        mergedModel,
		Contributors:       contributors,
		Unreadable:         unreadable,
		MergedContributors: mergedContributors,
	}
	if err := c.Call(ctx, MethodSubmitSpamBaseline, req, &reply); err != nil {
		return false, fmt.Errorf("submit_spam_baseline: %w", err)
	}
	return reply.Ok, nil
}

// ── fauna.labelers.inspect (community-labeler module fetch) ──────

// inspectLabelerRequest mirrors libs/fauna-protocol/src/labelers.rs:
// InspectLabelerRequest. `labeler_id` is the 32-byte AlgorithmLabeler
// algorithm_id (serde_bytes on the Rust side → CBOR byte-string).
type inspectLabelerRequest struct {
	LabelerID []byte `cbor:"labeler_id"`
}

// inspectLabelerReply mirrors InspectLabelerReply — the canonical-CBOR
// AlgorithmLabeler metadata blob (carries input_schema, resource_limits,
// signature) + the full WASM module bytes.
type inspectLabelerReply struct {
	MetadataBlob []byte `cbor:"metadata_blob"`
	WasmBytes    []byte `cbor:"wasm_bytes"`
}

// InspectLabeler fetches a community-labeler's signed metadata + WASM bytes so a
// re-score-drain holder can run `label()` over unsealed content (Slice 3b). The
// artifact is public/transparent (inspect-before-subscribe); the holder MUST
// re-verify the signature + wasm_hash before instantiation (security review B1) —
// done inside the shared `run_wasm_labeler_score` FFI, not here.
func InspectLabeler(ctx context.Context, c Caller, labelerID []byte) (metadataBlob, wasmBytes []byte, err error) {
	var reply inspectLabelerReply
	if err := c.Call(ctx, MethodInspectLabeler, inspectLabelerRequest{LabelerID: labelerID}, &reply); err != nil {
		return nil, nil, fmt.Errorf("inspect_labeler: %w", err)
	}
	return reply.MetadataBlob, reply.WasmBytes, nil
}

// ── validate_recipient ───────────────────────────────────────────

// validateRecipientRequest mirrors ValidateRecipientRequest.
type validateRecipientRequest struct {
	LocalPart string `cbor:"local_part"`
	Domain    string `cbor:"domain"`
}

// validateRecipientReply mirrors the Rust enum
// `ValidateRecipientReply` with `#[serde(tag = "outcome",
// rename_all = "snake_case")]`. Serde emits one CBOR map with an
// `outcome` discriminator field plus the variant's payload fields
// (`actor_id` for Resolved, `reason` for Reject) flat in the same
// map. We decode into a flat struct and let the caller branch on
// `Outcome`.
type validateRecipientReply struct {
	Outcome string `cbor:"outcome"`
	ActorID []byte `cbor:"actor_id,omitempty"`
	Reason  string `cbor:"reason,omitempty"`
	// IsRoleAddress is true when nest resolved this recipient via a
	// role-address route (postmaster@/abuse@/security@/tlsrpt@/…). Role
	// addresses bypass the recipient's per-mailbox quota on inbound delivery
	// (smtp-server.md :204) — the caller carries this through to the ingest
	// request so an over-quota admin mailbox still receives postmaster mail.
	IsRoleAddress bool `cbor:"is_role_address,omitempty"`
}

// ErrRecipientRejected is the sentinel error returned by
// ValidateRecipient when nest's reply outcome is "reject" — the
// caller (MTA's Session.Rcpt) tests for it via `errors.Is` to map a
// nest-side reject to `550 5.1.1` on the SMTP wire, vs `451 4.7.0`
// for an internal / transport error.
var ErrRecipientRejected = errors.New("validate_recipient: rejected")

// ValidateRecipient resolves an incoming RCPT TO to a local actor.
// Returns the 32-byte actor_id on success; nil + a wrapped
// `ErrRecipientRejected` on a nest-side reject (the wrap carries the
// reason as a postfix). Any other non-nil error is a transport /
// decode / unknown-outcome failure — callers map those to 451 4.7.0
// "try again later" rather than 550 5.1.1.
//
// `isRoleAddress` is true when nest resolved the recipient via a
// role-address route (postmaster@/abuse@/…); recipient-resolution callers
// (the MTA inbound + submission paths) carry it onto the ingest request so
// the delivery bypasses the recipient's per-mailbox quota (smtp-server.md
// :204). AUTH-time callers discard it.
func ValidateRecipient(ctx context.Context, c Caller, localPart, domain string) (actorID []byte, isRoleAddress bool, err error) {
	req := validateRecipientRequest{LocalPart: localPart, Domain: domain}
	var reply validateRecipientReply
	if err := c.Call(ctx, MethodValidateRecipient, req, &reply); err != nil {
		return nil, false, fmt.Errorf("validate_recipient: %w", err)
	}
	switch reply.Outcome {
	case "resolved":
		// Defensive: nest always sends a 32-byte actor_id, but a
		// malformed reply (zero-length or wrong-length) shouldn't be
		// silently propagated as a valid resolution.
		if len(reply.ActorID) == 0 {
			return nil, false, fmt.Errorf("validate_recipient: resolved outcome with empty actor_id")
		}
		return reply.ActorID, reply.IsRoleAddress, nil
	case "reject":
		return nil, false, fmt.Errorf("%w: %s", ErrRecipientRejected, reply.Reason)
	default:
		return nil, false, fmt.Errorf("validate_recipient: unknown outcome %q", reply.Outcome)
	}
}

// ── resolve_recipient (the fixed-order RCPT-TO resolver, superset of
// validate_recipient — mail-aliases.md § Resolution order) ───────────

// ResolveRecipientOutcome names one variant of the Rust
// `ResolveRecipientReply` `outcome` discriminator. The wire-level snake_case
// string is preserved verbatim so future variants land additively.
type ResolveRecipientOutcome string

const (
	// ResolveResolved — route to a local actor. ActorID is set; HeadersToStamp
	// carries the X-Fauna-Address-* headers the MDA stamps before filter rules
	// run; ControlOverrides carries the per-alias spam/rate overrides;
	// IsRoleAddress is true for the postmaster@/abuse@ never-reject route (the
	// caller carries it onto ingest to bypass per-mailbox quota + greylisting).
	ResolveResolved ResolveRecipientOutcome = "resolved"
	// ResolveForward — an admin external forwarder matched (mail-aliases.md
	// § Kind 7): no local delivery. ForwardTarget is the external destination,
	// ForwarderActorID the managing-admin actor (the SRS / rate-cap / NDR
	// principal). The caller hands these to the forward dispatch.
	ResolveForward ResolveRecipientOutcome = "forward"
	// ResolveReject — no route. SMTPCode + Reason map to the SMTP wire (550
	// user-unknown / disabled / expired; 451 once the bridge enforces rate caps).
	ResolveReject ResolveRecipientOutcome = "reject"
	// ResolveDiscard — a deployment-internal envelope-command address whose side
	// effect the nest already performed during resolution (today:
	// `unsubscribe+<token>@`, the RFC 8058 mailto one-click unsubscribe —
	// mail-mass-mailing.md § The mailto handler). The caller accepts the RCPT
	// with 250 and discards the body (never delivers to a mailbox); no payload
	// fields. Bare `unsubscribe@` with no token is a ResolveReject{550}, not this.
	ResolveDiscard ResolveRecipientOutcome = "discard"
)

// resolveRecipientRequest mirrors ResolveRecipientRequest. Reuses
// validate_recipient's {local_part, domain} shape (the bridge already splits
// the RCPT this way) plus the MAIL FROM `sender_domain` for the resolver's
// alias-hit audit log (`#[serde(default)]` nest-side keeps it optional).
type resolveRecipientRequest struct {
	LocalPart    string `cbor:"local_part"`
	Domain       string `cbor:"domain"`
	SenderDomain string `cbor:"sender_domain,omitempty"`
	// SenderAddress is the full envelope MAIL FROM. The nest's guardian mail
	// gate matches it against the recipient's known-sender allowlist and may
	// answer Reject (`family-safety.md` § The mail gate); `sender_domain` alone
	// cannot key that decision. Omitted when empty (the SMTP null reverse-path),
	// which the nest reads as a bounce and never gates.
	SenderAddress string `cbor:"sender_address,omitempty"`
}

// StampedHeader mirrors the Rust `StampedHeader` — one X-Fauna-Address-*
// header the MDA stamps onto the message before the recipient's filter rules
// run (subaddress / wildcard / disposable / catch-all). Exact + role-address
// resolves stamp none.
type StampedHeader struct {
	Name  string `cbor:"name"`
	Value string `cbor:"value"`
}

// AliasControls mirrors the Rust `AliasControls` carried on a Resolved
// outcome as the per-alias control overrides the bridge applies. `Label` is
// unset on the resolver path (it carries routing controls, not the UI tag).
// Each pointer is nil = inherit / unlimited.
type AliasControls struct {
	Label                 string  `cbor:"label,omitempty"`
	SpamThresholdOverride *uint32 `cbor:"spam_threshold_override,omitempty"`
	RateLimitPerHour      *int64  `cbor:"rate_limit_per_hour,omitempty"`
	RateLimitPerDay       *int64  `cbor:"rate_limit_per_day,omitempty"`
}

// resolveRecipientReply is the flat decode of the Rust enum
// `ResolveRecipientReply` (`#[serde(tag = "outcome", rename_all =
// "snake_case")]`): one CBOR map with the `outcome` discriminator plus the
// matched variant's payload fields flat in the same map. The caller branches
// on Outcome and reads only that variant's fields.
type resolveRecipientReply struct {
	Outcome          string          `cbor:"outcome"`
	ActorID          []byte          `cbor:"actor_id,omitempty"`
	HeadersToStamp   []StampedHeader `cbor:"headers_to_stamp,omitempty"`
	ControlOverrides AliasControls   `cbor:"control_overrides,omitempty"`
	IsRoleAddress    bool            `cbor:"is_role_address,omitempty"`
	ForwardTarget    string          `cbor:"forward_target,omitempty"`
	ForwarderActorID []byte          `cbor:"forwarder_actor_id,omitempty"`
	SMTPCode         uint16          `cbor:"smtp_code,omitempty"`
	Reason           string          `cbor:"reason,omitempty"`
}

// ResolveRecipientResult is the typed return of ResolveRecipient. Only the
// fields for the named Outcome are meaningful; the rest are zero.
type ResolveRecipientResult struct {
	Outcome ResolveRecipientOutcome
	// Resolved:
	ActorID          []byte
	HeadersToStamp   []StampedHeader
	ControlOverrides AliasControls
	IsRoleAddress    bool
	// Forward:
	ForwardTarget    string
	ForwarderActorID []byte
	// Reject:
	SMTPCode uint16
	Reason   string
}

// ResolveRecipient resolves an incoming RCPT TO through the fixed-order alias
// resolver (mail-aliases.md § Resolution order: exact → forwarder → +suffix →
// disposable → wildcard → role-address → catch-all → 550). It is the superset
// of ValidateRecipient and supersedes it once the Go MTA migrates both call
// sites. A non-nil error is a transport / decode / unknown-outcome
// failure — the caller tempfails 451 rather than guessing an outcome (the
// Reject *outcome* is a normal, non-error result the caller maps to the
// reply's SMTPCode).
func ResolveRecipient(ctx context.Context, c Caller, localPart, domain, senderDomain, senderAddress string) (ResolveRecipientResult, error) {
	req := resolveRecipientRequest{
		LocalPart:     localPart,
		Domain:        domain,
		SenderDomain:  senderDomain,
		SenderAddress: senderAddress,
	}
	var reply resolveRecipientReply
	if err := c.Call(ctx, MethodResolveRecipient, req, &reply); err != nil {
		return ResolveRecipientResult{}, fmt.Errorf("resolve_recipient: %w", err)
	}
	switch ResolveRecipientOutcome(reply.Outcome) {
	case ResolveResolved:
		if len(reply.ActorID) == 0 {
			return ResolveRecipientResult{}, fmt.Errorf("resolve_recipient: resolved outcome with empty actor_id")
		}
	case ResolveForward:
		if reply.ForwardTarget == "" || len(reply.ForwarderActorID) == 0 {
			return ResolveRecipientResult{}, fmt.Errorf("resolve_recipient: forward outcome missing target/forwarder")
		}
	case ResolveReject:
		// Normal result — the caller maps SMTPCode/Reason to the wire.
	case ResolveDiscard:
		// Normal result — the caller accepts the RCPT (250) and discards the
		// body; no payload fields to validate.
	default:
		return ResolveRecipientResult{}, fmt.Errorf("resolve_recipient: unknown outcome %q", reply.Outcome)
	}
	return ResolveRecipientResult{
		Outcome:          ResolveRecipientOutcome(reply.Outcome),
		ActorID:          reply.ActorID,
		HeadersToStamp:   reply.HeadersToStamp,
		ControlOverrides: reply.ControlOverrides,
		IsRoleAddress:    reply.IsRoleAddress,
		ForwardTarget:    reply.ForwardTarget,
		ForwarderActorID: reply.ForwarderActorID,
		SMTPCode:         reply.SMTPCode,
		Reason:           reply.Reason,
	}, nil
}

// ── check_greylist ───────────────────────────────────────────────

// checkGreylistRequest mirrors CheckGreylistRequest. The bridge forwards the
// raw envelope addresses + peer IP; nest derives the
// `(sender_domain, recipient, subnet)` tuple and applies the policy, so the
// bridge holds no greylist state (uniform across restart).
type checkGreylistRequest struct {
	From     string `cbor:"from"`
	To       string `cbor:"to"`
	ClientIP string `cbor:"client_ip"`
}

// checkGreylistReply mirrors CheckGreylistReply (float-free bool).
type checkGreylistReply struct {
	Pass bool `cbor:"pass"`
}

// CheckGreylist asks nest whether to accept this `(from, to, client_ip)`
// envelope or tempfail it (greylist deferral). Returns `pass=true` to accept,
// `pass=false` to defer (the caller maps that to `451 4.7.1 Greylisted`).
//
// **Fail-open on error:** a transport / decode failure returns `pass=true`
// with the error, so the caller never tempfails legitimate mail when nest
// blips — greylisting is a deferral, so failing it open (accept) is the safe
// direction, unlike `validate_recipient` (which fails closed to 451). The MTA
// logs the error but proceeds.
func CheckGreylist(ctx context.Context, c Caller, from, to, clientIP string) (pass bool, err error) {
	req := checkGreylistRequest{From: from, To: to, ClientIP: clientIP}
	var reply checkGreylistReply
	if err := c.Call(ctx, MethodCheckGreylist, req, &reply); err != nil {
		// Fail open — see doc comment.
		return true, fmt.Errorf("check_greylist: %w", err)
	}
	return reply.Pass, nil
}

// ── fetch_wrapped_submission_token ───────────────────────────────

// fetchWrappedSubmissionTokenRequest mirrors
// FetchWrappedSubmissionTokenRequest.
type fetchWrappedSubmissionTokenRequest struct {
	ActorID      []byte `cbor:"actor_id"`
	CredentialID string `cbor:"credential_id"`
}

// fetchWrappedSubmissionTokenReply mirrors
// FetchWrappedSubmissionTokenReply.
type fetchWrappedSubmissionTokenReply struct {
	Blob *[]byte `cbor:"blob"`
}

// FetchWrappedSubmissionToken returns the wrapped submission token
// for a (actor, credential) pair. Phase D submission auth consumes
// this; the bridge unwraps with the MUA-supplied credential before
// using the token for outbound submission quota enforcement.
func FetchWrappedSubmissionToken(ctx context.Context, c Caller, actorID []byte, credentialID string) ([]byte, error) {
	req := fetchWrappedSubmissionTokenRequest{ActorID: actorID, CredentialID: credentialID}
	var reply fetchWrappedSubmissionTokenReply
	if err := c.Call(ctx, MethodFetchWrappedSubmissionToken, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_wrapped_submission_token: %w", err)
	}
	if reply.Blob == nil {
		return nil, nil
	}
	return *reply.Blob, nil
}

// ── check_submission_quota ───────────────────────────────────────

// checkSubmissionQuotaRequest mirrors CheckSubmissionQuotaRequest.
// Note: the spec body refers to a `submission_capability` parameter,
// but the Rust wire type pins it down to (actor_id, recipient_count)
// — the bridge identifies the submitter by the unwrapped capability's
// embedded actor_id, not by sending the capability bytes themselves.
type checkSubmissionQuotaRequest struct {
	ActorID []byte `cbor:"actor_id"`
	// RecipientCount is the running count of this submission's accepted
	// recipients including this one — nest's per-message cap input, and
	// never what it debits.
	RecipientCount uint32 `cbor:"recipient_count"`
	// RecipientIsLocal says the recipient this call is for was placed in a
	// mailbox on this deployment at RCPT time; nest then debits nothing for
	// it (smtp-server.md § Architectural rules, the charging rule). Additive
	// 2026-09-25: an older nest ignores it and keeps charging every RCPT.
	RecipientIsLocal bool `cbor:"recipient_is_local"`
}

// checkSubmissionQuotaReply mirrors the
// `CheckSubmissionQuotaReply` enum (outcome-discriminated).
type checkSubmissionQuotaReply struct {
	Outcome   string `cbor:"outcome"`
	Remaining uint32 `cbor:"remaining,omitempty"`
}

// CheckSubmissionQuota is the per-RCPT quota check: recipientCount is the
// running count including this recipient (nest's per-message cap input)
// and recipientIsLocal whether this recipient stays on the deployment.
// Nest debits one unit of the daily allowance for a remote recipient and
// none for a local one — never recipientCount — so call it exactly once
// per accepted RCPT. Returns (allowed=true, _) on allowance; (false,
// remaining) when over quota with the headroom the caller can still
// consume on a retry.
func CheckSubmissionQuota(ctx context.Context, c Caller, actorID []byte, recipientCount uint32, recipientIsLocal bool) (allowed bool, remaining uint32, err error) {
	req := checkSubmissionQuotaRequest{ActorID: actorID, RecipientCount: recipientCount, RecipientIsLocal: recipientIsLocal}
	var reply checkSubmissionQuotaReply
	if err := c.Call(ctx, MethodCheckSubmissionQuota, req, &reply); err != nil {
		return false, 0, fmt.Errorf("check_submission_quota: %w", err)
	}
	switch reply.Outcome {
	case "allowed":
		return true, 0, nil
	case "over_quota":
		return false, reply.Remaining, nil
	default:
		return false, 0, fmt.Errorf("check_submission_quota: unknown outcome %q", reply.Outcome)
	}
}

// ── ingest_inbound_mail / submit_inbound_mail ────────────────────

// AuthVerdicts mirrors
// libs/fauna-protocol/src/bridge_routing.rs:AuthVerdicts — the four
// per-protocol verdicts SPF/DKIM/DMARC/ARC, each as an adjacently-
// tagged enum. The Rust side uses
// `#[serde(tag = "kind", content = "data", rename_all = "snake_case")]`
// on the verdict enums, which encodes every variant as a CBOR map
// `{"kind":"<variant>"}` (unit) or
// `{"kind":"<variant>","data":{<payload>}}` (struct-like). The
// uniform map shape lets the Go wire mirror reuse one struct shape
// per verdict — `Kind string` plus an optional `Data *Payload`
// pointer that fxamacker/cbor's `omitempty` drops for unit variants.
//
// The mailfauna package owns the UniFFI-generated Go types
// (`mailfauna.AuthVerdicts` / `mailfauna.DkimVerdict` interface +
// variant structs); use `mailfauna.AuthVerdictsToWire` to translate
// from a `verify_inbound` result into this wire shape. (This comment
// named `AuthVerdictsFromMailFauna` until 2026-08-17 — a function that
// has never existed anywhere in the tree.)
//
// ⚠ What pins this mirror, and what does not: the CBOR-tag conformance
// list in `wsrpc_conformance_test.go` covers the FIELD tags, but nothing
// asserts that the variant strings below (`"soft_fail"`, `"perm_error"`,
// …) still match the serde names of the Rust enums in
// `fauna_core::mail_auth` — which, since 2026-08-17, are the single
// definition both the wire and the UniFFI producer share. A verdict
// variant added Rust-side would leave the switches in
// `mailfauna.AuthVerdictsToWire` unable to name it, with no test
// objecting. That agreement test does not exist yet — writing it is
// the point of this note.
type AuthVerdicts struct {
	Dkim  DkimVerdict  `cbor:"dkim"`
	Spf   SpfVerdict   `cbor:"spf"`
	Dmarc DmarcVerdict `cbor:"dmarc"`
	Arc   ArcVerdict   `cbor:"arc"`
}

// DkimVerdict is the wire-shape mirror of fauna-mail::auth::
// DkimVerdict. Kind ∈ {"none","pass","fail","neutral","perm_error",
// "temp_error"}. Data is non-nil only for "fail" (carries the
// reason string).
type DkimVerdict struct {
	Kind string           `cbor:"kind"`
	Data *DkimVerdictFail `cbor:"data,omitempty"`
}

// DkimVerdictFail is the payload of DkimVerdict{Kind:"fail"}.
type DkimVerdictFail struct {
	Reason string `cbor:"reason"`
}

// SpfVerdict is the wire-shape mirror of fauna-mail::auth::
// SpfVerdict. Kind ∈ {"none","pass","fail","soft_fail","neutral",
// "perm_error","temp_error"}. All variants are unit, so there is no
// Data field.
type SpfVerdict struct {
	Kind string `cbor:"kind"`
}

// DmarcVerdict is the wire-shape mirror of fauna-mail::auth::
// DmarcVerdict. Kind ∈ {"none","pass","fail","perm_error",
// "temp_error"}. Data is non-nil only for "fail" (carries the
// DMARC policy as a bare snake_case string: "none" | "quarantine"
// | "reject").
type DmarcVerdict struct {
	Kind string            `cbor:"kind"`
	Data *DmarcVerdictFail `cbor:"data,omitempty"`
}

// DmarcVerdictFail is the payload of DmarcVerdict{Kind:"fail"}.
// Policy is the bare-string DmarcPolicy (no kind-wrapping; that
// matches the Rust definition `#[serde(rename_all = "snake_case")]`
// on the standalone DmarcPolicy enum).
type DmarcVerdictFail struct {
	Policy string `cbor:"policy"`
}

// ArcVerdict is the wire-shape mirror of fauna-mail::auth::
// ArcVerdict. Kind ∈ {"none","pass","fail","perm_error",
// "temp_error"}. All variants are unit, so there is no Data field.
type ArcVerdict struct {
	Kind string `cbor:"kind"`
}

// PublicMailMetadata mirrors
// libs/fauna-protocol/src/bridge_routing.rs:PublicMailMetadata.
// Plaintext-floor metadata per the encryption-at-rest target.
type PublicMailMetadata struct {
	Timestamp      int64  `cbor:"timestamp"`
	CiphertextSize uint32 `cbor:"ciphertext_size"`
	SenderDomain   string `cbor:"sender_domain"`
}

// ClamavVerdict mirrors fauna-protocol::bridge_routing::ClamavVerdict —
// an adjacently-tagged snake_case enum (`{"kind": ..., "data": {...}}`),
// same wire shape as DkimVerdict/DmarcVerdict above. Kind ∈
// {"clean","infected","error","bypassed_oversize","not_scanned"}. Data is non-nil only for
// the two payload-bearing variants ("infected" carries `signature`, "error"
// carries `detail`); CBOR `omitempty` drops it for the two unit variants.
//
// The bridge's perimeter scan (mta/scan_gate.go) produces a
// `mailfauna.ClamavVerdict` (the UniFFI enum); `mailfauna.ClamavVerdictToWire`
// maps it onto this wire shape, which rides into the nest on the ingest call.
type ClamavVerdict struct {
	Kind string             `cbor:"kind"`
	Data *ClamavVerdictData `cbor:"data,omitempty"`
}

// ClamavVerdictData is the adjacent `data` content for the payload-bearing
// ClamavVerdict variants. Exactly one field is set per kind — "infected" sets
// Signature, "error" sets Detail — and `omitempty` drops the other so the
// content matches the Rust variant struct (`Infected{signature}` /
// `Error{detail}`) byte-for-byte.
type ClamavVerdictData struct {
	Signature string `cbor:"signature,omitempty"`
	Detail    string `cbor:"detail,omitempty"`
}

// RspamdRuleContribution mirrors
// fauna-protocol::bridge_routing::RspamdRuleContribution (one fired rspamd
// rule + its milli-int contribution; ham rules are negative).
type RspamdRuleContribution struct {
	Rule       string `cbor:"rule"`
	ScoreMilli int32  `cbor:"score_milli"`
}

// RspamdScore mirrors fauna-protocol::bridge_routing::RspamdScore. All scores
// are milli-ints (no floats on the dag-cbor wire — serialization.md strict
// decode rejects them).
type RspamdScore struct {
	RawMilli     int32                    `cbor:"raw_milli"`
	ScaledMilli  int32                    `cbor:"scaled_milli"`
	FlaggedRules []string                 `cbor:"flagged_rules"`
	Breakdown    []RspamdRuleContribution `cbor:"breakdown"`
}

// ScoreEntry mirrors fauna-core::scoring::ScoreEntry — one factor's row on
// the uniform scoring-metadata bus (content-scoring.md § The scoring-metadata
// bus). Score is an integer per-mille milli-int (no floats on the dag-cbor
// wire); Tier is the model-authority tier 1 (user) / 2 (admin) / 3
// (community); ScorerVersion is the re-score watermark.
type ScoreEntry struct {
	Factor        string `cbor:"factor"`
	Score         int64  `cbor:"score"`
	Tier          uint8  `cbor:"tier"`
	ScorerVersion uint32 `cbor:"scorer_version"`
}

// Factor names + authority tiers mirror the `fauna_core::scoring::factor::*` /
// tier constants (libs/fauna-core/src/scoring.rs) — the canonical bus
// vocabulary; only the factors the bridge itself scores/re-scores are
// mirrored here.
const (
	FactorClamav = "clamav"
	FactorRspamd = "rspamd"

	// LabelerFactorPrefix is the reserved namespace for community-labeler bus
	// factors (`fauna_core::scoring::labeler_factor` = "labeler:" + hex(id)).
	// The re-score drain routes a unit whose Factor carries this prefix to the
	// WASM `label()` path (Slice 3b) instead of the built-in scanner switch.
	LabelerFactorPrefix = "labeler:"

	// TierAdmin is the admin-authority model tier (fauna_core TIER_ADMIN) —
	// the tier of the deployment-wide clamav/rspamd scanners.
	TierAdmin uint8 = 2
	// TierCommunity is the opt-in community model tier (fauna_core
	// TIER_COMMUNITY) — the tier a subscribed labeler's `label()` output is
	// stamped at.
	TierCommunity uint8 = 3
)

// IngestInboundMailParams collects every field
// `fauna.bridges.ingest_inbound_mail` (and its alias
// `submit_inbound_mail`) take on the wire.
type IngestInboundMailParams struct {
	ActorID            []byte
	EncryptedBody      []byte
	EncryptedIndexHint []byte
	PublicMetadata     PublicMailMetadata
	Verdicts           AuthVerdicts
	SpamScore          uint32
	// SpamDisposition is the canonical snake_case string token per
	// bridge_routing.rs:SpamDisposition: "accept" |
	// "accept_to_spam_folder" | "policy_junk".
	SpamDisposition string
	// ClamavVerdict + RspamdScore are the T1.4 perimeter content-scan
	// verdict (mail-content-scanning.md). The scan runs Go-side on the
	// plaintext `raw` at the MTA (mta/scan_gate.go); only this verdict
	// metadata crosses to the nest, which stores the message_scan_results
	// row. ClamavVerdict is {Kind:"not_scanned"} when ClamAV did not run
	// (scanner disabled, or a door that never invokes the gate — the
	// submission twin and the Sent copy); a zero value normalizes to it, never
	// to "clean". RspamdScore is nil when rspamd is disabled.
	ClamavVerdict ClamavVerdict
	RspamdScore   *RspamdScore
	// SenderAddress is the full envelope MAIL FROM. The nest recomputes the
	// guardian mail-gate verdict from it at ingest — it never trusts a
	// placement flag from the bridge — and may place the message in the
	// recipient's held mailbox (`family-safety.md` § The mail gate). Empty for
	// the sender's own Sent copy and for the SMTP null reverse-path.
	SenderAddress string
	// DsnOriginalMsgID carries mailfauna.DsnCorrelation's OriginalMsgID when
	// (and only when) SenderAddress is empty on the external inbound path: the
	// Message-ID of the message the report bounces, read from its
	// message/rfc822 / text/rfc822-headers part. This is what the nest's
	// guardian mail gate authorizes on — it delivers a null-path message to a
	// supervised recipient only when this id matches one the ward itself sent
	// (within the nest-side consumption budget; thread References: leak ids,
	// so a match is spent, never durable), and holds every other
	// (`family-safety.md` § The mail gate). Leave empty when not a DSN or
	// when the report returned no message.
	DsnOriginalMsgID string
	// DsnReportAddresses carries the same extraction's ReplyAddresses: the
	// report's own top-level address-header set (From/Sender/Reply-To/To/Cc)
	// — what a one-click reply to the report can be addressed to. On a
	// correlated delivery the nest records these and declines to auto-seed
	// the ward's allowlist for them; a correlated report with an empty or
	// implausibly large set is held (`family-safety.md` § The mail gate).
	// Leave empty when not a DSN.
	DsnReportAddresses []string
	// IsRoleAddress echoes the bit ValidateRecipient returned for this
	// recipient. When true the nest ingest handler skips the per-mailbox
	// quota pre-check (role-address bypass, smtp-server.md :204). Always
	// false for the sender's own Sent copy (own-submission is exempt
	// regardless — submission is not a quota enforcement point).
	IsRoleAddress bool
	// TargetMailbox is the T3.3 filter-rule placement override (smtp-server.md
	// § Email filter rules). When non-nil the recipient's matched filter
	// resolved a `FileInto { mailbox }` folder (or `Allow` → "INBOX"); nest
	// files the message there, overriding the spam-disposition → folder map,
	// while leaving the recorded SpamDisposition truthful. A custom folder is
	// auto-created. nil → place by SpamDisposition. Never set on the Sent copy.
	TargetMailbox *string
	// ExtraFlags is the T3.3 filter-rule `AddLabel { label }` keywords to set
	// as IMAP flags on the placed message. Empty for an unmatched / non-label
	// action.
	ExtraFlags []string
	// Scores is the uniform scoring-metadata bus array — the perimeter's own
	// rows for this recipient, minted by the ONE shared-Rust mapping
	// (mailfauna.PerimeterMailScoreRows →
	// fauna_core::scoring::perimeter_mail_score_rows) from the same verdicts
	// the per-kind fields above carry. The nest stores it as-is and derives
	// nothing from the per-kind fields (content-scoring.md § The
	// scoring-metadata bus, the contract phase); those fields are the detail
	// record beside the rows. Empty = the perimeter scored nothing (the
	// sender's own Sent copy) → no rows recorded.
	Scores []ScoreEntry
	// ReportHash is the canonical 32-byte report-hash computed pre-seal over
	// the parsed subject + body text via the shared-Rust
	// mailfauna.ReportHash (report-sharing.md § Content identity) —
	// identical for every recipient of the same message on every nest. Empty
	// ⇒ omitted (the message cannot aggregate; the nest treats absent as
	// graceful, e.g. when the hash cannot be computed).
	ReportHash []byte
	// DedupKey is the canonical dedup key computed at this same pre-seal
	// position via the shared-Rust mailfauna.MailDedupKeys
	// (mailbox-migration.md § Key format). The nest records it in
	// actor_message_dedup so a later import of this account from a foreign
	// IMAP server dedup-hits the copy delivered here; it never suppresses the
	// delivery. Required by the nest: every delivery sets it, and an empty key
	// is refused.
	DedupKey string
	// EnvelopeKey is the canonical-envelope key from the same shared call,
	// recorded beside DedupKey: the Message-ID is the sender's choice, so a
	// later import skips on a DedupKey hit only when the envelope keys agree
	// (mailbox-migration.md § The envelope key confirms a Message-ID hit).
	// Required by the nest: every delivery sets it.
	EnvelopeKey string
	// BodyRef carries the sealed body on the bulk-byte plane instead of inline,
	// for a message the 2 MiB WS-RPC frame cannot hold (smtp-server.md
	// § Message size limits). Set it *xor* EncryptedBody — the nest rejects an
	// ingest that is empty on both, and treats a non-empty EncryptedBody as
	// authoritative. nil ⇒ omitted, the byte-identical pre-field shape.
	BodyRef *MailBodyRef
}

// MailBodyRef is the reference a sealed mail body rides when it is too large
// for the RPC frame: the ordered blake3 hashes of its staged chunks, plus the
// length the rejoined body must have.
//
// Mirrors Rust `fauna_protocol::bridge_routing::MailBodyRef`. Deliberately NOT
// a ChunkManifest — a mail reference is just the ordered hash list, which keeps
// this path off that type's fail-closed decode (the folder path owns it).
//
// TotalBytes is not redundant: the chunk *contents* are self-verifying (they are
// fetched by their own hash from a content-addressed store) but the chunk *list*
// is not, so a producer that named the wrong chunks, named them out of order, or
// dropped one would otherwise hand the consumer a body that unseals to the wrong
// message. The declared total is the cheap end-to-end check that catches all
// three, and it fails closed on both sides.
type MailBodyRef struct {
	ChunkHashes [][]byte `cbor:"chunk_hashes"`
	TotalBytes  uint64   `cbor:"total_bytes"`
}

// StagedBodyRef mirrors Rust
// `fauna_protocol::bridge_routing::StagedBodyRef` — a PLAINTEXT-derived mail
// body that crossed on the bulk-byte plane under a one-shot AEAD envelope (the
// staged-envelope rule, smtp-server.md § Message size limits). The outbound
// queue pair (this bridge's submission enqueue leg, and the fetch leg the
// outbound worker drains) uses it where MailBodyRef cannot: the open
// chunk-download route is safe only because everything in the store is
// ciphertext, so a plaintext body must be AEAD-sealed under a fresh one-shot key
// FIRST, the CIPHERTEXT split with the same body_ref chunk rule, and the key
// sent alongside — inside the already-confidential WS-RPC that would otherwise
// carry the body inline. A deliberately DISTINCT type from MailBodyRef so a
// sealed-bytes reference and an ephemeral-key ciphertext reference can never be
// confused at the type level.
//
// Key is a plain []byte (CBOR byte string, major type 2) matching the Rust
// `SecretByteBuf` — NOT `SecretBytes`, which would encode as a CBOR array of
// integers. TotalBytes is the rejoined SEALED (nonce-prefixed ciphertext)
// length, not the recovered plaintext length; the consumer pins the join against
// it and fails closed before it ever opens the AEAD (whose tag then
// authenticates the entire join).
type StagedBodyRef struct {
	ChunkHashes [][]byte `cbor:"chunk_hashes"`
	TotalBytes  uint64   `cbor:"total_bytes"`
	Key         []byte   `cbor:"key"`
}

// ingestInboundMailRequest mirrors IngestInboundMailRequest /
// SubmitInboundMailRequest (the two are wire-equivalent on the
// request side; only the routing kind differs).
type ingestInboundMailRequest struct {
	ActorID            []byte             `cbor:"actor_id"`
	EncryptedBody      []byte             `cbor:"encrypted_body"`
	EncryptedIndexHint []byte             `cbor:"encrypted_index_hint"`
	PublicMetadata     PublicMailMetadata `cbor:"public_metadata"`
	Verdicts           AuthVerdicts       `cbor:"verdicts"`
	SenderAddress      string             `cbor:"sender_address,omitempty"`
	// dsn_original_msgid: set only when the envelope sender is empty (the null
	// reverse-path) AND the raw message is a genuine RFC 3464 report
	// (mailfauna.DsnCorrelation). The reported original MESSAGE-ID is the
	// correlation the nest authorizes on — unguessable to a party the ward
	// never mailed. (The bounced ADDRESS, a public hint, left the wire with
	// the compat-remnant sweep.)
	DsnOriginalMsgID string `cbor:"dsn_original_msgid,omitempty"`
	// dsn_report_addresses: the report's own address-header set, same
	// null-path-only condition as the two fields above. Rust
	// `Vec<String>` `#[serde(default)]` — omitted when empty
	// (`omitempty`), and Rust reads the absent key as an empty set.
	DsnReportAddresses []string `cbor:"dsn_report_addresses,omitempty"`
	SpamScore          uint32   `cbor:"spam_score"`
	SpamDisposition    string   `cbor:"spam_disposition"`
	// clamav_verdict is always sent ({"kind":"not_scanned"} when ClamAV did
	// not run); the Rust field is `#[serde(default)]` and its default is the
	// same NotScanned. rspamd_score `omitempty` ⇒ omitted (Rust None) when nil.
	ClamavVerdict ClamavVerdict `cbor:"clamav_verdict"`
	RspamdScore   *RspamdScore  `cbor:"rspamd_score,omitempty"`
	// is_role_address omitted (Rust `#[serde(default)]` → false) for normal
	// deliveries; sent true only on a role-address inbound so nest skips the
	// quota pre-check.
	IsRoleAddress bool `cbor:"is_role_address,omitempty"`
	// target_mailbox / extra_flags are the T3.3 filter-rule placement override
	// (Rust `Option<String>` / `Vec<String>`, both `#[serde(default,
	// skip_serializing_if=...)]`). omitempty ⇒ omitted (Rust None / empty vec)
	// when the recipient's filters didn't resolve a FileInto/AddLabel.
	TargetMailbox *string  `cbor:"target_mailbox,omitempty"`
	ExtraFlags    []string `cbor:"extra_flags,omitempty"`
	// scores is the uniform scoring-metadata bus (Rust `Vec<ScoreEntry>`,
	// `#[serde(default, skip_serializing_if = "Vec::is_empty")]`). omitempty ⇒
	// omitted when the perimeter supplies no explicit rows (the nest derives
	// them from the per-kind fields above).
	Scores []ScoreEntry `cbor:"scores,omitempty"`
	// report_hash (Rust `serde_bytes` `Vec<u8>`, `#[serde(default,
	// skip_serializing_if = "Vec::is_empty")]`). omitempty ⇒ omitted when the
	// perimeter computed no hash; non-empty must be exactly 32 bytes (nest
	// rejects other lengths as malformed).
	ReportHash []byte `cbor:"report_hash,omitempty"`
	// dedup_key and envelope_key (Rust `String`, required): the pair the
	// shared-Rust mailfauna.MailDedupKeys returns. The nest refuses a request
	// missing either, or carrying an empty one.
	DedupKey    string `cbor:"dedup_key"`
	EnvelopeKey string `cbor:"envelope_key"`
	// body_ref (Rust `Option<MailBodyRef>`, `#[serde(default,
	// skip_serializing_if = "Option::is_none")]`). omitempty ⇒ omitted when nil,
	// so every message that fits inline still encodes byte-identically to the
	// pre-field shape and an older nest decodes it unchanged. Set iff
	// encrypted_body is empty.
	BodyRef *MailBodyRef `cbor:"body_ref,omitempty"`
}

// ingestInboundMailReply mirrors IngestInboundMailReply. The Rust
// side uses `#[serde(with = "serde_bytes")]` on `message_id` so the
// 32-byte deterministic routing pointer rides as a CBOR byte string.
type ingestInboundMailReply struct {
	MessageID []byte `cbor:"message_id"`
}

func ingestOrSubmit(ctx context.Context, c Caller, method string, p IngestInboundMailParams) ([]byte, error) {
	// clamav_verdict is adjacently-tagged on the wire ({"kind": ...}); the
	// nest's strict decode rejects an empty/unknown tag (→ malformed "input
	// is not valid CBOR"). The contract is that the field is ALWAYS present
	// and well-formed. The external-MX ingest path fills it from the
	// perimeter scan and the submission / Sent-copy path (fauna_recipient.go)
	// says NotScanned explicitly; a caller that still ships the zero value is
	// normalized here, the one chokepoint both paths flow through, to the
	// same "not_scanned" — never to "clean", which the nest would record as a
	// scan that happened.
	clamav := p.ClamavVerdict
	if clamav.Kind == "" {
		clamav = ClamavVerdict{Kind: "not_scanned"}
	}
	req := ingestInboundMailRequest{
		ActorID:            p.ActorID,
		EncryptedBody:      p.EncryptedBody,
		EncryptedIndexHint: p.EncryptedIndexHint,
		PublicMetadata:     p.PublicMetadata,
		Verdicts:           p.Verdicts,
		SenderAddress:      p.SenderAddress,
		DsnOriginalMsgID:   p.DsnOriginalMsgID,
		DsnReportAddresses: p.DsnReportAddresses,
		SpamScore:          p.SpamScore,
		SpamDisposition:    p.SpamDisposition,
		ClamavVerdict:      clamav,
		RspamdScore:        p.RspamdScore,
		IsRoleAddress:      p.IsRoleAddress,
		TargetMailbox:      p.TargetMailbox,
		ExtraFlags:         p.ExtraFlags,
		Scores:             p.Scores,
		ReportHash:         p.ReportHash,
		DedupKey:           p.DedupKey,
		EnvelopeKey:        p.EnvelopeKey,
		BodyRef:            p.BodyRef,
	}
	var reply ingestInboundMailReply
	if err := c.Call(ctx, method, req, &reply); err != nil {
		return nil, fmt.Errorf("%s: %w", method, err)
	}
	return reply.MessageID, nil
}

// IngestInboundMail submits an MX-ingested message to nest. Per the
// inbound flow (spec § Inbound mail flow steps 8-9).
func IngestInboundMail(ctx context.Context, c Caller, p IngestInboundMailParams) ([]byte, error) {
	return ingestOrSubmit(ctx, c, MethodIngestInboundMail, p)
}

// SubmitInboundMailParams is an alias for IngestInboundMailParams —
// the wire shapes are identical; only the routing kind differs (see
// the bridge_routing.rs comment on `is_own_submission` set
// server-side). Kept as a type alias so call sites can name the
// intent.
type SubmitInboundMailParams = IngestInboundMailParams

// SubmitInboundMail submits an authenticated-user message to nest's
// "submission" pipeline (lands in Sent + relays outward).
func SubmitInboundMail(ctx context.Context, c Caller, p SubmitInboundMailParams) ([]byte, error) {
	return ingestOrSubmit(ctx, c, MethodSubmitInboundMail, p)
}

// ── report_rejected_scan (T1.4) ──────────────────────────────────

// reportRejectedScanRequest mirrors
// fauna-protocol::bridge_routing::ReportRejectedScanRequest.
type reportRejectedScanRequest struct {
	ClamavSignature string       `cbor:"clamav_signature"`
	RspamdScore     *RspamdScore `cbor:"rspamd_score,omitempty"`
	ReceivedAt      int64        `cbor:"received_at"`
	SenderDomain    string       `cbor:"sender_domain"`
}

// reportRejectedScanReply mirrors ReportRejectedScanReply — the synthetic
// 32-byte forensic-row id the nest derived.
type reportRejectedScanReply struct {
	MessageID []byte `cbor:"message_id"`
}

// ReportRejectedScan records the forensic message_scan_results row for a
// reject-at-perimeter ClamAV hit. The message 554'd at SMTP DATA and was
// never stored (no ingest call), so the nest derives a synthetic deterministic
// message_id and inserts the row with action_taken='rejected_malware',
// delivered_to_actor=NULL. Metadata only — never the rejected bytes
// (content-scoring.md § The scoring-metadata bus). Returns the synthetic id.
func ReportRejectedScan(
	ctx context.Context,
	c Caller,
	clamavSignature string,
	rspamdScore *RspamdScore,
	receivedAt int64,
	senderDomain string,
) ([]byte, error) {
	req := reportRejectedScanRequest{
		ClamavSignature: clamavSignature,
		RspamdScore:     rspamdScore,
		ReceivedAt:      receivedAt,
		SenderDomain:    senderDomain,
	}
	var reply reportRejectedScanReply
	if err := c.Call(ctx, MethodReportRejectedScan, req, &reply); err != nil {
		return nil, fmt.Errorf("report_rejected_scan: %w", err)
	}
	return reply.MessageID, nil
}

// ── fetch_recipient_mls_pubkey ───────────────────────────────────

// fetchRecipientMLSPubkeyRequest mirrors
// FetchRecipientMlsPubkeyRequest. MailNewIngest must be true ONLY for the
// genuine per-delivery mail-new-ingest resolution (the MTA) — every other
// caller (IMAP/DAV session pubkey caching) leaves it false, so the nest's
// content-sealing-epochs write gate can never seal their non-mail bytes
// under a mail epoch key none of their openers understand.
type fetchRecipientMLSPubkeyRequest struct {
	ActorID       []byte `cbor:"actor_id"`
	MailNewIngest bool   `cbor:"mail_new_ingest"`
}

// Sizes of the two public halves of a recipient's seal key: the X25519 pubkey
// and the ML-KEM-768 encapsulation key (fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN).
const (
	recipientMLSPubkeyLen = 32
	recipientMlkemEkLen   = 1184
)

// recipientSealKeyHalves mirrors RecipientSealKeyHalves: both public halves of
// a recipient's seal key — the 32-byte X25519 pubkey and the 1184-byte
// ML-KEM-768 encapsulation key. They travel as one value because the provision
// request requires both and the nest stores them in one row.
// FetchRecipientMLSPubkeyHybrid refuses a value missing either.
type recipientSealKeyHalves struct {
	MLSPubkey []byte `cbor:"mls_pubkey"`
	MlkemEk   []byte `cbor:"mlkem_ek"`
}

// fetchRecipientMLSPubkeyReply mirrors FetchRecipientMlsPubkeyReply
// (Option<RecipientSealKeyHalves> on the Rust side → a pointer on the Go side).
//
// SuccessionPending mirrors the Rust `succession_pending` field (additive,
// defaults false) — meaningful only when Key is nil; see
// FetchRecipientMLSPubkeyHybrid.
type fetchRecipientMLSPubkeyReply struct {
	Key               *recipientSealKeyHalves `cbor:"key"`
	SuccessionPending bool                    `cbor:"succession_pending"`
}

// FetchRecipientMLSPubkey returns the recipient's 32-byte X25519
// MLS pubkey for envelope encryption. nil = recipient has no MLS
// pubkey provisioned (caller decides bounce vs. spool vs. fallback).
// Not a mail-new-ingest resolution — see FetchRecipientMLSPubkeyHybrid.
func FetchRecipientMLSPubkey(ctx context.Context, c Caller, actorID []byte) ([]byte, error) {
	pubkey, _, _, err := FetchRecipientMLSPubkeyHybrid(ctx, c, actorID, false)
	return pubkey, err
}

// FetchRecipientMLSPubkeyHybrid returns BOTH the recipient's 32-byte X25519 MLS
// pubkey AND their published ML-KEM-768 encapsulation key (mlkemEk, 1184 B).
// A nil X25519 pubkey means no key provisioned at all (caller bounces or
// tempfails — see successionPending), and mlkemEk is nil with it; a recipient
// with a key on file has both halves — the reply carries them as one value.
// The body-seal sites pass pubkey/mlkemEk to
// mailfauna.EncryptToRecipientHybrid, which seals X-Wing to them (no
// capability token; `architecture/security/post-quantum.md` § Capability
// negotiation).
//
// This is the ONE door a recipient's seal key enters the bridge by, and it
// refuses a key that is present but lacks a well-formed half (a 32-byte
// pubkey, a 1184-byte ek): an error, never a recipient to seal classically.
// The nest requires both halves at rest, so no nest sends such a reply — and
// a misbehaving one must not be able to select a classical body seal by
// omitting the ek. Callers treat it as any fetch failure (the MTA tempfails,
// an IMAP/DAV sign-in fails); a non-nil pubkey therefore always comes with a
// full-length ek.
//
// successionPending is meaningful ONLY when pubkey is nil: true means this
// actor is a succession's successor (succession-aftermath.md § Re-key scope)
// who has not yet re-provisioned a key — the caller should tempfail (451),
// not bounce (550), per smtp-server.md § Error / tempfail strategy.
//
// mailNewIngest must be true ONLY for the genuine per-delivery mail resolution
// (the MTA, via ResolveRecipientSealKeys) — false for every other caller
// (IMAP/DAV session pubkey caching), so the content-sealing-epochs write gate
// never seals their non-mail bytes under a mail epoch key.
func FetchRecipientMLSPubkeyHybrid(ctx context.Context, c Caller, actorID []byte, mailNewIngest bool) (pubkey, mlkemEk []byte, successionPending bool, err error) {
	req := fetchRecipientMLSPubkeyRequest{ActorID: actorID, MailNewIngest: mailNewIngest}
	var reply fetchRecipientMLSPubkeyReply
	if err := c.Call(ctx, MethodFetchRecipientMLSPubkey, req, &reply); err != nil {
		return nil, nil, false, fmt.Errorf("fetch_recipient_mls_pubkey: %w", err)
	}
	if reply.Key == nil {
		return nil, nil, reply.SuccessionPending, nil
	}
	if len(reply.Key.MLSPubkey) != recipientMLSPubkeyLen || len(reply.Key.MlkemEk) != recipientMlkemEkLen {
		return nil, nil, false, fmt.Errorf(
			"fetch_recipient_mls_pubkey: the nest sent a recipient seal key without both halves (mls_pubkey %d bytes, want %d; mlkem_ek %d bytes, want %d)",
			len(reply.Key.MLSPubkey), recipientMLSPubkeyLen, len(reply.Key.MlkemEk), recipientMlkemEkLen)
	}
	return reply.Key.MLSPubkey, reply.Key.MlkemEk, false, nil
}

// ── fetch_recipient_index_key ────────────────────────────────────

// fetchRecipientIndexKeyRequest mirrors FetchRecipientIndexKeyRequest.
type fetchRecipientIndexKeyRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchRecipientIndexKeyReply mirrors FetchRecipientIndexKeyReply
// (Option<ByteBuf> on the Rust side → *[]byte on the Go side).
type fetchRecipientIndexKeyReply struct {
	Pubkey *[]byte `cbor:"pubkey"`
}

// FetchRecipientIndexKey returns the recipient's 32-byte X25519 index
// pubkey for the encrypted-search-hint envelope (sibling of the MLS
// pubkey for body content). nil = recipient has not provisioned an
// index pubkey on this nest. Phase C.9 ships the wire surface; the
// production provisioning RPC is a Phase E concern. Until then this
// returns nil for every actor; the MTA bridge falls back to the
// recipient's MLS pubkey when index pubkey is unavailable (see
// `internal/mta/server.go::Session.Data` for the fallback path and
// the Phase E TODO).
func FetchRecipientIndexKey(ctx context.Context, c Caller, actorID []byte) ([]byte, error) {
	req := fetchRecipientIndexKeyRequest{ActorID: actorID}
	var reply fetchRecipientIndexKeyReply
	if err := c.Call(ctx, MethodFetchRecipientIndexKey, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_recipient_index_key: %w", err)
	}
	if reply.Pubkey == nil {
		return nil, nil
	}
	return *reply.Pubkey, nil
}

// RecipientSealKeys is the resolved seal-key set every ingest seal site
// consumes — the Go half of the Phase-3 D2 single-resolver seam (design
// `2026-07-07-phase-3-sealed-both-modes-design.md`; the nest half is
// `CacheDb::get_recipient_seal_key`, which serves this fetch). When
// content-sealing epochs land, the reply grows an epoch-indexed schedule
// and ONLY this resolver adapts — seal sites keep their shape
// (tracked internally).
type RecipientSealKeys struct {
	// MLSPubkey is the recipient's 32-byte X25519 MSEK-derived pubkey the
	// body seals to. nil = recipient not provisioned (caller bounces).
	MLSPubkey []byte
	// MlkemEk is the recipient's ML-KEM-768 encapsulation key (1184 B), set
	// whenever MLSPubkey is — the body seals X-Wing to the pair.
	MlkemEk []byte
	// IndexPubkey is the key the canonical-token index hint seals to. The
	// Phase-E fallback is applied HERE (index key unprovisioned → the MLS
	// pubkey), so seal sites never re-implement it.
	IndexPubkey []byte
	// SuccessionPending is meaningful ONLY when MLSPubkey is nil: true means
	// the recipient is a succession's successor who has not yet
	// re-provisioned a key, so the caller should tempfail rather than
	// bounce — see FetchRecipientMLSPubkeyHybrid.
	SuccessionPending bool
}

// ResolveRecipientSealKeys fetches the recipient's body-seal and index-seal
// keys in one call — THE seal-key resolution seam for every MTA/MDA ingest
// seal site (Phase-3 D2). A nil MLSPubkey with nil error means the recipient
// has provisioned no key (caller bounces or tempfails per
// RecipientSealKeys.SuccessionPending); transport errors return err. The
// Phase-E index-key fallback (nil index key → MLS pubkey) is applied here
// once; drop it when `provision_recipient_index_key` lands. Always a genuine
// mail-new-ingest resolution (every caller is MTA per-delivery sealing), so
// it opts into the content-sealing-epochs gate.
func ResolveRecipientSealKeys(ctx context.Context, c Caller, actorID []byte) (RecipientSealKeys, error) {
	pubkey, mlkemEk, successionPending, err := FetchRecipientMLSPubkeyHybrid(ctx, c, actorID, true)
	if err != nil {
		return RecipientSealKeys{}, err
	}
	if pubkey == nil {
		return RecipientSealKeys{SuccessionPending: successionPending}, nil
	}
	indexPubkey, err := FetchRecipientIndexKey(ctx, c, actorID)
	if err != nil {
		return RecipientSealKeys{}, err
	}
	if indexPubkey == nil {
		// Phase E gap: no production index-key provisioning yet — the
		// recipient's MLS pubkey doubles as the index pubkey so the
		// recipient's MDA opens both envelope kinds with the same shape.
		indexPubkey = pubkey
	}
	return RecipientSealKeys{
		MLSPubkey:   pubkey,
		MlkemEk:     mlkemEk,
		IndexPubkey: indexPubkey,
	}, nil
}

// ── fetch_wrapped_mls_blob ───────────────────────────────────────

// fetchWrappedMLSBlobRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:FetchWrappedMlsBlobRequest.
type fetchWrappedMLSBlobRequest struct {
	ActorID      []byte `cbor:"actor_id"`
	CredentialID string `cbor:"credential_id"`
}

// fetchWrappedMLSBlobReply mirrors FetchWrappedMlsBlobReply — Rust
// `blob: Option<ByteBuf>` ↔ Go `*[]byte` (nil = nest has no blob
// on file; non-nil empty would be a malformed wire shape).
type fetchWrappedMLSBlobReply struct {
	Blob *[]byte `cbor:"blob"`
}

// FetchWrappedMLSBlob returns the canonical-CBOR-encoded
// `WrappedMsekBlob` for (actorID, credentialID), or `(nil, nil)`
// when nest has no blob on file. The bridge then AEAD-unwraps via
// mailfauna.UnwrapMLSBlob with the MUA-supplied credential per
// `docs/goal/behavior/imap-server.md` § Authentication.
//
// Phase C.3 of the I5 mail-bridge MDA arm; allowlisted server-side
// for `BridgeMda` callers only (see
// `bins/fauna-nest/src/bridge_method_allowlist.rs`).
func FetchWrappedMLSBlob(ctx context.Context, c Caller, actorID []byte, credentialID string) ([]byte, error) {
	req := fetchWrappedMLSBlobRequest{ActorID: actorID, CredentialID: credentialID}
	var reply fetchWrappedMLSBlobReply
	if err := c.Call(ctx, MethodFetchWrappedMLSBlob, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_wrapped_mls_blob: %w", err)
	}
	if reply.Blob == nil {
		return nil, nil
	}
	return *reply.Blob, nil
}

// ── fetch_mls_snapshot_blob ──────────────────────────────────────

// fetchMLSSnapshotBlobRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:FetchMlsSnapshotBlobRequest.
type fetchMLSSnapshotBlobRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchMLSSnapshotBlobReply mirrors FetchMlsSnapshotBlobReply —
// Rust `blob: Option<ByteBuf>` ↔ Go `*[]byte` (nil = nest has no
// snapshot on file, e.g. user's primary client hasn't provisioned
// one yet; non-nil empty would be a malformed wire shape).
type fetchMLSSnapshotBlobReply struct {
	Blob *[]byte `cbor:"blob"`
}

// FetchMLSSnapshotBlob returns the canonical-CBOR `MlsSnapshotBlob`
// (encrypted under MSEK) for `actorID`, or `(nil, nil)` when nest
// has no snapshot on file.
//
// The MDA AUTH path calls this right after FetchRecipientIndexKey,
// then `cap.Decrypt(blob)` to recover the canonical-CBOR
// `MlsSnapshotPlaintext`. The plaintext is cached on the Session
// for body-decrypt (IMAP FETCH) + metadata-unseal (CalDAV
// PROPFIND/REPORT) — both call sites then hand
// `(envelope_bytes, cached_plaintext)` into
// `mailfauna.OpenMailRecord`.
//
// I5 Phase F (mail-record-open); allowlisted server-side for
// `BridgeMda` callers only (see
// `bins/fauna-nest/src/bridge_method_allowlist.rs`).
func FetchMLSSnapshotBlob(ctx context.Context, c Caller, actorID []byte) ([]byte, error) {
	req := fetchMLSSnapshotBlobRequest{ActorID: actorID}
	var reply fetchMLSSnapshotBlobReply
	if err := c.Call(ctx, MethodFetchMLSSnapshotBlob, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_mls_snapshot_blob: %w", err)
	}
	if reply.Blob == nil {
		return nil, nil
	}
	return *reply.Blob, nil
}

// ── report_session_close ─────────────────────────────────────────

// reportSessionCloseRequest mirrors ReportSessionCloseRequest.
type reportSessionCloseRequest struct {
	ActorID      []byte `cbor:"actor_id"`
	CredentialID string `cbor:"credential_id"`
	Reason       string `cbor:"reason"`
	OccurredAt   int64  `cbor:"occurred_at"`
}

// reportSessionCloseReply mirrors ReportSessionCloseReply.
type reportSessionCloseReply struct {
	OK bool `cbor:"ok"`
}

// ReportSessionClose tells nest the bridge closed a session
// (submission session end usually). `occurredAt` is bridge-local
// epoch milliseconds. Empty `reason` (or whitespace-only) is
// rejected server-side as malformed.
func ReportSessionClose(ctx context.Context, c Caller, actorID []byte, credentialID, reason string, occurredAt int64) error {
	req := reportSessionCloseRequest{
		ActorID:      actorID,
		CredentialID: credentialID,
		Reason:       reason,
		OccurredAt:   occurredAt,
	}
	var reply reportSessionCloseReply
	if err := c.Call(ctx, MethodReportSessionClose, req, &reply); err != nil {
		return fmt.Errorf("report_session_close: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("report_session_close: nest returned ok=false")
	}
	return nil
}

// ── list_mailboxes (Phase C MDA read surface) ────────────────────

// listMailboxesRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:ListMailboxesRequest.
// `SubscribedOnly` (Phase D.7) restricts the reply to mailboxes in
// `bridge_imap_subscriptions`; LSUB and `LIST (SUBSCRIBED)` set it,
// plain LIST leaves it false. The Rust side defaults the field via
// `#[serde(default)]` so legacy decoders survive.
type listMailboxesRequest struct {
	ActorID        []byte `cbor:"actor_id"`
	SubscribedOnly bool   `cbor:"subscribed_only"`
}

// MailboxEntry mirrors bridge_routing.rs:MailboxEntry. Special-use
// attributes (\Drafts, \Sent, …) and the path separator (`/`) are
// derived MDA-side from the canonical mailbox name, not carried on
// the wire — see internal/mda/imap/list.go.
type MailboxEntry struct {
	Name          string `cbor:"name"`
	UIDValidity   uint32 `cbor:"uid_validity"`
	UIDNext       uint32 `cbor:"uid_next"`
	HighestModseq int64  `cbor:"highestmodseq"`
	Exists        uint32 `cbor:"exists"`
	Unseen        uint32 `cbor:"unseen"`
}

// listMailboxesReply mirrors bridge_routing.rs:ListMailboxesReply.
type listMailboxesReply struct {
	Mailboxes []MailboxEntry `cbor:"mailboxes"`
}

// ListMailboxes returns every mailbox visible to the given actor.
// Mailboxes are ordered as nest returns them; the IMAP LIST translator
// applies pattern + reference filtering. When `subscribedOnly` is
// true the reply is filtered to mailboxes the actor has SUBSCRIBEd
// to (LSUB / `LIST (SUBSCRIBED)`). Allowlisted server-side for
// `BridgeMda` callers only.
func ListMailboxes(
	ctx context.Context,
	c Caller,
	actorID []byte,
	subscribedOnly bool,
) ([]MailboxEntry, error) {
	req := listMailboxesRequest{ActorID: actorID, SubscribedOnly: subscribedOnly}
	var reply listMailboxesReply
	if err := c.Call(ctx, MethodListMailboxes, req, &reply); err != nil {
		return nil, fmt.Errorf("list_mailboxes: %w", err)
	}
	return reply.Mailboxes, nil
}

// ── select_mailbox (Phase C MDA read surface) ────────────────────

// QResyncHint mirrors bridge_routing.rs:QResyncHint (RFC 7162 §3.2
// SELECT (QRESYNC ...) parameters 1+2). The IMAP MDA populates it from
// the fork's `(QRESYNC (uidvalidity modseq ...))` SELECT-param parser
// and forwards it so nest can run restore-divergence detection (γ)
// (imap-server.md § Restore divergence detection). Nil when the client
// did not supply QRESYNC SELECT parameters.
type QResyncHint struct {
	LastUIDValidity uint32 `cbor:"last_uid_validity"`
	LastModseq      int64  `cbor:"last_modseq"`
}

// selectMailboxRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:SelectMailboxRequest.
type selectMailboxRequest struct {
	ActorID       []byte       `cbor:"actor_id"`
	Mailbox       string       `cbor:"mailbox"`
	ClientQresync *QResyncHint `cbor:"client_qresync,omitempty"`
	MuaID         *string      `cbor:"mua_id,omitempty"`
}

// selectMailboxReply mirrors the Rust enum
// `SelectMailboxReply` with `#[serde(tag = "outcome",
// rename_all = "snake_case")]`. The discriminator `outcome` is
// "selected" or "no_such_mailbox"; on "selected" the payload fields
// land flat in the same map. We decode into a flat struct + pointer
// FirstUnseenUID (Option<u32> on the Rust side) and let the public
// SelectMailbox wrapper branch on Outcome.
type selectMailboxReply struct {
	Outcome        string  `cbor:"outcome"`
	UIDValidity    uint32  `cbor:"uid_validity,omitempty"`
	UIDNext        uint32  `cbor:"uid_next,omitempty"`
	HighestModseq  int64   `cbor:"highestmodseq,omitempty"`
	Exists         uint32  `cbor:"exists,omitempty"`
	Recent         uint32  `cbor:"recent,omitempty"`
	Unseen         uint32  `cbor:"unseen,omitempty"`
	FirstUnseenUID *uint32 `cbor:"first_unseen_uid,omitempty"`
}

// SelectedMailbox is the typed `selected` payload of select_mailbox.
// FirstUnseenUID is nil when every message has been seen (matches the
// Rust Option<u32>::None).
type SelectedMailbox struct {
	UIDValidity    uint32
	UIDNext        uint32
	HighestModseq  int64
	Exists         uint32
	Recent         uint32
	Unseen         uint32
	FirstUnseenUID *uint32
}

// ErrNoSuchMailbox is the sentinel returned by SelectMailbox when nest
// reports `outcome="no_such_mailbox"`. Callers test it via
// `errors.Is` to map a missing-mailbox to IMAP `NO Mailbox does not
// exist`, vs other errors that map to `NO [SERVERBUG]`.
var ErrNoSuchMailbox = errors.New("select_mailbox: no such mailbox")

// SelectMailbox resolves a mailbox to its current state for SELECT /
// EXAMINE. Returns `(*SelectedMailbox, nil)` on success;
// `(nil, ErrNoSuchMailbox)` when nest reports the mailbox does not
// exist; any other non-nil error is a transport / decode / unknown-
// outcome failure. `qr` carries the SELECT (QRESYNC ...) hint (RFC 7162
// §3.2.5) when the client supplied it; pass nil otherwise.
func SelectMailbox(ctx context.Context, c Caller, actorID []byte, mailbox string, qr *QResyncHint) (*SelectedMailbox, error) {
	req := selectMailboxRequest{ActorID: actorID, Mailbox: mailbox, ClientQresync: qr}
	var reply selectMailboxReply
	if err := c.Call(ctx, MethodSelectMailbox, req, &reply); err != nil {
		return nil, fmt.Errorf("select_mailbox: %w", err)
	}
	switch reply.Outcome {
	case "selected":
		return &SelectedMailbox{
			UIDValidity:    reply.UIDValidity,
			UIDNext:        reply.UIDNext,
			HighestModseq:  reply.HighestModseq,
			Exists:         reply.Exists,
			Recent:         reply.Recent,
			Unseen:         reply.Unseen,
			FirstUnseenUID: reply.FirstUnseenUID,
		}, nil
	case "no_such_mailbox":
		return nil, ErrNoSuchMailbox
	default:
		return nil, fmt.Errorf("select_mailbox: unknown outcome %q", reply.Outcome)
	}
}

// ── list_messages / fetch_message_metadata (Phase C MDA) ─────────

// MessageMeta mirrors bridge_routing.rs:MessageMeta. `internal_date`
// is epoch seconds (IMAP INTERNALDATE). `message_id` is the 32-byte
// server-assigned routing pointer. `flags` follows IMAP token
// convention (`\Seen`, `\Answered`, …, keywords).
type MessageMeta struct {
	UID            uint32   `cbor:"uid"`
	MessageID      []byte   `cbor:"message_id"`
	Modseq         int64    `cbor:"modseq"`
	Flags          []string `cbor:"flags"`
	InternalDate   int64    `cbor:"internal_date"`
	CiphertextSize uint32   `cbor:"ciphertext_size"`
	// SeqNum is the 1-based IMAP sequence number: the message's rank in the
	// mailbox's full ascending-UID order (RFC 9051 §6.4.5), computed nest-side.
	// Carried on every row (incl. a UID-subset fetch) so FETCH emits correct
	// seqNums without re-fetching the whole mailbox to number them (F1).
	SeqNum uint32 `cbor:"seq_num"`
}

// ListMessagesParams collects every field
// `fauna.bridges.list_messages` takes on the wire. SinceModseq /
// AfterUID are pointer-typed because the Rust shape uses
// `Option<i64>` / `Option<u32>` (None encodes as CBOR null).
type ListMessagesParams struct {
	ActorID     []byte
	Mailbox     string
	SinceModseq *int64  // nil → None; CONDSTORE incremental sync gate.
	Limit       uint32  // 0 = no limit.
	AfterUID    *uint32 // nil → None; pagination resume token.
}

// listMessagesRequest mirrors
// bridge_routing.rs:ListMessagesRequest. Pointer-typed Option fields
// match the Rust shape: a nil pointer encodes to CBOR null (Rust
// None), a non-nil pointer encodes the inner value (Rust Some).
type listMessagesRequest struct {
	ActorID     []byte  `cbor:"actor_id"`
	Mailbox     string  `cbor:"mailbox"`
	SinceModseq *int64  `cbor:"since_modseq"`
	Limit       uint32  `cbor:"limit"`
	AfterUID    *uint32 `cbor:"after_uid"`
}

// ListMessagesReply mirrors bridge_routing.rs:ListMessagesReply.
// `expunged_uids` is populated only when SinceModseq was non-nil.
// `more = true` means the caller should page with
// `after_uid = messages[len-1].uid`.
type ListMessagesReply struct {
	Messages      []MessageMeta `cbor:"messages"`
	ExpungedUIDs  []uint32      `cbor:"expunged_uids"`
	HighestModseq int64         `cbor:"highestmodseq"`
	More          bool          `cbor:"more"`
}

// ListMessages returns paginated message metadata for one mailbox.
// Pure read; no body data fetched. Allowlisted server-side for
// `BridgeMda` callers only.
func ListMessages(ctx context.Context, c Caller, p ListMessagesParams) (ListMessagesReply, error) {
	req := listMessagesRequest{
		ActorID:     p.ActorID,
		Mailbox:     p.Mailbox,
		SinceModseq: p.SinceModseq,
		Limit:       p.Limit,
		AfterUID:    p.AfterUID,
	}
	var reply ListMessagesReply
	if err := c.Call(ctx, MethodListMessages, req, &reply); err != nil {
		return ListMessagesReply{}, fmt.Errorf("list_messages: %w", err)
	}
	return reply, nil
}

// fetchMessageMetadataRequest mirrors
// bridge_routing.rs:FetchMessageMetadataRequest. Empty Uids ⇒ all
// messages in the mailbox (matches the Rust handler's "no UID
// filter" semantic).
type fetchMessageMetadataRequest struct {
	ActorID []byte   `cbor:"actor_id"`
	Mailbox string   `cbor:"mailbox"`
	Uids    []uint32 `cbor:"uids"`
}

// fetchMessageMetadataReply mirrors
// bridge_routing.rs:FetchMessageMetadataReply.
type fetchMessageMetadataReply struct {
	Messages []MessageMeta `cbor:"messages"`
	// MailboxTotal is the count of live messages in the mailbox (IMAP EXISTS),
	// independent of the `uids` filter — lets IDLE report EXISTS after a
	// single-UID metadata fetch instead of pulling the whole mailbox (F1).
	MailboxTotal uint32 `cbor:"mailbox_total"`
}

// FetchMessageMetadata returns per-UID metadata for an explicit UID
// list. Empty `uids` returns every message in the mailbox. Allow-
// listed server-side for `BridgeMda` callers only.
func FetchMessageMetadata(ctx context.Context, c Caller, actorID []byte, mailbox string, uids []uint32) ([]MessageMeta, error) {
	rows, _, err := FetchMessageMetadataWithTotal(ctx, c, actorID, mailbox, uids)
	return rows, err
}

// FetchMessageMetadataWithTotal is FetchMessageMetadata plus the mailbox's
// live-message total (IMAP EXISTS). IDLE uses it to resolve a single UID's
// sequence number AND the EXISTS count in one round-trip, instead of pulling
// the whole mailbox to derive both by position (F1).
func FetchMessageMetadataWithTotal(ctx context.Context, c Caller, actorID []byte, mailbox string, uids []uint32) ([]MessageMeta, uint32, error) {
	req := fetchMessageMetadataRequest{
		ActorID: actorID,
		Mailbox: mailbox,
		Uids:    uids,
	}
	// A nil slice CBOR-encodes as `null` (0xf6), which nest's strict
	// dag-cbor decode rejects for the non-Option `uids: Vec<u32>` field
	// (expects an array, 0x04). The wire contract is "empty array ⇒ all
	// messages", so normalize nil → empty array — same pattern as
	// `Append`'s `flags` normalization above.
	if req.Uids == nil {
		req.Uids = []uint32{}
	}
	var reply fetchMessageMetadataReply
	if err := c.Call(ctx, MethodFetchMessageMetadata, req, &reply); err != nil {
		return nil, 0, fmt.Errorf("fetch_message_metadata: %w", err)
	}
	return reply.Messages, reply.MailboxTotal, nil
}

// ── fetch_message_ciphertext (Phase C.6 MDA body fetch) ──────────

// fetchMessageCiphertextRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:FetchMessageCiphertextRequest.
type fetchMessageCiphertextRequest struct {
	ActorID   []byte `cbor:"actor_id"`
	MessageID []byte `cbor:"message_id"`
}

// fetchMessageCiphertextReply mirrors the Rust enum
// `FetchMessageCiphertextReply` with `#[serde(tag = "outcome",
// rename_all = "snake_case")]`. The discriminator is "found" or
// "not_found"; on "found" the payload rides flat in the same map.
type fetchMessageCiphertextReply struct {
	Outcome        string `cbor:"outcome"`
	EncryptedBody  []byte `cbor:"encrypted_body,omitempty"`
	CiphertextSize uint32 `cbor:"ciphertext_size,omitempty"`
	InternalDate   int64  `cbor:"internal_date,omitempty"`
	// body_ref is set (and encrypted_body empty) when the sealed body is too
	// large for the RPC frame and rides the bulk-byte plane instead
	// (smtp-server.md § Message size limits). Rust
	// `Option<MailBodyRef>`, `#[serde(default, skip_serializing_if =
	// "Option::is_none")]` ⇒ absent for every message that fits inline.
	BodyRef *MailBodyRef `cbor:"body_ref,omitempty"`
	// stored_at (unix seconds) is the instant THIS nest stored — and
	// therefore sealed — the record: the content-sealing-epochs
	// classification basis. Always sent on `found`; 0 = unknown (the nest's
	// append-time clock read failed). `omitempty` only because this one
	// struct mirrors both outcomes and `not_found` carries no fields.
	// Consumers use SealEpochBasisUnix, never this field directly.
	StoredAt int64 `cbor:"stored_at,omitempty"`
}

// FetchedCiphertext is the typed `found` payload of
// fetch_message_ciphertext. The bridge AEAD-decrypts the sealed body under the
// session's MLS capability to recover the RFC 5322 plaintext; CiphertextSize
// matches the public_metadata-floor visible in FETCH RFC822.SIZE; InternalDate
// is epoch seconds.
//
// EncryptedBody and BodyRef are exclusive: a body that fits the RPC frame
// arrives inline in the former, one that does not arrives by reference in the
// latter. Consumers must not read EncryptedBody directly — call SealedBody on
// the MDA's body resolver, which handles both.
type FetchedCiphertext struct {
	EncryptedBody  []byte
	CiphertextSize uint32
	InternalDate   int64
	BodyRef        *MailBodyRef
	// StoredAt is the unix-seconds instant the nest stored (= sealed) the
	// record; 0 when unknown. See SealEpochBasisUnix.
	StoredAt int64
}

// SealEpochBasisUnix returns the unix-seconds instant a record's sealing
// epoch classifies from (content-sealing-epochs design § 4): the nest-side
// append instant (StoredAt), never InternalDate — the seal keys off
// ingest-time now while InternalDate carries an *imported* message's own
// historical timestamp, so an epoch-aware reader classifying off InternalDate
// would try only epochs older than the seal's and go permanently dark on
// imported mail. 0 = unknown (the nest's append-time clock
// read failed): a standing-sealed record, which every epoch chain's standing
// arm opens.
func (ct *FetchedCiphertext) SealEpochBasisUnix() uint64 {
	if ct.StoredAt > 0 {
		return uint64(ct.StoredAt)
	}
	return 0
}

// ErrMessageNotFound is the sentinel returned by FetchMessageCiphertext
// when nest reports `outcome="not_found"` (UID resolved to a message_id
// that has since been expunged, or the actor doesn't own the message).
// Callers test it via `errors.Is` and skip the row instead of failing
// the whole FETCH — per RFC 9051 § 6.4.8, a missing UID in the result
// set is not an error.
var ErrMessageNotFound = errors.New("fetch_message_ciphertext: message not found")

// FetchMessageCiphertext returns the encrypted RFC 5322 body for one
// server-assigned message_id. Returns `(*FetchedCiphertext, nil)` on
// success; `(nil, ErrMessageNotFound)` when nest reports the message
// does not exist for this actor; any other non-nil error is a transport
// / decode / unknown-outcome failure. Allowlisted server-side for
// `BridgeMda` callers only.
func FetchMessageCiphertext(ctx context.Context, c Caller, actorID, messageID []byte) (*FetchedCiphertext, error) {
	req := fetchMessageCiphertextRequest{ActorID: actorID, MessageID: messageID}
	var reply fetchMessageCiphertextReply
	if err := c.Call(ctx, MethodFetchMessageCiphertext, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_message_ciphertext: %w", err)
	}
	switch reply.Outcome {
	case "found":
		return &FetchedCiphertext{
			EncryptedBody:  reply.EncryptedBody,
			CiphertextSize: reply.CiphertextSize,
			InternalDate:   reply.InternalDate,
			BodyRef:        reply.BodyRef,
			StoredAt:       reply.StoredAt,
		}, nil
	case "not_found":
		return nil, ErrMessageNotFound
	default:
		return nil, fmt.Errorf("fetch_message_ciphertext: unknown outcome %q", reply.Outcome)
	}
}

// ── fetch_index_segments_since (Phase C.7 — body-axis SEARCH source) ─

// IndexSegment mirrors
// libs/fauna-protocol/src/bridge_routing.rs:IndexSegment. The sealed
// `EncryptedIndexHint` is decrypted MDA-side under the session's
// MLS capability; the plaintext is the tokenized search hint the
// body-axis SEARCH path matches against.
type IndexSegment struct {
	MessageID          []byte `cbor:"message_id"`
	Mailbox            string `cbor:"mailbox"`
	Modseq             int64  `cbor:"modseq"`
	EncryptedIndexHint []byte `cbor:"encrypted_index_hint"`
	// StoredAt (unix seconds) is the instant the nest stored (= sealed) the
	// record — the content-sealing-epochs classification basis for the
	// sealed hint, which seals to the same epoch-gated recipient key as the
	// body in the same ingest transaction. Always sent; 0 = unknown (the
	// nest's append-time clock read failed) — a standing-sealed hint, which
	// the epoch chain's standing arm opens.
	StoredAt int64 `cbor:"stored_at"`
}

// fetchIndexSegmentsSinceRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:FetchIndexSegmentsSinceRequest.
// `Mailbox` is pointer-typed because the Rust shape uses
// `Option<String>` (None encodes as CBOR null → all mailboxes).
type fetchIndexSegmentsSinceRequest struct {
	ActorID     []byte  `cbor:"actor_id"`
	Mailbox     *string `cbor:"mailbox"`
	SinceModseq int64   `cbor:"since_modseq"`
	Limit       uint32  `cbor:"limit"`
}

// FetchIndexSegmentsSinceReply mirrors
// libs/fauna-protocol/src/bridge_routing.rs:FetchIndexSegmentsSinceReply.
// `Segments` is ascending by `Modseq`. `More` reports whether the
// caller should page with `since_modseq = segments[len-1].Modseq`.
type FetchIndexSegmentsSinceReply struct {
	Segments      []IndexSegment `cbor:"segments"`
	HighestModseq int64          `cbor:"highestmodseq"`
	More          bool           `cbor:"more"`
}

// FetchIndexSegmentsSinceParams collects every field
// `fauna.bridges.fetch_index_segments_since` takes on the wire.
// `Mailbox == nil` → no mailbox filter (matches `Option::None`).
type FetchIndexSegmentsSinceParams struct {
	ActorID     []byte
	Mailbox     *string
	SinceModseq int64
	Limit       uint32 // 0 = no limit.
}

// FetchIndexSegmentsSince returns the sealed index-hint segments for
// `actor_id` (optionally scoped to a single mailbox), ordered by
// ascending modseq. Used by the body-axis IMAP SEARCH path: the MDA
// decrypts each segment locally via the session's MLS capability,
// tokenizes the plaintext hint, and matches against the body terms.
// Allowlisted server-side for `BridgeMda` callers only.
func FetchIndexSegmentsSince(ctx context.Context, c Caller, p FetchIndexSegmentsSinceParams) (FetchIndexSegmentsSinceReply, error) {
	req := fetchIndexSegmentsSinceRequest{
		ActorID:     p.ActorID,
		Mailbox:     p.Mailbox,
		SinceModseq: p.SinceModseq,
		Limit:       p.Limit,
	}
	var reply FetchIndexSegmentsSinceReply
	if err := c.Call(ctx, MethodFetchIndexSegmentsSince, req, &reply); err != nil {
		return FetchIndexSegmentsSinceReply{}, fmt.Errorf("fetch_index_segments_since: %w", err)
	}
	return reply, nil
}

// ── search_messages (Phase C.7 — flag + header SEARCH axes) ──────

// HeaderField mirrors
// libs/fauna-protocol/src/bridge_routing.rs:HeaderField. The wire
// shape is `#[serde(rename_all = "snake_case")]`, so each variant
// encodes as a lowercase string token ("from" / "to" / ...).
type HeaderField string

const (
	HeaderFieldFrom    HeaderField = "from"
	HeaderFieldTo      HeaderField = "to"
	HeaderFieldCc      HeaderField = "cc"
	HeaderFieldSubject HeaderField = "subject"
)

// SearchTermKind mirrors the `kind` discriminator on the Rust
// `SearchTerm` enum (`#[serde(tag = "kind", rename_all = "snake_case")]`).
type SearchTermKind string

const (
	SearchTermKindHasFlag            SearchTermKind = "has_flag"
	SearchTermKindLacksFlag          SearchTermKind = "lacks_flag"
	SearchTermKindHeaderContains     SearchTermKind = "header_contains"
	SearchTermKindSinceInternalDate  SearchTermKind = "since_internal_date"
	SearchTermKindBeforeInternalDate SearchTermKind = "before_internal_date"
	SearchTermKindLarger             SearchTermKind = "larger"
	SearchTermKindSmaller            SearchTermKind = "smaller"
)

// SearchTerm mirrors the flat Rust `SearchTerm` enum encoding: a
// `kind` discriminator and the variant fields rolled into the same
// map (omitempty drops unused fields). Constructors below are the
// preferred call surface.
type SearchTerm struct {
	Kind  SearchTermKind `cbor:"kind"`
	Flag  string         `cbor:"flag,omitempty"`
	Field HeaderField    `cbor:"field,omitempty"`
	Value string         `cbor:"value,omitempty"`
	Ts    int64          `cbor:"ts,omitempty"`
	Size  uint32         `cbor:"size,omitempty"`
}

// NewSearchTermHasFlag builds a `\Seen` / keyword presence term.
func NewSearchTermHasFlag(flag string) SearchTerm {
	return SearchTerm{Kind: SearchTermKindHasFlag, Flag: flag}
}

// NewSearchTermLacksFlag builds the negation of HasFlag.
func NewSearchTermLacksFlag(flag string) SearchTerm {
	return SearchTerm{Kind: SearchTermKindLacksFlag, Flag: flag}
}

// NewSearchTermHeaderContains builds a case-folded substring match
// against one of the four `*_norm` columns. The handler lowercases
// `value` on the way in (it's already stored lowercased) so the
// match is case-insensitive.
func NewSearchTermHeaderContains(field HeaderField, value string) SearchTerm {
	return SearchTerm{Kind: SearchTermKindHeaderContains, Field: field, Value: value}
}

// NewSearchTermSinceInternalDate builds an inclusive `internal_date >= ts` predicate.
func NewSearchTermSinceInternalDate(ts int64) SearchTerm {
	return SearchTerm{Kind: SearchTermKindSinceInternalDate, Ts: ts}
}

// NewSearchTermBeforeInternalDate builds an exclusive `internal_date < ts` predicate.
func NewSearchTermBeforeInternalDate(ts int64) SearchTerm {
	return SearchTerm{Kind: SearchTermKindBeforeInternalDate, Ts: ts}
}

// NewSearchTermLarger builds a strict `ciphertext_size > size` predicate.
func NewSearchTermLarger(size uint32) SearchTerm {
	return SearchTerm{Kind: SearchTermKindLarger, Size: size}
}

// NewSearchTermSmaller builds a strict `ciphertext_size < size` predicate.
func NewSearchTermSmaller(size uint32) SearchTerm {
	return SearchTerm{Kind: SearchTermKindSmaller, Size: size}
}

// searchMessagesRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:SearchMessagesRequest.
type searchMessagesRequest struct {
	ActorID []byte       `cbor:"actor_id"`
	Mailbox string       `cbor:"mailbox"`
	Terms   []SearchTerm `cbor:"terms"`
}

// searchMessagesReply mirrors
// libs/fauna-protocol/src/bridge_routing.rs:SearchMessagesReply.
type searchMessagesReply struct {
	UIDs []uint32 `cbor:"uids"`
}

// SearchMessages runs the conjunctive IMAP SEARCH query against nest
// (flag, header, date, and size axes). Returns matching UIDs in
// ascending order. Body-axis SEARCH is performed locally on the MDA
// (see `FetchIndexSegmentsSince`) — this RPC handles only the
// nest-resident predicates. Allowlisted server-side for `BridgeMda`
// callers only.
func SearchMessages(ctx context.Context, c Caller, actorID []byte, mailbox string, terms []SearchTerm) ([]uint32, error) {
	// `UID SEARCH ALL` (and any all-body-axis search) reaches here with no
	// server-side terms (a nil `terms`). The codec's NilContainersAsEmpty
	// (internal/dagcbor/codec.go) marshals that nil slice as the canonical
	// empty list `[]` (0x80), never `null` (0xf6) — the Rust
	// `SearchMessagesRequest.terms: Vec<SearchTerm>` strict-decodes and
	// rejects `null` for a list (which surfaced as `UID SEARCH ALL` → NO,
	// `ok=false`). nest then applies "no filter" and returns every UID
	// (`search_bridge_imap_messages` builds no extra WHERE clause —
	// `search_empty_terms_returns_all_uids_ascending`). No per-method nil
	// coercion is needed: the codec flip closes this for every list field.
	req := searchMessagesRequest{
		ActorID: actorID,
		Mailbox: mailbox,
		Terms:   terms,
	}
	var reply searchMessagesReply
	if err := c.Call(ctx, MethodSearchMessages, req, &reply); err != nil {
		return nil, fmt.Errorf("search_messages: %w", err)
	}
	return reply.UIDs, nil
}

// ── get_quota ────────────────────────────────────────────────────

// getQuotaRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:GetQuotaRequest.
type getQuotaRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// GetQuotaReply mirrors
// libs/fauna-protocol/src/bridge_routing.rs:GetQuotaReply. Returned
// flat (not pointer-wrapped) — the RPC has no "no quota root" branch
// because every approved actor has the catalog default.
type GetQuotaReply struct {
	// StorageBytesUsed is the SUM of byte_length over the actor's
	// non-tombstoned mail placements (each COPY duplicate counts).
	StorageBytesUsed uint64 `cbor:"storage_bytes_used"`
	// MessageCountUsed is the COUNT of the same placements.
	MessageCountUsed uint32 `cbor:"message_count_used"`
	// StorageBytesLimit / MessageCountLimit echo the catalog
	// defaults (ImapPolicy.{StorageBytesDefault,MessageCountDefault})
	// in-band so the bridge serves QUOTA without a concurrent
	// fetch_config round-trip. Per-actor tier override is Phase F+.
	StorageBytesLimit uint64 `cbor:"storage_bytes_limit"`
	MessageCountLimit uint32 `cbor:"message_count_limit"`
}

// GetQuota fetches the per-actor QUOTA usage + limits. Allowlisted
// server-side for `BridgeMda` only. Phase C wires this for direct
// programmatic use (and Phase F+ when emersion/go-imap upstream
// lands the QUOTA server-side dispatch — see imap-server.md
// § Upstream-blocked gaps).
func GetQuota(ctx context.Context, c Caller, actorID []byte) (GetQuotaReply, error) {
	var reply GetQuotaReply
	if err := c.Call(ctx, MethodGetQuota, getQuotaRequest{ActorID: actorID}, &reply); err != nil {
		return GetQuotaReply{}, fmt.Errorf("get_quota: %w", err)
	}
	return reply, nil
}

// ── store_flags (I5 Phase D.2 MDA write surface) ─────────────────

// StoreFlagsOp mirrors libs/fauna-protocol/src/bridge_routing.rs:StoreFlagsOp.
// Encoded as the bare snake_case string on the wire.
type StoreFlagsOp string

const (
	// StoreFlagsOpSet replaces the entire flag set on each UID.
	StoreFlagsOpSet StoreFlagsOp = "set"
	// StoreFlagsOpAdd unions the supplied flags onto the existing set.
	StoreFlagsOpAdd StoreFlagsOp = "add"
	// StoreFlagsOpRemove subtracts the supplied flags from the existing set.
	StoreFlagsOpRemove StoreFlagsOp = "remove"
)

// StoreFlagsParams collects every field
// `fauna.bridges.store_flags` takes on the wire.
//
// UnchangedSince is pointer-typed because the Rust shape uses
// `Option<i64>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`).
// A nil pointer omits the field on the wire — the handler treats that
// as the legacy unconditional STORE. A non-nil pointer activates
// RFC 7162 §3.1.3 CONDSTORE `UNCHANGEDSINCE`: UIDs whose current
// modseq exceeds the supplied value are skipped and reported in
// `reply.Modified`.
type StoreFlagsParams struct {
	ActorID        []byte
	Mailbox        string
	UIDs           []uint32
	Op             StoreFlagsOp
	Flags          []string
	UnchangedSince *int64
}

// storeFlagsRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:StoreFlagsRequest. The
// `omitempty` on UnchangedSince mirrors the Rust serde-skip; the
// CBOR encoder drops the key entirely when the pointer is nil.
type storeFlagsRequest struct {
	ActorID        []byte       `cbor:"actor_id"`
	Mailbox        string       `cbor:"mailbox"`
	UIDs           []uint32     `cbor:"uids"`
	Op             StoreFlagsOp `cbor:"op"`
	Flags          []string     `cbor:"flags"`
	UnchangedSince *int64       `cbor:"unchanged_since,omitempty"`
}

// StoreFlagsResultEntry mirrors
// libs/fauna-protocol/src/bridge_routing.rs:StoreFlagsResultEntry.
// One entry per UID that had a placement row and was updated.
type StoreFlagsResultEntry struct {
	UID    uint32   `cbor:"uid"`
	Flags  []string `cbor:"flags"`
	ModSeq int64    `cbor:"modseq"`
}

// StoreFlagsReply mirrors
// libs/fauna-protocol/src/bridge_routing.rs:StoreFlagsReply.
//
// `Modified` is the RFC 7162 §3.1.3 MODIFIED partition — UIDs whose
// modseq exceeded the request's UnchangedSince and were therefore
// skipped. Empty when no precondition was supplied or every UID
// passed; matches the Rust `#[serde(default, skip_serializing_if)]`
// shape so a Phase-C-style reply is bit-identical on the wire.
type StoreFlagsReply struct {
	Updated       []StoreFlagsResultEntry `cbor:"updated"`
	HighestModSeq int64                   `cbor:"highestmodseq"`
	Modified      []uint32                `cbor:"modified,omitempty"`
}

// StoreFlags applies one IMAP STORE / UID STORE on the actor's
// per-mailbox placements. Allowlisted server-side for `BridgeMda`
// only. The wire response carries the per-UID new flag set, the
// shared new modseq, and (under CONDSTORE) any UIDs rejected for
// stale modseq.
func StoreFlags(ctx context.Context, c Caller, p StoreFlagsParams) (StoreFlagsReply, error) {
	req := storeFlagsRequest{
		ActorID:        p.ActorID,
		Mailbox:        p.Mailbox,
		UIDs:           p.UIDs,
		Op:             p.Op,
		Flags:          p.Flags,
		UnchangedSince: p.UnchangedSince,
	}
	var reply StoreFlagsReply
	if err := c.Call(ctx, MethodStoreFlags, req, &reply); err != nil {
		return StoreFlagsReply{}, fmt.Errorf("store_flags: %w", err)
	}
	return reply, nil
}

// ── copy / move (I5 Phase D.4) ───────────────────────────────────

// CopyPair mirrors libs/fauna-protocol/src/bridge_routing.rs:CopyPair —
// one (source_uid, dest_uid) entry returned by COPY or MOVE.
type CopyPair struct {
	SourceUID uint32 `cbor:"source_uid"`
	DestUID   uint32 `cbor:"dest_uid"`
}

// CopyParams collects every field `fauna.bridges.copy` takes.
type CopyParams struct {
	ActorID       []byte
	SourceMailbox string
	UIDs          []uint32
	DestMailbox   string
}

// copyMessagesRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:CopyMessagesRequest.
type copyMessagesRequest struct {
	ActorID       []byte   `cbor:"actor_id"`
	SourceMailbox string   `cbor:"source_mailbox"`
	UIDs          []uint32 `cbor:"uids"`
	DestMailbox   string   `cbor:"dest_mailbox"`
}

// CopyMessagesReply mirrors
// libs/fauna-protocol/src/bridge_routing.rs:CopyMessagesReply.
// `Copied` is in the source's request-order; entries for source UIDs
// without a placement row are simply absent.
type CopyMessagesReply struct {
	DestUIDValidity   uint32     `cbor:"dest_uid_validity"`
	Copied            []CopyPair `cbor:"copied"`
	DestHighestmodseq int64      `cbor:"dest_highestmodseq"`
}

// Copy performs one IMAP COPY / UID COPY on the actor's placements.
// Allowlisted server-side for `BridgeMda` only.
func Copy(ctx context.Context, c Caller, p CopyParams) (CopyMessagesReply, error) {
	req := copyMessagesRequest{
		ActorID:       p.ActorID,
		SourceMailbox: p.SourceMailbox,
		UIDs:          p.UIDs,
		DestMailbox:   p.DestMailbox,
	}
	var reply CopyMessagesReply
	if err := c.Call(ctx, MethodCopy, req, &reply); err != nil {
		return CopyMessagesReply{}, fmt.Errorf("copy: %w", err)
	}
	return reply, nil
}

// MoveParams collects every field `fauna.bridges.move` takes.
type MoveParams struct {
	ActorID       []byte
	SourceMailbox string
	UIDs          []uint32
	DestMailbox   string
}

// moveMessagesRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:MoveMessagesRequest.
type moveMessagesRequest struct {
	ActorID       []byte   `cbor:"actor_id"`
	SourceMailbox string   `cbor:"source_mailbox"`
	UIDs          []uint32 `cbor:"uids"`
	DestMailbox   string   `cbor:"dest_mailbox"`
}

// MoveMessagesReply mirrors
// libs/fauna-protocol/src/bridge_routing.rs:MoveMessagesReply.
type MoveMessagesReply struct {
	DestUIDValidity     uint32     `cbor:"dest_uid_validity"`
	Moved               []CopyPair `cbor:"moved"`
	SourceHighestmodseq int64      `cbor:"source_highestmodseq"`
	DestHighestmodseq   int64      `cbor:"dest_highestmodseq"`
}

// Move performs one IMAP MOVE / UID MOVE on the actor's placements
// (RFC 6851 / 9051 §6.6 — atomic copy-then-expunge). Allowlisted
// server-side for `BridgeMda` only.
func Move(ctx context.Context, c Caller, p MoveParams) (MoveMessagesReply, error) {
	req := moveMessagesRequest{
		ActorID:       p.ActorID,
		SourceMailbox: p.SourceMailbox,
		UIDs:          p.UIDs,
		DestMailbox:   p.DestMailbox,
	}
	var reply MoveMessagesReply
	if err := c.Call(ctx, MethodMove, req, &reply); err != nil {
		return MoveMessagesReply{}, fmt.Errorf("move: %w", err)
	}
	return reply, nil
}

// ── expunge / UID expunge (I5 Phase D.3) ─────────────────────────

// ExpungeParams collects every field `fauna.bridges.expunge` takes on
// the wire. Empty `UIDs` ⇒ plain EXPUNGE (nest filters \Deleted across
// the entire mailbox); non-empty `UIDs` ⇒ RFC 4315 UID EXPUNGE
// (nest intersects with \Deleted server-side).
type ExpungeParams struct {
	ActorID []byte
	Mailbox string
	UIDs    []uint32
}

// expungeRequest mirrors libs/fauna-protocol/src/bridge_routing.rs:ExpungeRequest.
type expungeRequest struct {
	ActorID []byte   `cbor:"actor_id"`
	Mailbox string   `cbor:"mailbox"`
	UIDs    []uint32 `cbor:"uids"`
}

// ExpungeReply mirrors libs/fauna-protocol/src/bridge_routing.rs:ExpungeReply.
// `ExpungedUIDs` is the ascending list of UIDs that nest actually removed
// (after the \Deleted intersection); `Highestmodseq` is the post-write
// mailbox-level modseq.
type ExpungeReply struct {
	ExpungedUIDs  []uint32 `cbor:"expunged_uids"`
	HighestModseq int64    `cbor:"highestmodseq"`
}

// Expunge performs one IMAP EXPUNGE / UID EXPUNGE on the actor's
// placements. Allowlisted server-side for `BridgeMda` only.
//
// Note: `* VANISHED <uid_set>` (QRESYNC) is upstream-blocked in
// `emersion/go-imap/v2 v2.0.0-beta.8` — see `imap-server.md` §
// Upstream-blocked gaps. Today the MDA always emits the per-UID
// `* <seq> EXPUNGE` fallback regardless of QRESYNC state.
func Expunge(ctx context.Context, c Caller, p ExpungeParams) (ExpungeReply, error) {
	req := expungeRequest{
		ActorID: p.ActorID,
		Mailbox: p.Mailbox,
		UIDs:    p.UIDs,
	}
	var reply ExpungeReply
	if err := c.Call(ctx, MethodExpunge, req, &reply); err != nil {
		return ExpungeReply{}, fmt.Errorf("expunge: %w", err)
	}
	return reply, nil
}

// ── append (I5 Phase D.5) ────────────────────────────────────────

// AppendParams collects every field `fauna.bridges.append` takes.
// Mirrors libs/fauna-protocol/src/bridge_routing.rs:AppendMessageRequest
// field-by-field; the seal-to-self ciphertexts are pre-computed by
// the MDA (mailfauna.EncryptToRecipient → actor's MLS pubkey and
// index pubkey) before this wrapper is called.
type AppendParams struct {
	ActorID            []byte
	Mailbox            string
	Flags              []string
	EncryptedBody      []byte
	EncryptedIndexHint []byte
	Timestamp          int64
	CiphertextSize     uint32
	SenderDomain       string
	// DedupKey is the canonical dedup key computed pre-seal from the plaintext
	// literal via the shared-Rust mailfauna.MailDedupKeys
	// (mailbox-migration.md § Key format). The nest records it in
	// actor_message_dedup so a later import dedup-hits mail filed by this MUA;
	// it never suppresses the APPEND. Required by the nest: every APPEND sets
	// it, and an empty key is refused.
	DedupKey string
	// EnvelopeKey is the canonical-envelope key from the same shared call,
	// recorded beside DedupKey so a later import skips on a hit only when the
	// envelope keys agree (mailbox-migration.md § The envelope key confirms a
	// Message-ID hit). Required by the nest: every APPEND sets it.
	EnvelopeKey string
	// BodyRef carries the sealed body on the bulk-byte plane instead of inline,
	// when it is too large for the 2 MiB WS-RPC frame (smtp-server.md § Message
	// size limits — the MDA-APPEND upward leg). Exactly one of EncryptedBody /
	// BodyRef carries the body. APPEND stages *already-sealed* bytes, so it rides
	// a plain MailBodyRef, never the plaintext staged-envelope. nil ⇒ the body
	// rode inline (the overwhelmingly common case).
	BodyRef *MailBodyRef
}

// appendRequest mirrors libs/fauna-protocol/src/bridge_routing.rs:AppendMessageRequest.
type appendRequest struct {
	ActorID            []byte   `cbor:"actor_id"`
	Mailbox            string   `cbor:"mailbox"`
	Flags              []string `cbor:"flags"`
	EncryptedBody      []byte   `cbor:"encrypted_body"`
	EncryptedIndexHint []byte   `cbor:"encrypted_index_hint"`
	Timestamp          int64    `cbor:"timestamp"`
	CiphertextSize     uint32   `cbor:"ciphertext_size"`
	SenderDomain       string   `cbor:"sender_domain"`
	// dedup_key and envelope_key (Rust `String`, required): the pair computed
	// pre-seal from the plaintext literal by the shared-Rust
	// mailfauna.MailDedupKeys — the nest only ever sees EncryptedBody here, so
	// it cannot derive them itself, and refuses a request missing either or
	// carrying an empty one.
	DedupKey    string `cbor:"dedup_key"`
	EnvelopeKey string `cbor:"envelope_key"`
	// body_ref (Rust `Option<MailBodyRef>`, `#[serde(default,
	// skip_serializing_if = "Option::is_none")]`). Set (and encrypted_body empty)
	// when the sealed body was over the inline budget and rode the byte plane.
	// omitempty ⇒ omitted when nil, the byte-identical pre-field shape an older
	// nest still decodes.
	BodyRef *MailBodyRef `cbor:"body_ref,omitempty"`
}

// AppendReply mirrors libs/fauna-protocol/src/bridge_routing.rs:AppendMessageReply.
// `UID` + `UIDValidity` flow back to emersion via `*imap.AppendData` so
// the server emits the UIDPLUS `OK [APPENDUID <validity> <uid>]` tagged
// response (RFC 4315). `MessageID` is the server-assigned 32-byte
// blake3 id of the appended ciphertext.
type AppendReply struct {
	MessageID   []byte `cbor:"message_id"`
	UID         uint32 `cbor:"uid"`
	UIDValidity uint32 `cbor:"uid_validity"`
}

// Append performs one IMAP APPEND on the actor's placements.  The MDA
// bridge encrypts the literal to the actor's own MLS pubkey + index
// pubkey before this call (seal-to-self).  Allowlisted server-side for
// `BridgeMda` only.  MULTIAPPEND (RFC 9051 §6.3.12) is one Append call
// per literal — per-RPC transaction discipline is preserved, partial-
// success returns the landed UIDs plus a final BAD/NO.
func Append(ctx context.Context, c Caller, p AppendParams) (AppendReply, error) {
	req := appendRequest{
		ActorID:            p.ActorID,
		Mailbox:            p.Mailbox,
		Flags:              p.Flags,
		EncryptedBody:      p.EncryptedBody,
		EncryptedIndexHint: p.EncryptedIndexHint,
		Timestamp:          p.Timestamp,
		CiphertextSize:     p.CiphertextSize,
		SenderDomain:       p.SenderDomain,
		DedupKey:           p.DedupKey,
		EnvelopeKey:        p.EnvelopeKey,
		BodyRef:            p.BodyRef,
	}
	if req.Flags == nil {
		req.Flags = []string{}
	}
	var reply AppendReply
	if err := c.Call(ctx, MethodAppend, req, &reply); err != nil {
		return AppendReply{}, fmt.Errorf("append: %w", err)
	}
	return reply, nil
}

// ── spam training labels (put_spam_model history rows) ──────────

// SpamLabel mirrors libs/fauna-protocol/src/bridge_routing.rs:SpamLabel.
// Encoded as the bare snake_case string ("spam" / "ham").
type SpamLabel string

const (
	SpamLabelSpam SpamLabel = "spam"
	SpamLabelHam  SpamLabel = "ham"
)

// TrainingSource mirrors libs/fauna-protocol/src/bridge_routing.rs:TrainingSource.
// Encoded as the bare snake_case string. The MDA's caller picks the
// source matching the IMAP-side trigger (per mail-spam.md § Training
// signal sources). New variants land server-first; the bridge sends
// `manual_other` for any non-IMAP-button signal.
type TrainingSource string

const (
	TrainingSourceImapJunkFlag TrainingSource = "imap_junk_flag"
	TrainingSourceImapJunkMove TrainingSource = "imap_junk_move"
	TrainingSourceManualOther  TrainingSource = "manual_other"
)

// fetchSpamModelRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:FetchSpamModelRequest. The
// nest field is `actor_id: Vec<u8>` carried `#[serde(with =
// "serde_bytes")]`, i.e. a CBOR byte string — the Go `ActorID []byte`,
// which the dagcbor encoder lowers to a byte string. The flatten/`extra` forward-compat map nest
// declares is empty here and omitted on the wire.
type fetchSpamModelRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchSpamModelReply mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:FetchSpamModelReply. `blob`
// is `Option<ByteBuf>` — a CBOR byte string (the `fauna_mls::wrapped_blob`
// shape, sealed to the actor's MSEK-derived key) when the actor has a
// stored model or the nest serves the cold-start seed, null/absent
// otherwise. Decodes into a nil slice in the absent case.
type fetchSpamModelReply struct {
	Blob []byte `cbor:"blob"`
	// StoredSealed mirrors the additive `stored_sealed: bool` — `true` iff
	// `blob` is the actor's stored model (always sealed at rest, returned
	// verbatim), `false` when `blob` is the cold-start seed the nest
	// sealed-on-read from the deployment baseline (or absent). The wire
	// blob is a sealed envelope either way, so this flag is the ONLY way to
	// tell the two apart: the `\Junk`-train path trains a `true` blob and
	// starts from an EMPTY model otherwise — never from the seed (the
	// baseline fold is read-time only, mail-spam.md § Cold start Path 2).
	StoredSealed bool `cbor:"stored_sealed"`
	// The published deployment baseline (plaintext aggregate), present
	// EXACTLY when the nest did not fold it into the served blob — i.e.
	// only for a stored model (`stored_sealed: true`). The SCORING agent
	// folds it locally (`mailfauna.FoldSpamModelBaseline` — the
	// no-double-fold rule, mail-spam.md § Encrypted-mode interaction); the
	// TRAIN path must ignore it (the fold is read-time-only, never
	// persisted). Absent (nil) for the cold-start seed, which already
	// carries the fold.
	Baseline []byte `cbor:"baseline"`
	// ContributeBaseline mirrors the additive `contribute_baseline: bool` —
	// `true` iff the actor opted into the deployment-baseline contribution
	// (`spam_preferences.contribute_baseline`). Paired with HolderSealTarget, it
	// tells the agent-side `\Junk`-train re-seal path (piece (b4)) to seal +
	// attach a fresh `SpamModelCopyBlob` to the box's aggregation holder on the
	// next put_spam_model (mail-spam.md § Encrypted-mode interaction). Scoring
	// ignores it. Absent from a pre-piece-(b) nest ⇒ false (attach no copy).
	ContributeBaseline bool `cbor:"contribute_baseline"`
	// HolderSealTarget mirrors the additive `holder_seal_target:
	// Option<HolderSealTarget>` — the box's volunteered content-processor holder
	// seal target. Present (non-nil) only when the actor opted in AND a holder is
	// enrolled; nil otherwise (opted-out, no holder, or a pre-piece-(b) nest that
	// omits the key). The train re-seal path seals the copy to this target.
	HolderSealTarget *HolderSealTarget `cbor:"holder_seal_target"`
}

// HolderSealTarget mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:HolderSealTarget — the box's
// volunteered aggregation-holder seal target. X25519Pubkey is a 32-byte CBOR
// byte string (required); MLKemEk is an optional CBOR byte string, present
// (non-nil) iff the holder published an ML-KEM ek — which selects the
// post-quantum X-Wing seal suite (else classical X25519). The same target keys
// both the `SpamModelCopyBlob` seal and the paired keyless grant's holder pubkey.
// Exported (not just an internal reply field) because the MDA `\Junk`-train
// re-seal path (piece b4, `store.go`) needs to name the type across packages.
type HolderSealTarget struct {
	X25519Pubkey []byte `cbor:"x25519_pubkey"`
	MLKemEk      []byte `cbor:"mlkem_ek"`
}

// FetchSpamModel fetches the AUTH'd actor's per-user Bayesian spam
// model, **sealed to the actor's MSEK-derived key** (the same
// wrapped-blob shape as a mail body / index hint). Allowlisted
// server-side for `BridgeMda` only (mail-spam.md § Wire shapes — the
// User/Fauna-app fetch is deferred behind an authorization gate). The
// caller unwraps the returned blob with the session MLS capability's
// `OpenMailRecord` — exactly as it opens a sealed body for SEARCH —
// then runs the shared scorer (`mailfauna.WeightedBayesianMilliForModel`)
// over the unwrapped model bytes.
//
// Returns a nil blob when the actor has no stored model and no deployment
// baseline is published ⇒ cold start (the scorer's confidence weight is 0
// below `bayesian_min_samples`, so a cold-start model contributes 0 to the
// combined score). The raw model never crosses the wire; nest holds
// only the public half and can never read the stored model.
// The second return is `stored_sealed`: `true` iff the returned blob is the
// actor's stored (sealed-at-rest) model verbatim — the agent-side train opens,
// mutates, re-seals and `put_spam_model`s it; `false` for the cold-start seed
// (the baseline folded onto a fresh model, sealed on read) — the train path
// starts from an empty model and never trains the seed.
// The third return is the published deployment baseline, non-nil only for a
// stored model (the cold-start seed already carries the fold):
// the SCORING caller folds it into the unwrapped model locally
// (`mailfauna.FoldSpamModelBaseline`); the TRAIN caller ignores it — the fold
// is read-time-only and must never be persisted into the user's model.
// The fourth/fifth returns are the piece-(b) contribute-baseline write signal:
// `contributeBaseline` true and a non-nil `holderSealTarget` together tell the
// agent-side `\Junk`-train re-seal path (b4) to seal + attach a fresh
// `SpamModelCopyBlob` to that holder on the next `PutSpamModel`; the SCORING
// caller ignores both.
func FetchSpamModel(ctx context.Context, c Caller, actorID []byte) (blob []byte, storedSealed bool, baseline []byte, contributeBaseline bool, holderSealTarget *HolderSealTarget, err error) {
	var reply fetchSpamModelReply
	if err := c.Call(ctx, MethodFetchSpamModel, fetchSpamModelRequest{ActorID: actorID}, &reply); err != nil {
		return nil, false, nil, false, nil, fmt.Errorf("fetch_spam_model: %w", err)
	}
	return reply.Blob, reply.StoredSealed, reply.Baseline, reply.ContributeBaseline, reply.HolderSealTarget, nil
}

// SpamHistoryInsert is the payload of the wire `SpamHistoryOp::Insert`
// variant (libs/fauna-protocol/src/bridge_routing.rs, externally tagged:
// `{"Insert": {...}}`) — one sealed training-history audit row committed
// ATOMICALLY with the model write on `put_spam_model`. SealedSubject and
// SealedDelta are opaque `wrapped_blob` bytes sealed to the ACTOR's own
// recipient key (the nest stores them verbatim; only the actor's client
// can unwrap them for display / client-side undo); MessageID, Mailbox,
// Label, Source are plaintext metadata, the same columns a server-written
// row carries in the clear.
type SpamHistoryInsert struct {
	MessageID     []byte         `cbor:"message_id"`
	Mailbox       string         `cbor:"mailbox"`
	SealedSubject []byte         `cbor:"sealed_subject"`
	SealedDelta   []byte         `cbor:"sealed_delta"`
	Label         SpamLabel      `cbor:"label"`
	Source        TrainingSource `cbor:"source"`
}

// spamHistoryOp mirrors the externally-tagged SpamHistoryOp enum: the wire
// value is a one-key map `{"Insert": {...}}`. The MDA only ever emits
// Insert (Delete is the client-side undo's shape); fxamacker/cbor's
// omitempty drops a nil variant pointer.
type spamHistoryOp struct {
	Insert *SpamHistoryInsert `cbor:"Insert,omitempty"`
}

// putSpamModelRequest mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:PutSpamModelRequest. ActorID
// names the served actor (the BridgeMda trusted-naming path — REQUIRED
// non-empty for a bridge caller; a `User`/`Admin` client leaves it empty
// and is rejected if it names anyone else). SampleCount is advisory
// display-only (never trusted). HistoryOp is `Option<SpamHistoryOp>` —
// nil pointer + omitempty ⇒ absent ⇒ a model-only write.
type putSpamModelRequest struct {
	ActorID     []byte               `cbor:"actor_id"`
	SealedModel []byte               `cbor:"sealed_model"`
	SampleCount uint32               `cbor:"sample_count"`
	HistoryOp   *spamHistoryOp       `cbor:"history_op,omitempty"`
	HolderCopy  *SpamModelHolderCopy `cbor:"holder_copy,omitempty"`
}

// SpamModelHolderCopy mirrors
// libs/fauna-protocol/src/wrapped_blob.rs:SpamModelHolderCopy — a
// deployment-baseline holder copy riding a `put_spam_model` write (piece b4:
// the MDA `\Junk`-train re-seal path). HolderPubkey is the aggregation
// holder's X25519 pubkey the copy is sealed to (the same identity
// `capability_grants.holder_pubkey` keys grants by); SealedCopy is the
// `SpamModelCopyBlob` canonical bytes (`fauna_ffi.SealSpamModelCopy`),
// nest-opaque.
type SpamModelHolderCopy struct {
	HolderPubkey []byte `cbor:"holder_pubkey"`
	SealedCopy   []byte `cbor:"sealed_copy"`
}

// putSpamModelReply mirrors PutSpamModelReply: the `outcome` string
// ("written" / "duplicate_signal"). A nest from before the field omits
// it, which decodes as "" and reads as written — the only outcome such a
// nest could produce.
type putSpamModelReply struct {
	Outcome string `cbor:"outcome"`
}

// PutSpamModelOutcome is what a put_spam_model write did — the typed
// wire `outcome`.
type PutSpamModelOutcome string

const (
	// PutOutcomeWritten — the model (and the history row, if any)
	// committed.
	PutOutcomeWritten PutSpamModelOutcome = "written"
	// PutOutcomeDuplicateSignal — the history insert repeated the actor's
	// newest recorded lesson for that message (the one-lesson rule,
	// mail-spam.md § 3): the nest wrote NOTHING — not the model, not the
	// row, not the holder copy. A caller carrying a mutated model forward
	// must discard this mutation and continue from the last accepted one.
	PutOutcomeDuplicateSignal PutSpamModelOutcome = "duplicate_signal"
)

// PutSpamModel writes the served actor's whole RE-SEALED spam model back
// opaque (the leg-2 write twin of FetchSpamModel): the MDA opened the
// sealed model under the actor's AUTH'd session, applied the training
// delta via the `apply_spam_training` FFI, re-sealed the result to the
// actor's own recipient key, and ships it here together with the sealed
// history audit row — the model write and the history INSERT commit in
// ONE nest transaction (crash-atomic). `insert` may be nil for a
// model-only write. `holderCopy` is non-nil iff the actor is opted into the
// deployment baseline and a holder is enrolled (piece b4) — a fresh
// `SpamModelCopyBlob` sealed to the aggregation holder, stored atomically
// alongside the model. The nest stores everything verbatim (it holds only
// the actor's public half — mail-spam.md § Wire shapes `put_spam_model`).
func PutSpamModel(ctx context.Context, c Caller, actorID, sealedModel []byte, sampleCount uint32, insert *SpamHistoryInsert, holderCopy *SpamModelHolderCopy) (PutSpamModelOutcome, error) {
	req := putSpamModelRequest{
		ActorID:     actorID,
		SealedModel: sealedModel,
		SampleCount: sampleCount,
		HolderCopy:  holderCopy,
	}
	if insert != nil {
		req.HistoryOp = &spamHistoryOp{Insert: insert}
	}
	var reply putSpamModelReply
	if err := c.Call(ctx, MethodPutSpamModel, req, &reply); err != nil {
		return "", fmt.Errorf("put_spam_model: %w", err)
	}
	switch reply.Outcome {
	case "", string(PutOutcomeWritten):
		return PutOutcomeWritten, nil
	case string(PutOutcomeDuplicateSignal):
		return PutOutcomeDuplicateSignal, nil
	default:
		// A newer nest's outcome this bridge cannot interpret: fail the
		// event (best-effort signal, the model stays at the last accepted
		// write) rather than guess whether anything was written.
		return "", fmt.Errorf("put_spam_model: unknown outcome %q", reply.Outcome)
	}
}

// ── I4 Phase D.5 outbound queue ───────────────────────────────────

// OutboundUnit mirrors libs/fauna-protocol/src/bridge_routing.rs
// OutboundUnit. One due outbound-queue row the MTA bridge polled out
// of nest. RawMessage is dot-stuffed, CRLF-terminated — ready to ship
// over SMTP — and already DKIM-signed by the nest where it signs for the
// message's From: domain; the worker relays the bytes as received.
type OutboundUnit struct {
	ID             int64  `cbor:"id"`
	MessageID      string `cbor:"message_id"`
	OriginalSender string `cbor:"original_sender"`
	Recipient      string `cbor:"recipient"`
	RawMessage     []byte `cbor:"raw_message"`
	AttemptCount   uint32 `cbor:"attempt_count"`
	// StagedBody is set IFF RawMessage is empty: the plaintext body was over the
	// inline reply budget, so nest sealed it under a one-shot AEAD key and staged
	// the ciphertext on the bulk-byte plane (the staged-envelope rule,
	// smtp-server.md § Message size limits). The worker GETs the chunks over the
	// open download route, joins them fail-closed on TotalBytes, and opens the
	// AEAD with Key to recover the identical RawMessage bytes (bodyFor). A nil
	// pointer is omitted on the wire (Rust `#[serde(default,
	// skip_serializing_if = "Option::is_none")]`), so an older nest's shape
	// decodes unchanged.
	StagedBody *StagedBodyRef `cbor:"staged_body,omitempty"`
}

// fetchOutboundDueRequest mirrors FetchOutboundDueRequest.
type fetchOutboundDueRequest struct {
	Max          uint32 `cbor:"max"`
	LeaseSeconds uint32 `cbor:"lease_seconds"`
}

// fetchOutboundDueReply mirrors FetchOutboundDueReply.
type fetchOutboundDueReply struct {
	Units []OutboundUnit `cbor:"units"`
}

// FetchOutboundDue polls nest for due outbound-queue rows the bridge
// should attempt to deliver. `max` is a batch ceiling; `leaseSeconds`
// is an advisory hint of how long the bridge expects to take before
// reporting back (nest may use it in future to hide leased rows from
// sibling MTA workers). Both must be > 0.
func FetchOutboundDue(ctx context.Context, c Caller, max, leaseSeconds uint32) ([]OutboundUnit, error) {
	req := fetchOutboundDueRequest{Max: max, LeaseSeconds: leaseSeconds}
	var reply fetchOutboundDueReply
	if err := c.Call(ctx, MethodFetchOutboundDue, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_outbound_due: %w", err)
	}
	return reply.Units, nil
}

// markOutboundDeliveredRequest mirrors MarkOutboundDeliveredRequest.
type markOutboundDeliveredRequest struct {
	ID int64 `cbor:"id"`
}

// markOutboundDeliveredReply mirrors MarkOutboundDeliveredReply.
type markOutboundDeliveredReply struct {
	OK bool `cbor:"ok"`
}

// MarkOutboundDelivered tells nest the recipient's MX accepted the
// message (2xx end-of-DATA). Nest transitions the row to `sent` and
// stops scheduling retries.
func MarkOutboundDelivered(ctx context.Context, c Caller, id int64) error {
	req := markOutboundDeliveredRequest{ID: id}
	var reply markOutboundDeliveredReply
	if err := c.Call(ctx, MethodMarkOutboundDelivered, req, &reply); err != nil {
		return fmt.Errorf("mark_outbound_delivered: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("mark_outbound_delivered: nest returned ok=false")
	}
	return nil
}

// markOutboundFailedRequest mirrors MarkOutboundFailedRequest.
type markOutboundFailedRequest struct {
	ID                int64  `cbor:"id"`
	RetryAfterSeconds uint32 `cbor:"retry_after_seconds"`
	LastError         string `cbor:"last_error"`
}

// markOutboundFailedReply mirrors MarkOutboundFailedReply.
type markOutboundFailedReply struct {
	OK bool `cbor:"ok"`
}

// MarkOutboundFailed tells nest the attempt soft-failed; nest owns the
// retry curve and reschedules (or, on budget/timeout exhaustion, bounces
// + emits the 4 h delay-warning). `retryAfterSeconds` is a server
// Retry-After hint (0 = none) nest honours as a floor under its backoff,
// never a ceiling. `lastError` is recorded for the eventual NDR +
// diagnostics.
func MarkOutboundFailed(ctx context.Context, c Caller, id int64, retryAfterSeconds uint32, lastError string) error {
	req := markOutboundFailedRequest{ID: id, RetryAfterSeconds: retryAfterSeconds, LastError: lastError}
	var reply markOutboundFailedReply
	if err := c.Call(ctx, MethodMarkOutboundFailed, req, &reply); err != nil {
		return fmt.Errorf("mark_outbound_failed: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("mark_outbound_failed: nest returned ok=false")
	}
	return nil
}

// markOutboundBouncedRequest mirrors MarkOutboundBouncedRequest.
type markOutboundBouncedRequest struct {
	ID     int64  `cbor:"id"`
	Reason string `cbor:"reason"`
}

// markOutboundBouncedReply mirrors MarkOutboundBouncedReply.
type markOutboundBouncedReply struct {
	OK bool `cbor:"ok"`
}

// MarkOutboundBounced tells nest the row terminally failed: 5xx from
// the recipient's MX, no MX resolved, or the retry budget was
// exhausted. Nest transitions the row to `bounced` and queues a DSN
// for the original sender (subject to per-sender bounce rate-limit).
func MarkOutboundBounced(ctx context.Context, c Caller, id int64, reason string) error {
	req := markOutboundBouncedRequest{ID: id, Reason: reason}
	var reply markOutboundBouncedReply
	if err := c.Call(ctx, MethodMarkOutboundBounced, req, &reply); err != nil {
		return fmt.Errorf("mark_outbound_bounced: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("mark_outbound_bounced: nest returned ok=false")
	}
	return nil
}

// fetchMtaStsPolicyRequest mirrors FetchMtaStsPolicyRequest
// (libs/fauna-protocol/src/bridge_routing.rs).
type fetchMtaStsPolicyRequest struct {
	Domain string `cbor:"domain"`
}

// FCrDNSModeWire names one value of the `fcrdns_mode` policy knob nest sends
// on FetchConfigReply (mail-policy-config.md § Inbound hardening). The owner is
// fauna_client_mail_settings::admin_policy::FcrdnsMode — the same table the
// tui and linux admin-mail pickers draw — and these constants are pinned
// against it by go_wire_outcome_contract_test.go, which also bans spelling a
// token by hand in ../mta/policy.go.
//
// Distinct from ../mta.FCrDNSMode, which is this process's *internal* int enum:
// ParseFCrDNSMode maps one to the other, and applies the unknown-token fallback
// the Rust owner documents (score_signal — observability-first: rejecting on a
// value nobody chose, or silently disabling a check the admin enabled, are both
// worse than scoring it).
type FCrDNSModeWire string

const (
	// FCrDNSModeWireOff skips the FCrDNS check entirely.
	FCrDNSModeWireOff FCrDNSModeWire = "off"
	// FCrDNSModeWireScoreSignal runs the check and forwards the verdict to
	// the spam scorer without rejecting.
	FCrDNSModeWireScoreSignal FCrDNSModeWire = "score_signal"
	// FCrDNSModeWireEnforce rejects a failing connection with 550 5.7.25,
	// once reject_fcrdns_fail is also set.
	FCrDNSModeWireEnforce FCrDNSModeWire = "enforce"
)

// MtaStsOutcome names one variant of the Rust FetchMtaStsPolicyReply
// `outcome` discriminator. The owner is
// fauna_mail::outbound::mta_sts::MtaStsOutcome; these constants are pinned
// against it by go_wire_outcome_contract_test.go, which also bans spelling
// any of these tokens by hand in ../mta/outbound.go.
//
// The outcome makes a round trip through this binary: nest sends it here,
// and ReportTlsAttemptRequest.MtaStsOutcome echoes it back so nest rebuilds
// the same RFC 8460 §4.4 TLSRPT policy bucket without a second fetch.
type MtaStsOutcome string

const (
	// MtaStsOutcomeNotPublished — no `_mta-sts.<domain>` TXT (or it lacks
	// v=STSv1 + id=). STS is not advertised; TLSRPT attributes
	// policy_type="no-policy-found".
	MtaStsOutcomeNotPublished MtaStsOutcome = "not_published"
	// MtaStsOutcomeFetchError — TXT advertises STS but the HTTPS GET / DNS
	// failed. Per RFC 8461 §5 this is treated as no-policy for delivery: it
	// must never force plaintext or refusal. RFC 8460 §4.3 still wants it
	// reported as `sts-policy-fetch-error` rather than `no-policy-found`.
	MtaStsOutcomeFetchError MtaStsOutcome = "fetch_error"
	// MtaStsOutcomeInvalid — body fetched but unparseable (RFC 8461 §3.2
	// violation). No-policy for delivery, `sts-policy-invalid` for TLSRPT.
	MtaStsOutcomeInvalid MtaStsOutcome = "invalid"
	// MtaStsOutcomeFound — policy fetched + parsed; Policy is non-nil.
	MtaStsOutcomeFound MtaStsOutcome = "found"
)

// MtaStsMode names one RFC 8461 §3.2 `mode:` token, carried on
// MtaStsPolicyWire.Mode. Owner: fauna_mail::outbound::mta_sts::MtaStsMode;
// same contract test.
type MtaStsMode string

const (
	// MtaStsModeEnforce — the sender must refuse an MX the policy's `mx:`
	// list does not cover.
	MtaStsModeEnforce MtaStsMode = "enforce"
	// MtaStsModeTesting — attempt STARTTLS and report failures via TLSRPT,
	// but never refuse delivery.
	MtaStsModeTesting MtaStsMode = "testing"
	// MtaStsModeNone — the domain published a policy that asks for nothing.
	MtaStsModeNone MtaStsMode = "none"
)

// MtaStsPolicyWire mirrors MtaStsPolicyWire (RFC 8461 §3.2 policy body),
// present iff FetchMtaStsPolicyReply.Outcome == MtaStsOutcomeFound. `Mx`
// preserves the policy's `mx:` input order; RFC 8461 §4.1 matching (case /
// trailing-dot / single-label wildcard) is applied by the UniFFI
// matcher fauna_ffi.MtaStsMxMatches.
type MtaStsPolicyWire struct {
	ID         string   `cbor:"id"`
	Mode       string   `cbor:"mode"`
	Mx         []string `cbor:"mx"`
	MaxAgeSecs uint32   `cbor:"max_age_secs"`
}

// FetchMtaStsPolicyReply mirrors FetchMtaStsPolicyReply. `Outcome` is one of
// the MtaStsOutcome constants above; `Policy` is non-nil iff Outcome ==
// MtaStsOutcomeFound. Per RFC 8461 §5 the three non-found outcomes are all
// treated as no-policy by the caller — a published-but-broken policy never
// forces plaintext fallback or refusal.
type FetchMtaStsPolicyReply struct {
	Outcome string            `cbor:"outcome"`
	Policy  *MtaStsPolicyWire `cbor:"policy"`
}

// FetchMtaStsPolicy asks nest for the recipient domain's MTA-STS policy
// (RFC 8461) at delivery time. nest owns the `_mta-sts.<domain>` TXT +
// `.well-known/mta-sts.txt` fetch and the per-`max_age` cache; the
// caller applies the per-host enforce/testing decision locally before
// the TLS handshake. An RPC error is the caller's signal to proceed
// without enforcement (an MTA-STS fetch failure must not block delivery).
func FetchMtaStsPolicy(ctx context.Context, c Caller, domain string) (FetchMtaStsPolicyReply, error) {
	req := fetchMtaStsPolicyRequest{Domain: domain}
	var reply FetchMtaStsPolicyReply
	if err := c.Call(ctx, MethodFetchMtaStsPolicy, req, &reply); err != nil {
		return FetchMtaStsPolicyReply{}, fmt.Errorf("fetch_mta_sts_policy: %w", err)
	}
	return reply, nil
}

// fetchTlsaRequest mirrors FetchTlsaRequest
// (libs/fauna-protocol/src/bridge_routing.rs).
type fetchTlsaRequest struct {
	MxHost string `cbor:"mx_host"`
}

// TlsaRecordWire mirrors TlsaRecordWire (RFC 6698). nest returns only
// DNSSEC-secure, SMTP-usable (DANE-TA/EE) records; `Data` is the
// certificate-association data (raw bytes / digest), carried as a CBOR
// byte string.
type TlsaRecordWire struct {
	Usage    uint8  `cbor:"usage"`
	Selector uint8  `cbor:"selector"`
	Matching uint8  `cbor:"matching"`
	Data     []byte `cbor:"data"`
}

// FetchTlsaReply mirrors FetchTlsaReply. An empty `Records` list means the
// host publishes no usable DANE/TLSA records — the caller falls back to the
// MTA-STS / opportunistic posture (no DANE pinning). The pin decision uses
// the UniFFI matcher fauna_ffi.DaneChainMatches against the presented chain.
type FetchTlsaReply struct {
	Records []TlsaRecordWire `cbor:"records"`
}

// FetchTlsa asks nest for a recipient MX host's DANE/TLSA records (RFC 7672)
// at delivery time. nest owns the `_25._tcp.<mx_host>` DNSSEC-validating
// lookup (the Go stdlib can't do DNSSEC); the caller pins the outbound TLS
// handshake against the returned records. An RPC error is the caller's
// signal to proceed without DANE (a TLSA fetch failure must not block
// delivery — fall back to the MTA-STS / opportunistic posture).
func FetchTlsa(ctx context.Context, c Caller, mxHost string) (FetchTlsaReply, error) {
	req := fetchTlsaRequest{MxHost: mxHost}
	var reply FetchTlsaReply
	if err := c.Call(ctx, MethodFetchTlsa, req, &reply); err != nil {
		return FetchTlsaReply{}, fmt.Errorf("fetch_tlsa: %w", err)
	}
	return reply, nil
}

// resolveMxRequest mirrors ResolveMxRequest
// (libs/fauna-protocol/src/bridge_routing.rs).
type resolveMxRequest struct {
	Domain string `cbor:"domain"`
}

// MxHostWire mirrors MxHostWire: one ranked SMTP target. Priority is the
// RFC 5321 §5.1 MX preference (lowest first); Hostname is the bare exchange
// name, no trailing dot and no port.
type MxHostWire struct {
	Priority uint16 `cbor:"priority"`
	Hostname string `cbor:"hostname"`
}

// ResolveMxReply mirrors ResolveMxReply.
//
// Secure reports whether the MX RRset these hosts came from carried a DNSSEC
// Secure proof. The caller MUST gate DANE pinning on it: without that gate
// the DNSSEC validation on the TLSA leg authenticates a name the attacker
// chose — a DNS-spoofing attacker forges `MX victim.test → mx.attacker.test`,
// publishes a genuine signed TLSA for their own name, and the pin succeeds
// against the wrong host (smtp-server.md § Architectural rules, outbound
// DANE; RFC 7672 §2.2). Secure=false means deliver anyway, with the
// MTA-STS/opportunistic posture and no DANE — never fail delivery over it.
type ResolveMxReply struct {
	Hosts  []MxHostWire `cbor:"hosts"`
	Secure bool         `cbor:"secure"`
}

// ResolveMx asks nest to resolve a recipient domain's SMTP targets. nest owns
// the lookup for the same stated reason it owns fetch_tlsa: the Go stdlib
// resolver cannot do DNSSEC, and outbound DANE may only bind to a name that
// came out of a DNSSEC-validated MX RRset (RFC 7672 §2.2). Unlike a TLSA
// fetch failure, an error here has no safe fallback — the caller has no hosts
// to deliver to — so it tempfails into the retry curve.
func ResolveMx(ctx context.Context, c Caller, domain string) (ResolveMxReply, error) {
	req := resolveMxRequest{Domain: domain}
	var reply ResolveMxReply
	if err := c.Call(ctx, MethodResolveMx, req, &reply); err != nil {
		return ResolveMxReply{}, fmt.Errorf("resolve_mx: %w", err)
	}
	return reply, nil
}

// reportTlsAttemptRequest mirrors ReportTlsAttemptRequest (T2.4,
// libs/fauna-protocol/src/bridge_routing.rs). After each outbound delivery
// attempt to one MX host, the bridge reports the raw TLS-posture facts it
// already holds; nest reconstructs the RFC 8460 §4.4 policy bucket via the
// shared pure fauna_mail::outbound::tlsrpt::policy_for_attempt and records
// it into the daily TLSRPT aggregator. The only Go-side-original field is
// ResultType (the crypto/tls handshake outcome). `ResultType` /
// `MtaStsPolicy` are pointers (Rust `Option<…>`): nil → null → None.
type reportTlsAttemptRequest struct {
	RecipientDomain string            `cbor:"recipient_domain"`
	MxHost          string            `cbor:"mx_host"`
	ResultType      *string           `cbor:"result_type"`
	MtaStsOutcome   string            `cbor:"mta_sts_outcome"`
	MtaStsPolicy    *MtaStsPolicyWire `cbor:"mta_sts_policy"`
	TlsaRecords     []TlsaRecordWire  `cbor:"tlsa_records"`
}

// reportTlsAttemptReply mirrors ReportTlsAttemptReply.
type reportTlsAttemptReply struct {
	OK bool `cbor:"ok"`
}

// ReportTlsAttempt reports one outbound TLS attempt's outcome to nest's
// TLSRPT aggregator (RFC 8460). `resultType` is nil for a successful TLS
// session, else an RFC 8460 §4.3 result-type token. `mtaStsOutcome` echoes
// the FetchMtaStsPolicy outcome back (one of the MtaStsOutcome constants) so
// nest re-derives the policy bucket without a second fetch; `mtaStsPolicy` is
// non-nil iff the outcome was MtaStsOutcomeFound; `tlsaRecords` is non-empty
// iff DANE applied. A reporting RPC error must NOT fail delivery — the caller
// logs and proceeds (TLSRPT is cooperative, not load-bearing).
//
// The parameter is the named type rather than a bare string precisely because
// this is the echo half of a round trip nest re-parses: an outcome this bridge
// invented would be rejected there as malformed, and the token vocabulary is
// owned by fauna_mail::outbound::mta_sts::MtaStsOutcome.
func ReportTlsAttempt(
	ctx context.Context,
	c Caller,
	recipientDomain, mxHost string,
	resultType *string,
	mtaStsOutcome MtaStsOutcome,
	mtaStsPolicy *MtaStsPolicyWire,
	tlsaRecords []TlsaRecordWire,
) error {
	// nest decodes `tlsa_records` into a Rust Vec, which rejects a CBOR
	// null — always send an array, never a nil slice.
	if tlsaRecords == nil {
		tlsaRecords = []TlsaRecordWire{}
	}
	req := reportTlsAttemptRequest{
		RecipientDomain: recipientDomain,
		MxHost:          mxHost,
		ResultType:      resultType,
		MtaStsOutcome:   string(mtaStsOutcome),
		MtaStsPolicy:    mtaStsPolicy,
		TlsaRecords:     tlsaRecords,
	}
	var reply reportTlsAttemptReply
	if err := c.Call(ctx, MethodReportTlsAttempt, req, &reply); err != nil {
		return fmt.Errorf("report_tls_attempt: %w", err)
	}
	if !reply.OK {
		return fmt.Errorf("report_tls_attempt: nest returned ok=false")
	}
	return nil
}

// enqueueOutboundMailRequest mirrors EnqueueOutboundMailRequest.
//
// OnBehalfOfActor is a pointer-nullable mirror of the Rust
// `Option<Vec<u8>>` (`omitempty` so a nil pointer is omitted on the wire —
// the bare-slice `NilContainersAsEmpty` codec footgun would otherwise encode
// nil as `Some(empty)`). The MTA submission path leaves it nil; the MDA's
// server-side auto-schedule gateway sets it to the AUTH'd organizer's actor id
// so nest caller-scopes the enqueue to that organizer (caldav-server.md
// § Server-side auto-schedule).
type enqueueOutboundMailRequest struct {
	OriginalMsgID   string   `cbor:"original_msgid"`
	OriginalSender  string   `cbor:"original_sender"`
	Recipients      []string `cbor:"recipients"`
	RawMessage      []byte   `cbor:"raw_message"`
	OnBehalfOfActor *[]byte  `cbor:"on_behalf_of_actor,omitempty"`
	// StagedBody is set IFF RawMessage is empty: the body was over the
	// inline request budget, so the MTA sealed it under a one-shot AEAD key and
	// staged the ciphertext on the bulk-byte plane (the staged-envelope rule,
	// smtp-server.md § Message size limits). nest resolves the chunks from its own
	// blob store, joins them fail-closed on TotalBytes, opens the AEAD with Key,
	// and enqueues the recovered RawMessage exactly as an inline submission. A nil
	// pointer is omitted on the wire (Rust `#[serde(default,
	// skip_serializing_if = "Option::is_none")]`), so the inline MTA bytes are
	// unchanged.
	StagedBody *StagedBodyRef `cbor:"staged_body,omitempty"`
}

// enqueueOutboundMailReply mirrors EnqueueOutboundMailReply.
type enqueueOutboundMailReply struct {
	IDs []int64 `cbor:"ids"`
}

// EnqueueOutboundMail hands a freshly-signed submission body off to
// nest's outbound queue. One row per recipient is created; the
// returned ids are in the same order as `recipients` (useful for
// log correlation when those rows later land on the worker via
// FetchOutboundDue).
//
// onBehalfOfActor is nil for the MTA submission path (sender-unconstrained,
// nest trusts the AUTH'd submitter). The MDA's server-side auto-schedule
// gateway passes the AUTH'd organizer's actor id so nest caller-scopes the
// enqueue: it accepts the call only if originalSender resolves to that actor
// (caldav-server.md § Server-side auto-schedule).
//
// stagedBody is nil for the common inline path (rawMessage carries the body).
// When the body is over the inline request budget the MTA stages
// the ciphertext on the bulk-byte plane and passes stagedBody with an empty
// rawMessage (the staged-envelope rule); nest recovers the identical bytes.
func EnqueueOutboundMail(ctx context.Context, c Caller, originalMsgID, originalSender string, recipients []string, rawMessage []byte, onBehalfOfActor *[]byte, stagedBody *StagedBodyRef) ([]int64, error) {
	req := enqueueOutboundMailRequest{
		OriginalMsgID:   originalMsgID,
		OriginalSender:  originalSender,
		Recipients:      recipients,
		RawMessage:      rawMessage,
		OnBehalfOfActor: onBehalfOfActor,
		StagedBody:      stagedBody,
	}
	var reply enqueueOutboundMailReply
	if err := c.Call(ctx, MethodEnqueueOutboundMail, req, &reply); err != nil {
		return nil, fmt.Errorf("enqueue_outbound_mail: %w", err)
	}
	return reply.IDs, nil
}

// ── MDA auto-schedule mailbox-less rail (deliver_sealed_scheduling /
//    actor.by_handle / keypackage.fetch — caldav-server.md § Server-side
//    auto-schedule, C4) ───────────────────────────────────────────────────

// deliverSealedSchedulingRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:DeliverSealedSchedulingRequest
// (`#[serde(deny_unknown_fields)]` — send exactly these fields, no extras).
//
// The MDA seals the one-off MLS scheduling welcome + iMIP message ITSELF
// (mailfauna.BuildSchedulingDelivery, an ephemeral signer — nest only ever sees
// ciphertext) and ships the OPAQUE bytes here. nest runs welcome.deliver
// (tagged Scheduling) + channel.send AS THE ORGANIZER, caller-scoped to it
// exactly like enqueue_outbound_mail (BridgeMda-only; OnBehalfOfActor set,
// OriginalSender must resolve, exact-alias, to it). PeerDomain carries the rail
// cross-nest: a non-nil Some(domain) ⇒ the recipient lives on a foreign nest
// (nest relays the welcome there); nil ⇒ same-nest. serde_bytes Vec<u8> →
// []byte; Option<String> with skip_serializing_if ⇒ *string omitempty (a nil
// pointer is omitted, matching None — the bare-string codec footgun would
// otherwise send "" as Some("")).
type deliverSealedSchedulingRequest struct {
	OnBehalfOfActor  []byte  `cbor:"on_behalf_of_actor"`
	OriginalSender   string  `cbor:"original_sender"`
	RecipientActorID string  `cbor:"recipient_actor_id"`
	PeerDomain       *string `cbor:"peer_domain,omitempty"`
	ChannelID        string  `cbor:"channel_id"`
	WelcomeBytes     []byte  `cbor:"welcome_bytes"`
	AppEnvelope      []byte  `cbor:"app_envelope"`
}

// deliverSealedSchedulingReply mirrors DeliverSealedSchedulingReply.
type deliverSealedSchedulingReply struct {
	InboxID int64 `cbor:"inbox_id"`
	Seq     int64 `cbor:"seq"`
}

// DeliverSealedScheduling ships an opaque MDA-sealed scheduling delivery
// (one-off MLS welcome + iMIP channel message) to a mailbox-less Fauna recipient
// over the caller-scoped BridgeMda rail. `onBehalfOfActor` is the AUTH'd
// organizer (32 bytes); `originalSender` its `local@domain` (nest verifies it
// resolves to the organizer); `peerDomain` is "" for same-nest (omitted on the
// wire) or the recipient's foreign-nest domain for cross-nest. Returns the
// (inboxID, seq) from the welcome push.
func DeliverSealedScheduling(
	ctx context.Context, c Caller,
	onBehalfOfActor []byte, originalSender, recipientActorID, peerDomain, channelID string,
	welcomeBytes, appEnvelope []byte,
) (inboxID, seq int64, err error) {
	req := deliverSealedSchedulingRequest{
		OnBehalfOfActor:  onBehalfOfActor,
		OriginalSender:   originalSender,
		RecipientActorID: recipientActorID,
		ChannelID:        channelID,
		WelcomeBytes:     welcomeBytes,
		AppEnvelope:      appEnvelope,
	}
	if peerDomain != "" {
		req.PeerDomain = &peerDomain
	}
	var reply deliverSealedSchedulingReply
	if err := c.Call(ctx, MethodDeliverSealedScheduling, req, &reply); err != nil {
		return 0, 0, fmt.Errorf("deliver_sealed_scheduling: %w", err)
	}
	return reply.InboxID, reply.Seq, nil
}

// ErrActorNotFound is the sentinel ActorByHandle returns when nest rejects the
// handle with `fauna.actor.not_found` — a NORMAL result for the MDA classifier
// (the attendee address is not a local Fauna handle), distinct from a
// transport/decode failure. Callers test it via errors.Is and route the
// recipient to the email rail.
var ErrActorNotFound = errors.New("actor_by_handle: not found")

// actorByHandleRequest mirrors
// libs/fauna-protocol/src/discovery.rs:ActorByHandleRequest. The Rust type
// carries a flattened `extra` map (default), so omitting it is fine.
type actorByHandleRequest struct {
	Handle string `cbor:"handle"`
}

// actorByHandleReply is a flat decode of ActorByHandleReply; the echoed
// `handle`/`addresses` fields and the flattened `extra` map are unknown to this
// struct and ignored. `actor_id` is 64-char hex of the 32-byte actor key;
// `domain` is the nest's handle domain; `addressable` is the Spec-Y2
// reachability probe (≥1 usable key package).
type actorByHandleReply struct {
	ActorID     string `cbor:"actor_id"`
	Domain      string `cbor:"domain"`
	Addressable bool   `cbor:"addressable"`
}

// ActorByHandleResult is the typed return of ActorByHandle.
type ActorByHandleResult struct {
	ActorID     string
	Domain      string
	Addressable bool
}

// ActorByHandle resolves a handle to an actor for addressing
// (fauna.actor.by_handle). It is a public, anonymous-reachable discovery read,
// so a BridgeMda caller reaches it WITHOUT an allowlist widening: the
// per-handler caller-class gate (`bridge_method_allowlist::is_permitted`) is
// opt-in and the discovery handlers don't apply it; an authenticated bridge
// connection skips the anonymous pre-identity gate (routes.rs dispatch_request)
// and trips no loopback gate, so the handler runs. The MDA's mailbox-less
// auto-schedule classifier uses it to map a local-domain attendee
// `<local>@<domain>` to its actor_id (resolve_recipient gives no actor_id on a
// mailbox-less reject). An unknown handle returns ErrActorNotFound (errors.Is);
// any other non-nil error is transport/decode.
func ActorByHandle(ctx context.Context, c Caller, handle string) (ActorByHandleResult, error) {
	var reply actorByHandleReply
	if err := c.Call(ctx, MethodActorByHandle, actorByHandleRequest{Handle: handle}, &reply); err != nil {
		if code, ok := RpcErrorCode(err); ok && code == "fauna.actor.not_found" {
			return ActorByHandleResult{}, ErrActorNotFound
		}
		return ActorByHandleResult{}, fmt.Errorf("actor_by_handle: %w", err)
	}
	if reply.ActorID == "" {
		return ActorByHandleResult{}, fmt.Errorf("actor_by_handle: resolved with empty actor_id")
	}
	return ActorByHandleResult{
		ActorID:     reply.ActorID,
		Domain:      reply.Domain,
		Addressable: reply.Addressable,
	}, nil
}

// keypackageFetchRequest mirrors
// libs/fauna-protocol/src/conversations.rs:KeypackageFetchRequest. NestURL is
// the Option<String> cross-nest relay target (Some(base_url) ⇒ the home nest
// signs + forwards the fetch to that foreign nest); nil ⇒ same-nest. *string
// omitempty so a nil pointer is omitted (None), dodging the bare-string
// Some("") footgun. The flattened `extra` map (default) is omitted.
type keypackageFetchRequest struct {
	ActorID string  `cbor:"actor_id"`
	NestURL *string `cbor:"nest_url,omitempty"`
}

// keypackageFetchReply mirrors KeypackageFetchReply: key_package is
// Option<Vec<u8>> (serde_bytes) — None ⇔ no non-expired key package available —
// so *[]byte, nil ⇒ absent.
type keypackageFetchReply struct {
	KeyPackage *[]byte `cbor:"key_package,omitempty"`
}

// KeypackageFetch consumes (FIFO, oldest non-expired) one key package belonging
// to actorID — the recipient KP the MDA seals a one-off scheduling delivery
// against (fauna.conversations.keypackage.fetch, widened to User | BridgeMda in
// C3). nestURL is "" for same-nest; a non-empty base URL fetches a foreign-nest
// recipient's KP via the home nest's signed relay. A nil return with nil error
// means no key package is available (the recipient is not currently
// reachable) — the caller treats it as "can't seal, fall back to email".
func KeypackageFetch(ctx context.Context, c Caller, actorID, nestURL string) ([]byte, error) {
	req := keypackageFetchRequest{ActorID: actorID}
	if nestURL != "" {
		req.NestURL = &nestURL
	}
	var reply keypackageFetchReply
	if err := c.Call(ctx, MethodKeypackageFetch, req, &reply); err != nil {
		return nil, fmt.Errorf("keypackage_fetch: %w", err)
	}
	if reply.KeyPackage == nil {
		return nil, nil
	}
	return *reply.KeyPackage, nil
}

// sendAutoReplyRequest mirrors fauna_protocol::bridge_routing::SendAutoReplyRequest.
type sendAutoReplyRequest struct {
	RecipientActorID []byte `cbor:"recipient_actor_id"`
	EnvelopeFrom     string `cbor:"envelope_from"`
	IntervalHours    uint32 `cbor:"interval_hours"`
	OriginalMsgID    string `cbor:"original_msgid"`
	RawMessage       []byte `cbor:"raw_message"`
}

// sendAutoReplyReply mirrors SendAutoReplyReply.
type sendAutoReplyReply struct {
	Sent bool `cbor:"sent"`
}

// SendAutoReply hands nest the composed vacation reply (unsigned — the nest
// signs it at the outbound hand-out). nest
// atomically claims the (recipient, envelope-sender) rate-limit slot — keyed on
// a hash of the lowercased envelope-from — and, if free, enqueues the reply
// null-sender. A `true` return means the reply is now queued; `false` means a
// reply was already sent within `intervalHours` (suppressed).
func SendAutoReply(ctx context.Context, c Caller, recipientActorID []byte, envelopeFrom string, intervalHours uint32, originalMsgID string, rawMessage []byte) (bool, error) {
	req := sendAutoReplyRequest{
		RecipientActorID: recipientActorID,
		EnvelopeFrom:     envelopeFrom,
		IntervalHours:    intervalHours,
		OriginalMsgID:    originalMsgID,
		RawMessage:       rawMessage,
	}
	var reply sendAutoReplyReply
	if err := c.Call(ctx, MethodSendAutoReply, req, &reply); err != nil {
		return false, fmt.Errorf("send_auto_reply: %w", err)
	}
	return reply.Sent, nil
}

// ── fetch_recipient_forward_config / forward_message (mail-forwarding N2) ──
//
// The MTA's post-delivery forward stage (mail-forwarding.md § Trigger point).
// fetch_recipient_forward_config is the single per-recipient chokepoint for
// reading the recipient's forward-all target at the perimeter (the one spot
// the N1b sealed-fetch upgrade swaps); forward_message enqueues one
// outbound_mail_queue row (is_forwarded=1) carrying the forwarder actor +
// rule. Mirrors libs/fauna-protocol/src/bridge_routing.rs:
// {FetchRecipientForwardConfig*, ForwardMessage*, ForwardCopyMode}.

// fetchRecipientForwardConfigRequest mirrors FetchRecipientForwardConfigRequest.
type fetchRecipientForwardConfigRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchRecipientForwardConfigReply mirrors FetchRecipientForwardConfigReply.
// `forward_all_to` is nil/absent when forward-all is disabled (no row / NULL
// / blank) — the Rust side is `Option<String>` (serialized as CBOR null).
type fetchRecipientForwardConfigReply struct {
	ForwardAllTo *string `cbor:"forward_all_to"`
}

// FetchRecipientForwardConfig reads the recipient actor's forward-all target.
// Returns "" when forward-all is disabled (the nest collapses no-row / NULL /
// blank to None).
func FetchRecipientForwardConfig(ctx context.Context, c Caller, actorID []byte) (string, error) {
	req := fetchRecipientForwardConfigRequest{ActorID: actorID}
	var reply fetchRecipientForwardConfigReply
	if err := c.Call(ctx, MethodFetchRecipientForwardConfig, req, &reply); err != nil {
		return "", fmt.Errorf("fetch_recipient_forward_config: %w", err)
	}
	if reply.ForwardAllTo == nil {
		return "", nil
	}
	return *reply.ForwardAllTo, nil
}

// ── fetch_recipient_filters (T3.3 delivery-time email filter rules) ───
//
// The MTA's per-recipient chokepoint for reading a recipient's stored
// Sieve-like filter rules at delivery time. The rules are *stored* in nest
// (`email_filters`, owned per-actor) and *evaluated* Go-side at the perimeter
// on the plaintext envelope/headers/spam-score — nest never evaluates them
// (in encrypted mode it cannot). Mirrors
// libs/fauna-protocol/src/bridge_routing.rs:FetchRecipientFiltersReply and the
// user-facing libs/fauna-protocol/src/email.rs:EmailFilter row shape.
//
// `rules` and `action` are externally-tagged serde enums on the wire
// (`serde_ipld_dagcbor`): a struct variant is a single-key CBOR map
// (`{"SenderDomain":{"domain":"x"}}`), a unit variant is a bare string
// (`"Allow"`). This transport layer leaves them as `cbor.RawMessage`;
// `mailfauna.StoredFiltersFromWire` interprets them into the
// `fauna_mail::filter` variants the evaluator consumes (keeping the generated
// fauna_mail package off the wsrpc transport layer).

// EmailFilterWire is the on-wire shape of one stored filter row
// (`email.rs:EmailFilter`). `Rules`/`Action` are the raw externally-tagged
// enum bytes — `mailfauna.StoredFiltersFromWire` decodes them.
type EmailFilterWire struct {
	ID              int64             `cbor:"id"`
	Name            string            `cbor:"name"`
	Rules           []cbor.RawMessage `cbor:"rules"`
	Combination     string            `cbor:"combination"`
	Action          cbor.RawMessage   `cbor:"action"`
	Priority        int32             `cbor:"priority"`
	ContinueOnMatch bool              `cbor:"continue_on_match"`
	CreatedAt       int64             `cbor:"created_at"`
}

// fetchRecipientFiltersRequest mirrors FetchRecipientFiltersRequest.
type fetchRecipientFiltersRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchRecipientFiltersReply mirrors FetchRecipientFiltersReply. nest returns
// the actor's filters in `priority ASC, id ASC` order (the evaluator re-sorts
// defensively, so order is informational here).
type fetchRecipientFiltersReply struct {
	Filters []EmailFilterWire `cbor:"filters"`
}

// FetchRecipientFilters reads the recipient actor's stored filter rules for
// delivery-time evaluation. An empty slice means the recipient has no rules
// (the common case) — the caller falls through to spam-disposition placement.
func FetchRecipientFilters(ctx context.Context, c Caller, actorID []byte) ([]EmailFilterWire, error) {
	req := fetchRecipientFiltersRequest{ActorID: actorID}
	var reply fetchRecipientFiltersReply
	if err := c.Call(ctx, MethodFetchRecipientFilters, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_recipient_filters: %w", err)
	}
	return reply.Filters, nil
}

// ForwardCopyMode is the wire enum for forward_message's copy_mode (Rust
// `ForwardCopyMode`, serde rename_all=snake_case). Forward-all is always
// `Copy`; `Redirect` is the per-rule / admin-forwarder no-local-copy shape.
type ForwardCopyMode string

const (
	ForwardCopyModeCopy     ForwardCopyMode = "copy"
	ForwardCopyModeRedirect ForwardCopyMode = "redirect"
)

// forwardMessageRequest mirrors ForwardMessageRequest.
type forwardMessageRequest struct {
	ActorID            []byte          `cbor:"actor_id"`
	OriginalMsgID      string          `cbor:"original_msgid"`
	OriginalSender     string          `cbor:"original_sender"`
	Destination        string          `cbor:"destination"`
	RawMessage         []byte          `cbor:"raw_message"`
	RuleIDOrForwardAll string          `cbor:"rule_id_or_forward_all"`
	CopyMode           ForwardCopyMode `cbor:"copy_mode"`
}

// forwardMessageReply mirrors ForwardMessageReply. `Queued` is true when the
// forward exceeded the per-account hourly rate cap and was parked in nest's
// forward_queue (promoted later at the rate-cap cadence) rather than dispatched
// now (mail-forwarding.md § Per-account forward rate-limit). The MTA
// fire-and-forgets either way; the field is informational.
type forwardMessageReply struct {
	ID     int64 `cbor:"id"`
	Queued bool  `cbor:"queued"`
}

// ForwardMessage enqueues one outbound_mail_queue forward row
// (is_forwarded=1, carrying the forwarder actor + rule for the N3 SRS rewrite
// + N4 NDR routing). The MTA must have already locally delivered, stamped
// X-Fauna-Forwarded-By onto rawMessage, and skipped null-sender /
// loop-suppressed messages before calling (mail-forwarding.md § Trigger point
// / § Loop detection). The original envelope is stored as-is; SRS rewrite is
// at queue-out (N3). Returns the enqueued row id.
func ForwardMessage(
	ctx context.Context,
	c Caller,
	actorID []byte,
	originalMsgID, originalSender, destination string,
	rawMessage []byte,
	ruleIDOrForwardAll string,
	copyMode ForwardCopyMode,
) (int64, error) {
	req := forwardMessageRequest{
		ActorID:            actorID,
		OriginalMsgID:      originalMsgID,
		OriginalSender:     originalSender,
		Destination:        destination,
		RawMessage:         rawMessage,
		RuleIDOrForwardAll: ruleIDOrForwardAll,
		CopyMode:           copyMode,
	}
	var reply forwardMessageReply
	if err := c.Call(ctx, MethodForwardMessage, req, &reply); err != nil {
		return 0, fmt.Errorf("forward_message: %w", err)
	}
	return reply.ID, nil
}

// ── decode_srs_bounce (mail-forwarding N4) ───────────────────────────

// SrsBounceOutcome names one variant of the Rust DecodeSrsBounceReply
// `outcome` discriminator (mail-forwarding.md § Bounce decode). The
// wire-level snake_case string is preserved verbatim so future variants
// land additively.
//
// The owner is fauna_mail::srs::SrsBounceOutcome; these constants are pinned
// against it by go_wire_outcome_contract_test.go, which also bans spelling any
// of these tokens by hand in ../mta/server.go. Before 2026-08-23 this family
// was the *stronger* of the two sides — Rust hand-wrote all six literals
// against a `pub outcome: String` — and nothing compared them.
type SrsBounceOutcome string

const (
	// SrsBounceOutcomeOk — verified, our-issued bounce; ForwarderActorID /
	// OriginalSender / OriginalDestination are populated. The MTA delivers
	// the bounce to the forwarder's mailbox.
	SrsBounceOutcomeOk SrsBounceOutcome = "ok"
	// SrsBounceOutcomeNotSrs — the local-part is not an SRS0=/SRS1= address; the
	// caller treats it as a normal recipient (falls through to validate_recipient).
	SrsBounceOutcomeNotSrs SrsBounceOutcome = "not_srs"
	// SrsBounceOutcomeMalformed — looks like SRS but is structurally invalid → 550.
	SrsBounceOutcomeMalformed SrsBounceOutcome = "malformed"
	// SrsBounceOutcomeMacFail — HMAC mismatch (forged/corrupt) → 550 5.1.1, no retry.
	SrsBounceOutcomeMacFail SrsBounceOutcome = "mac_fail"
	// SrsBounceOutcomeExpired — TT age over the max → 550 5.4.4.
	SrsBounceOutcomeExpired SrsBounceOutcome = "expired"
	// SrsBounceOutcomeOrphan — verified, but the forwarding row is gone (account
	// deleted / row pruned). RCPT is accepted, but DATA is dropped + countered;
	// never delivered to the admin mailbox (it carries the original sender's PII).
	SrsBounceOutcomeOrphan SrsBounceOutcome = "orphan"
)

// decodeSrsBounceRequest mirrors DecodeSrsBounceRequest. `local_part` is the
// RCPT TO part before `@<our-domain>` (the bridge strips the domain).
type decodeSrsBounceRequest struct {
	LocalPart string `cbor:"local_part"`
}

// decodeSrsBounceReply mirrors DecodeSrsBounceReply. The payload fields are
// populated only on Outcome=="ok"; serde sends zero-length / empty otherwise.
type decodeSrsBounceReply struct {
	Outcome             string `cbor:"outcome"`
	ForwarderActorID    []byte `cbor:"forwarder_actor_id,omitempty"`
	OriginalSender      string `cbor:"original_sender,omitempty"`
	OriginalDestination string `cbor:"original_destination,omitempty"`
}

// SrsBounceDecoded is the decoded result of an inbound SRS bounce recipient.
// Only the Outcome field is meaningful for every variant; the address fields
// are set only when Outcome==SrsBounceOutcomeOk.
type SrsBounceDecoded struct {
	Outcome             SrsBounceOutcome
	ForwarderActorID    []byte
	OriginalSender      string
	OriginalDestination string
}

// DecodeSrsBounce decodes + verifies an inbound `SRS0=`/`SRS1=` recipient at
// RCPT-TO against nest's stored SRS secret(s) (mail-forwarding.md § Bounce
// decode). On SrsBounceOutcomeOk the result names the forwarding actor + the bounced
// destination so the caller routes the NDR to the forwarder, not the original
// sender (§ NDR routing). `localPart` is the RCPT local-part without the
// trailing `@<our-domain>`. A non-nil error is a transport / decode failure —
// the caller tempfails (451) rather than guessing an outcome.
func DecodeSrsBounce(ctx context.Context, c Caller, localPart string) (SrsBounceDecoded, error) {
	req := decodeSrsBounceRequest{LocalPart: localPart}
	var reply decodeSrsBounceReply
	if err := c.Call(ctx, MethodDecodeSrsBounce, req, &reply); err != nil {
		return SrsBounceDecoded{}, fmt.Errorf("decode_srs_bounce: %w", err)
	}
	return SrsBounceDecoded{
		Outcome:             SrsBounceOutcome(reply.Outcome),
		ForwarderActorID:    reply.ForwarderActorID,
		OriginalSender:      reply.OriginalSender,
		OriginalDestination: reply.OriginalDestination,
	}, nil
}

// ── create_mailbox / delete_mailbox / rename_mailbox (Phase D.6) ──

// CreateMailboxOutcomeKind names one variant of the Rust
// `CreateMailboxReply` outcome-tagged enum. The wire-level
// snake_case string is preserved verbatim so future variants land
// additively.
type CreateMailboxOutcomeKind string

const (
	CreateMailboxOutcomeCreated       CreateMailboxOutcomeKind = "created"
	CreateMailboxOutcomeAlreadyExists CreateMailboxOutcomeKind = "already_exists"
	CreateMailboxOutcomeReserved      CreateMailboxOutcomeKind = "reserved"
	CreateMailboxOutcomeInvalidName   CreateMailboxOutcomeKind = "invalid_name"
)

// CreateMailboxOutcome is the typed return value of CreateMailbox.
// `UIDValidity` is set iff `Kind == Created`; `Reason` is set iff
// `Kind == InvalidName`. Callers must switch on `Kind`.
type CreateMailboxOutcome struct {
	Kind        CreateMailboxOutcomeKind
	UIDValidity uint32
	Reason      string
}

// createMailboxRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:CreateMailboxRequest.
type createMailboxRequest struct {
	ActorID []byte `cbor:"actor_id"`
	Name    string `cbor:"name"`
}

// createMailboxReply mirrors the Rust enum `CreateMailboxReply` with
// `#[serde(tag = "outcome", rename_all = "snake_case")]`. Payload
// fields land flat; we decode into a flat struct and let
// `CreateMailbox` route on `Outcome`.
type createMailboxReply struct {
	Outcome     string `cbor:"outcome"`
	UIDValidity uint32 `cbor:"uid_validity,omitempty"`
	Reason      string `cbor:"reason,omitempty"`
}

// CreateMailbox issues `fauna.bridges.create_mailbox` against nest.
// The IMAP wire layer maps the returned outcome to:
//   - `Created`        → `OK CREATE completed`
//   - `AlreadyExists`  → `NO Mailbox already exists`
//   - `Reserved`       → `NO Mailbox name is reserved`
//   - `InvalidName`    → `BAD Invalid mailbox name: <reason>`
//
// Allowlisted server-side for `BridgeMda` only.
func CreateMailbox(ctx context.Context, c Caller, actorID []byte, name string) (CreateMailboxOutcome, error) {
	req := createMailboxRequest{ActorID: actorID, Name: name}
	var reply createMailboxReply
	if err := c.Call(ctx, MethodCreateMailbox, req, &reply); err != nil {
		return CreateMailboxOutcome{}, fmt.Errorf("create_mailbox: %w", err)
	}
	switch reply.Outcome {
	case string(CreateMailboxOutcomeCreated):
		return CreateMailboxOutcome{
			Kind:        CreateMailboxOutcomeCreated,
			UIDValidity: reply.UIDValidity,
		}, nil
	case string(CreateMailboxOutcomeAlreadyExists),
		string(CreateMailboxOutcomeReserved):
		return CreateMailboxOutcome{Kind: CreateMailboxOutcomeKind(reply.Outcome)}, nil
	case string(CreateMailboxOutcomeInvalidName):
		return CreateMailboxOutcome{
			Kind:   CreateMailboxOutcomeInvalidName,
			Reason: reply.Reason,
		}, nil
	default:
		return CreateMailboxOutcome{}, fmt.Errorf("create_mailbox: unknown outcome %q", reply.Outcome)
	}
}

// DeleteMailboxOutcome names one variant of the Rust
// `DeleteMailboxReply` outcome-tagged enum. All variants are unit
// (no payload); a bare string type fits.
type DeleteMailboxOutcome string

const (
	DeleteMailboxOutcomeDeleted       DeleteMailboxOutcome = "deleted"
	DeleteMailboxOutcomeNoSuchMailbox DeleteMailboxOutcome = "no_such_mailbox"
	DeleteMailboxOutcomeReserved      DeleteMailboxOutcome = "reserved"
	DeleteMailboxOutcomeNotEmpty      DeleteMailboxOutcome = "not_empty"
)

// deleteMailboxRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:DeleteMailboxRequest.
type deleteMailboxRequest struct {
	ActorID []byte `cbor:"actor_id"`
	Name    string `cbor:"name"`
}

// deleteMailboxReply mirrors `DeleteMailboxReply` with
// `#[serde(tag = "outcome", rename_all = "snake_case")]`.
type deleteMailboxReply struct {
	Outcome string `cbor:"outcome"`
}

// DeleteMailbox issues `fauna.bridges.delete_mailbox` against nest.
// The IMAP wire layer maps the returned outcome to:
//   - `Deleted`        → `OK DELETE completed`
//   - `NoSuchMailbox`  → `NO Mailbox does not exist`
//   - `Reserved`       → `NO Mailbox is reserved and cannot be deleted`
//   - `NotEmpty`       → `NO Mailbox is not empty`
//
// The non-empty-policy gate (`mail.imap.delete_nonempty`) is read
// server-side from `ImapPolicy::default` — no per-call knob.
// Allowlisted server-side for `BridgeMda` only.
func DeleteMailbox(ctx context.Context, c Caller, actorID []byte, name string) (DeleteMailboxOutcome, error) {
	req := deleteMailboxRequest{ActorID: actorID, Name: name}
	var reply deleteMailboxReply
	if err := c.Call(ctx, MethodDeleteMailbox, req, &reply); err != nil {
		return "", fmt.Errorf("delete_mailbox: %w", err)
	}
	switch reply.Outcome {
	case string(DeleteMailboxOutcomeDeleted),
		string(DeleteMailboxOutcomeNoSuchMailbox),
		string(DeleteMailboxOutcomeReserved),
		string(DeleteMailboxOutcomeNotEmpty):
		return DeleteMailboxOutcome(reply.Outcome), nil
	default:
		return "", fmt.Errorf("delete_mailbox: unknown outcome %q", reply.Outcome)
	}
}

// RenameMailboxOutcomeKind names one variant of the Rust
// `RenameMailboxReply` outcome-tagged enum.
type RenameMailboxOutcomeKind string

const (
	RenameMailboxOutcomeRenamed        RenameMailboxOutcomeKind = "renamed"
	RenameMailboxOutcomeNoSuchSource   RenameMailboxOutcomeKind = "no_such_source"
	RenameMailboxOutcomeReservedSource RenameMailboxOutcomeKind = "reserved_source"
	RenameMailboxOutcomeTargetReserved RenameMailboxOutcomeKind = "target_reserved"
	RenameMailboxOutcomeTargetExists   RenameMailboxOutcomeKind = "target_exists"
	RenameMailboxOutcomeInvalidName    RenameMailboxOutcomeKind = "invalid_name"
)

// RenameMailboxOutcome is the typed return value of RenameMailbox.
// `Reason` is set iff `Kind == InvalidName`. Callers must switch on
// `Kind`.
type RenameMailboxOutcome struct {
	Kind   RenameMailboxOutcomeKind
	Reason string
}

// renameMailboxRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:RenameMailboxRequest.
type renameMailboxRequest struct {
	ActorID []byte `cbor:"actor_id"`
	OldName string `cbor:"old_name"`
	NewName string `cbor:"new_name"`
}

// renameMailboxReply mirrors `RenameMailboxReply` with
// `#[serde(tag = "outcome", rename_all = "snake_case")]`.
type renameMailboxReply struct {
	Outcome string `cbor:"outcome"`
	Reason  string `cbor:"reason,omitempty"`
}

// RenameMailbox issues `fauna.bridges.rename_mailbox` against nest.
// INBOX as `oldName` triggers the RFC 9051 §6.3.6 special-case
// (move contents to a fresh `newName` mailbox, re-seed an empty
// INBOX). The IMAP wire layer maps the returned outcome to:
//   - `Renamed`         → `OK RENAME completed`
//   - `NoSuchSource`    → `NO Source mailbox does not exist`
//   - `ReservedSource`  → `NO Mailbox is reserved and cannot be renamed`
//   - `TargetReserved`  → `NO Target name is reserved`
//   - `TargetExists`    → `NO Target mailbox already exists`
//   - `InvalidName`     → `BAD Invalid mailbox name: <reason>`
//
// Allowlisted server-side for `BridgeMda` only.
func RenameMailbox(ctx context.Context, c Caller, actorID []byte, oldName, newName string) (RenameMailboxOutcome, error) {
	req := renameMailboxRequest{ActorID: actorID, OldName: oldName, NewName: newName}
	var reply renameMailboxReply
	if err := c.Call(ctx, MethodRenameMailbox, req, &reply); err != nil {
		return RenameMailboxOutcome{}, fmt.Errorf("rename_mailbox: %w", err)
	}
	switch reply.Outcome {
	case string(RenameMailboxOutcomeRenamed),
		string(RenameMailboxOutcomeNoSuchSource),
		string(RenameMailboxOutcomeReservedSource),
		string(RenameMailboxOutcomeTargetReserved),
		string(RenameMailboxOutcomeTargetExists):
		return RenameMailboxOutcome{Kind: RenameMailboxOutcomeKind(reply.Outcome)}, nil
	case string(RenameMailboxOutcomeInvalidName):
		return RenameMailboxOutcome{
			Kind:   RenameMailboxOutcomeInvalidName,
			Reason: reply.Reason,
		}, nil
	default:
		return RenameMailboxOutcome{}, fmt.Errorf("rename_mailbox: unknown outcome %q", reply.Outcome)
	}
}

// ── subscribe_mailbox / unsubscribe_mailbox (Phase D.7) ──────────

// subscribeMailboxRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:SubscribeMailboxRequest.
type subscribeMailboxRequest struct {
	ActorID []byte `cbor:"actor_id"`
	Mailbox string `cbor:"mailbox"`
}

// subscribeMailboxReply mirrors `SubscribeMailboxReply` with
// `#[serde(tag = "outcome", rename_all = "snake_case")]`. The sole
// current variant is `subscribed`; future variants would land
// additively.
type subscribeMailboxReply struct {
	Outcome string `cbor:"outcome"`
}

// SubscribeMailbox issues `fauna.bridges.subscribe_mailbox`. Always
// succeeds when the call reaches nest — the reply is informational
// only (idempotent on PK conflict per RFC 9051 §6.3.7, and the
// mailbox need not exist). An error here is transport / RPC-level.
// Allowlisted server-side for `BridgeMda` only.
func SubscribeMailbox(ctx context.Context, c Caller, actorID []byte, mailbox string) error {
	req := subscribeMailboxRequest{ActorID: actorID, Mailbox: mailbox}
	var reply subscribeMailboxReply
	if err := c.Call(ctx, "fauna.bridges.subscribe_mailbox", req, &reply); err != nil {
		return fmt.Errorf("subscribe_mailbox: %w", err)
	}
	if reply.Outcome != "subscribed" {
		return fmt.Errorf("subscribe_mailbox: unknown outcome %q", reply.Outcome)
	}
	return nil
}

// unsubscribeMailboxRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:UnsubscribeMailboxRequest.
type unsubscribeMailboxRequest struct {
	ActorID []byte `cbor:"actor_id"`
	Mailbox string `cbor:"mailbox"`
}

// unsubscribeMailboxReply mirrors `UnsubscribeMailboxReply`.
type unsubscribeMailboxReply struct {
	Outcome string `cbor:"outcome"`
}

// UnsubscribeMailbox issues `fauna.bridges.unsubscribe_mailbox`.
// Idempotent (RFC 9051 §6.3.8 — UNSUBSCRIBE on a not-subscribed
// mailbox succeeds). Allowlisted server-side for `BridgeMda` only.
func UnsubscribeMailbox(ctx context.Context, c Caller, actorID []byte, mailbox string) error {
	req := unsubscribeMailboxRequest{ActorID: actorID, Mailbox: mailbox}
	var reply unsubscribeMailboxReply
	if err := c.Call(ctx, "fauna.bridges.unsubscribe_mailbox", req, &reply); err != nil {
		return fmt.Errorf("unsubscribe_mailbox: %w", err)
	}
	if reply.Outcome != "unsubscribed" {
		return fmt.Errorf("unsubscribe_mailbox: unknown outcome %q", reply.Outcome)
	}
	return nil
}

// ── subscribe_mailbox_state (Phase F.1 — IDLE / NOTIFY push) ─────

// subscribeMailboxStateRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs:SubscribeMailboxStateRequest.
// `ActorID` carries the *served user's* 32-byte identifier — NOT the
// MDA service-user (the MDA actor is inferred server-side from the
// WS-RPC caller identity). See nest's
// `imap-server.md` § IDLE / Push wiring.
type subscribeMailboxStateRequest struct {
	ActorID []byte `cbor:"actor_id"`
	Mailbox string `cbor:"mailbox"`
}

// subscribeMailboxStateReply mirrors `SubscribeMailboxStateReply` with
// `#[serde(tag = "outcome", rename_all = "snake_case")]`. The sole
// current variant is `subscribed { subscription_id }`.
type subscribeMailboxStateReply struct {
	Outcome        string `cbor:"outcome"`
	SubscriptionID uint64 `cbor:"subscription_id"`
}

// SubscribeMailboxState issues `fauna.bridges.subscribe_mailbox_state`
// to register interest in mailbox-state push events for `actorID` /
// `mailbox`. Returns the `subscription_id` nest assigns; the MDA's
// notification router demuxes incoming pushes by this id to dispatch
// each event to the right IMAP session. Subscriptions are per-WS-
// connection-lifetime (`imap-server.md` § IDLE): once the calling MDA's WS connection
// closes, all subscriptions registered on it become inert.
//
// `mailbox` MAY be the empty string to register a NOTIFY-style
// wildcard subscription (open SET) — v1 nest emission only fires on
// concrete-mailbox state changes today, so the wildcard slot is reserved for future
// use. Allowlisted server-side for `BridgeMda` only.
func SubscribeMailboxState(ctx context.Context, c Caller, actorID []byte, mailbox string) (uint64, error) {
	req := subscribeMailboxStateRequest{ActorID: actorID, Mailbox: mailbox}
	var reply subscribeMailboxStateReply
	if err := c.Call(ctx, "fauna.bridges.subscribe_mailbox_state", req, &reply); err != nil {
		return 0, fmt.Errorf("subscribe_mailbox_state: %w", err)
	}
	if reply.Outcome != "subscribed" {
		return 0, fmt.Errorf("subscribe_mailbox_state: unknown outcome %q", reply.Outcome)
	}
	return reply.SubscriptionID, nil
}

// BridgeMailboxStatePushKind is the `Push.Kind` string nest emits for
// IDLE/NOTIFY mailbox-state pushes. The wsrpc client's `OnPush`
// callback (per `client.go`) sees this kind exactly once per
// matching subscription per mutation, with `Payload` as the
// canonical-CBOR encoding of `BridgeMailboxStatePush`.
const BridgeMailboxStatePushKind = "fauna.bridges.push.mailbox_state"

// MailboxStateEventKind discriminates between the four event
// variants inside `BridgeMailboxStatePush.Event`. Matches the Rust
// `#[serde(tag = "kind", rename_all = "snake_case")]` discriminator
// on `MailboxStateEvent`.
type MailboxStateEventKind string

const (
	MailboxStateEventAppend  MailboxStateEventKind = "append"
	MailboxStateEventFlags   MailboxStateEventKind = "flags"
	MailboxStateEventExpunge MailboxStateEventKind = "expunge"
	MailboxStateEventMove    MailboxStateEventKind = "move"
)

// MailboxStateEvent mirrors
// libs/fauna-protocol/src/bridge_routing.rs:MailboxStateEvent. The
// internally-tagged enum decodes into a single Go struct with all
// possible fields nullable / zeroed — the consumer inspects `Kind`
// to decide which fields are meaningful:
//
//   - Append  → Uid, Flags, Modseq
//   - Flags   → Uid, Flags, Modseq
//   - Expunge → Uid, Modseq
//   - Move    → SrcUid, DstUid, ModseqSrc, ModseqDst, Side
//
// Fields that don't apply to the current `Kind` are zero-valued and
// MUST NOT be relied on by the consumer.
type MailboxStateEvent struct {
	Kind      MailboxStateEventKind `cbor:"kind"`
	Uid       uint32                `cbor:"uid,omitempty"`
	Flags     []string              `cbor:"flags,omitempty"`
	Modseq    int64                 `cbor:"modseq,omitempty"`
	SrcUid    uint32                `cbor:"src_uid,omitempty"`
	DstUid    uint32                `cbor:"dst_uid,omitempty"`
	ModseqSrc int64                 `cbor:"modseq_src,omitempty"`
	ModseqDst int64                 `cbor:"modseq_dst,omitempty"`
	// Side is required on a Move (the nest always names it); no
	// omitempty — a Move that reaches the MDA without it is malformed.
	Side MoveSide `cbor:"side"`
}

// MoveSide mirrors libs/fauna-protocol/src/bridge_routing.rs:MoveSide —
// which end of a Move the receiving subscription's mailbox is. IMAP UIDs
// are per-mailbox, so the side cannot be recovered from the UIDs.
type MoveSide string

const (
	MoveSideSource      MoveSide = "source"
	MoveSideDestination MoveSide = "destination"
)

// BridgeMailboxStatePush mirrors
// libs/fauna-protocol/src/bridge_routing.rs:BridgeMailboxStatePush —
// the typed payload nest sends under `Push.Kind =
// BridgeMailboxStatePushKind`. Consumers decode `PushFrame.Payload`
// (which is canonical-CBOR `BridgeMailboxStatePush`) via
// `dagcbor.Unmarshal[BridgeMailboxStatePush](frame.Payload)` and
// demux to the right IMAP session by `SubscriptionID`.
type BridgeMailboxStatePush struct {
	SubscriptionID uint64            `cbor:"subscription_id"`
	ActorID        []byte            `cbor:"actor_id"`
	Mailbox        string            `cbor:"mailbox"`
	Event          MailboxStateEvent `cbor:"event"`
}

// ── provision_calendar / list_calendars (Phase E.2 CalDAV MDA) ───
//
// Mirrors libs/fauna-protocol/src/bridge_routing.rs
// ProvisionCalendarRequest / ProvisionCalendarReply and
// ListCalendarsRequest / ListCalendarsReply. The MDA-class caller
// (per bridge_method_allowlist.rs) wires these to the
// lazy-"Personal" calendar flow and the CalDAV PROPFIND read
// surface.

// provisionCalendarRequest mirrors ProvisionCalendarRequest. The
// `update_metadata` bool routes between the MKCOL insert path
// (`false`) and the PROPPATCH overwrite path (`true`); on the wire
// the field is always present, and the Rust side's
// `#[serde(default)]` keeps legacy-encoded bodies (no field) decoding
// to `false` for forward compat.
type provisionCalendarRequest struct {
	ActorID           []byte `cbor:"actor_id"`
	CalendarID        []byte `cbor:"calendar_id"`
	EncryptedMetadata []byte `cbor:"encrypted_metadata"`
	UpdateMetadata    bool   `cbor:"update_metadata"`
}

// provisionCalendarReply mirrors ProvisionCalendarReply.
type provisionCalendarReply struct {
	Outcome string `cbor:"outcome"`
}

// ProvisionCalendarOutcome is the typed result of a ProvisionCalendar
// call. The first three variants (`created`, `already_exists`,
// `conflict`) fire on the MKCOL path (`updateMetadata=false`); the
// last two (`updated`, `not_found`) fire on the PROPPATCH overwrite
// path (`updateMetadata=true`). Nest dispatches between the two
// branches based on the request bool.
type ProvisionCalendarOutcome string

const (
	ProvisionCalendarCreated       ProvisionCalendarOutcome = "created"
	ProvisionCalendarAlreadyExists ProvisionCalendarOutcome = "already_exists"
	ProvisionCalendarConflict      ProvisionCalendarOutcome = "conflict"
	ProvisionCalendarUpdated       ProvisionCalendarOutcome = "updated"
	ProvisionCalendarNotFound      ProvisionCalendarOutcome = "not_found"
)

// ProvisionCalendar issues `fauna.bridges.provision_calendar` for
// the given (actor, calendar_id) tuple. `updateMetadata=false` is the
// MKCOL / lazy-Personal insert path — idempotent on byte-identical
// `encryptedMetadata`, returns `Conflict` on byte-different retry.
// `updateMetadata=true` is the PROPPATCH overwrite path per goal doc
// § Write surface row 104: nest overwrites the existing
// `encrypted_metadata` column, bumps `highestmodseq`, and returns
// `Updated` (or `NotFound` if the row doesn't exist). Allowlisted
// server-side for `BridgeMda` (and `User`-class direct callers).
func ProvisionCalendar(
	ctx context.Context,
	c Caller,
	actorID, calendarID, encryptedMetadata []byte,
	updateMetadata bool,
) (ProvisionCalendarOutcome, error) {
	req := provisionCalendarRequest{
		ActorID:           actorID,
		CalendarID:        calendarID,
		EncryptedMetadata: encryptedMetadata,
		UpdateMetadata:    updateMetadata,
	}
	var reply provisionCalendarReply
	if err := c.Call(ctx, MethodProvisionCalendar, req, &reply); err != nil {
		return "", fmt.Errorf("provision_calendar: %w", err)
	}
	switch ProvisionCalendarOutcome(reply.Outcome) {
	case ProvisionCalendarCreated, ProvisionCalendarAlreadyExists, ProvisionCalendarConflict,
		ProvisionCalendarUpdated, ProvisionCalendarNotFound:
		return ProvisionCalendarOutcome(reply.Outcome), nil
	default:
		return "", fmt.Errorf("provision_calendar: unknown outcome %q", reply.Outcome)
	}
}

// placeInboundInviteRequest mirrors
// libs/fauna-protocol/src/bridge_routing.rs PlaceInboundInviteRequest.
type placeInboundInviteRequest struct {
	ActorID            []byte `cbor:"actor_id"`
	UIDHash            []byte `cbor:"uid_hash"`
	EncryptedBody      []byte `cbor:"encrypted_body"`
	EncryptedIndexHint []byte `cbor:"encrypted_index_hint"`
	Timestamp          int64  `cbor:"timestamp"`
	SenderAddress      string `cbor:"sender_address"`
}

// placeInboundInviteReply mirrors PlaceInboundInviteReply.
type placeInboundInviteReply struct {
	Outcome string `cbor:"outcome"`
}

// PlaceInboundInviteOutcome is the typed result of PlaceInboundInvite. An
// outcome this bridge does not know (a newer nest) is returned verbatim, not
// an error: placement is best-effort and the mail is already delivered.
type PlaceInboundInviteOutcome string

const (
	PlaceInboundInvitePlaced            PlaceInboundInviteOutcome = "placed"
	PlaceInboundInviteAlreadyOnCalendar PlaceInboundInviteOutcome = "already_on_calendar"
	PlaceInboundInviteWithheld          PlaceInboundInviteOutcome = "withheld"
	// The recipient's storage is full, so the invitation was not placed
	// (caldav-server.md § QUOTA → § Enforcement points); the mail copy is
	// delivered all the same.
	PlaceInboundInviteOverQuota PlaceInboundInviteOutcome = "over_quota"
)

// PlaceInboundInvite issues `fauna.bridges.place_inbound_invite` — the MTA
// places an invitation that arrived by email, already sealed to the recipient,
// on their calendar (caldav-server.md § Server-side auto-schedule, "Inbound
// invite"). nest keeps it create-only and re-derives the guardian mail verdict
// from `senderAddress`. Allowlisted server-side for `BridgeMta` only.
func PlaceInboundInvite(
	ctx context.Context,
	c Caller,
	actorID, uidHash, encryptedBody, encryptedIndexHint []byte,
	timestamp int64,
	senderAddress string,
) (PlaceInboundInviteOutcome, error) {
	req := placeInboundInviteRequest{
		ActorID:            actorID,
		UIDHash:            uidHash,
		EncryptedBody:      encryptedBody,
		EncryptedIndexHint: encryptedIndexHint,
		Timestamp:          timestamp,
		SenderAddress:      senderAddress,
	}
	var reply placeInboundInviteReply
	if err := c.Call(ctx, MethodPlaceInboundInvite, req, &reply); err != nil {
		return "", fmt.Errorf("place_inbound_invite: %w", err)
	}
	return PlaceInboundInviteOutcome(reply.Outcome), nil
}

// listCalendarsRequest mirrors ListCalendarsRequest.
type listCalendarsRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// CalendarEntry mirrors libs/fauna-protocol/src/bridge_routing.rs
// CalendarEntry. `EncryptedMetadata` is the sealed-to-recipient blob
// the MDA decrypts in-session for the AUTH'd actor; nest treats it
// as opaque bytes.
type CalendarEntry struct {
	CalendarID        []byte `cbor:"calendar_id"`
	EncryptedMetadata []byte `cbor:"encrypted_metadata"`
	CTag              int64  `cbor:"ctag"`
	HighestModseq     int64  `cbor:"highestmodseq"`
	EventCount        uint32 `cbor:"event_count"`
	CreatedAt         int64  `cbor:"created_at"`
}

// listCalendarsReply mirrors ListCalendarsReply.
type listCalendarsReply struct {
	Calendars []CalendarEntry `cbor:"calendars"`
}

// ListCalendars issues `fauna.bridges.list_calendars` for an actor.
// Empty reply is normal — the actor simply has no provisioned
// calendars (the CalDAV lazy-Personal path triggers off that).
// Allowlisted server-side for `BridgeMda` only.
func ListCalendars(ctx context.Context, c Caller, actorID []byte) ([]CalendarEntry, error) {
	req := listCalendarsRequest{ActorID: actorID}
	var reply listCalendarsReply
	if err := c.Call(ctx, MethodListCalendars, req, &reply); err != nil {
		return nil, fmt.Errorf("list_calendars: %w", err)
	}
	return reply.Calendars, nil
}

// ── put_event_ciphertext / delete_event (Phase E.3 CalDAV PUT/DELETE) ──
//
// Mirrors libs/fauna-protocol/src/bridge_routing.rs
// PutEventCiphertextRequest / PutEventCiphertextReply and
// DeleteEventRequest / DeleteEventReply. Allowlisted server-side for
// BridgeMda only (see bins/fauna-nest/src/bridge_method_allowlist.rs).

// putEventCiphertextRequest mirrors PutEventCiphertextRequest.
// `IfMatch` is `*string` so absent (`None` on the wire) and empty
// string are distinguishable — CalDAV `If-Match: *` is normalized to
// absent on the bridge per the goal doc § Write surface.
type putEventCiphertextRequest struct {
	ActorID            []byte  `cbor:"actor_id"`
	CalendarID         []byte  `cbor:"calendar_id"`
	UIDHash            []byte  `cbor:"uid_hash"`
	EncryptedBody      []byte  `cbor:"encrypted_body"`
	EncryptedIndexHint []byte  `cbor:"encrypted_index_hint"`
	Timestamp          int64   `cbor:"timestamp"`
	CiphertextSize     uint32  `cbor:"ciphertext_size"`
	IfMatch            *string `cbor:"if_match"`
	// EncryptedFaunaExt mirrors Rust `PutEventCiphertextRequest.encrypted_fauna_ext`
	// (Option<Vec<u8>>) — the Fauna-only sidecar. The MDA NEVER writes a sidecar
	// (a MUA PUT carries none), so this stays nil ⇒ omitted ⇒ Rust `None` ⇒ the
	// nest PRESERVES the prior sidecar on UPDATE (caldav-server.md § Event
	// resources). `*[]byte` not `[]byte`: a nil bare slice would encode as empty
	// bytes (= Some(empty)) under NilContainersAsEmpty; a nil pointer encodes as
	// CBOR null / is omitted — preserving the Option None semantics.
	EncryptedFaunaExt *[]byte `cbor:"encrypted_fauna_ext,omitempty"`
}

// putEventCiphertextReply flattens the Rust tagged union — every
// possible payload field rides in the same map, gated by `Outcome`.
// `EventID`/`ETag`/`Modseq` are populated on Created/Updated;
// `CurrentETag` on PreconditionFailed; nothing extra on
// CalendarNotFound.
type putEventCiphertextReply struct {
	Outcome     string `cbor:"outcome"`
	EventID     []byte `cbor:"event_id,omitempty"`
	ETag        string `cbor:"etag,omitempty"`
	Modseq      int64  `cbor:"modseq,omitempty"`
	CurrentETag string `cbor:"current_etag,omitempty"`
}

// PutEventCiphertextOutcome is the externally-meaningful classification
// of a `put_event_ciphertext` reply. Maps 1:1 to PutEventCiphertextReply
// variants on the Rust side.
type PutEventCiphertextOutcome string

const (
	PutEventCreated            PutEventCiphertextOutcome = "created"
	PutEventUpdated            PutEventCiphertextOutcome = "updated"
	PutEventPreconditionFailed PutEventCiphertextOutcome = "precondition_failed"
	PutEventCalendarNotFound   PutEventCiphertextOutcome = "calendar_not_found"
)

// PutEventResult is the typed payload of a successful put_event_ciphertext
// call. On `PutEventCreated` / `PutEventUpdated`, `EventID`, `ETag`,
// `Modseq` are populated. On `PutEventPreconditionFailed`, `CurrentETag`
// carries the row's current etag (so the bridge can surface it in the
// HTTP response). On `PutEventCalendarNotFound`, all fields are zero.
type PutEventResult struct {
	Outcome     PutEventCiphertextOutcome
	EventID     []byte
	ETag        string
	Modseq      int64
	CurrentETag string
}

// PutEventCiphertext issues `fauna.bridges.put_event_ciphertext`.
// The MDA calls this from CalDAV PUT after parsing + validating the
// iCalendar body, blake3-hashing the plaintext UID, and HPKE-sealing
// the raw body + tokenized index hint to the AUTH'd actor's MLS
// pubkey + index key. `ifMatch` is `nil` for unconditional PUT,
// `&etag` for conditional PUT (CalDAV `If-Match` header). Allowlisted
// server-side for `BridgeMda` only.
func PutEventCiphertext(
	ctx context.Context,
	c Caller,
	actorID, calendarID, uidHash, encryptedBody, encryptedIndexHint []byte,
	timestamp int64,
	ciphertextSize uint32,
	ifMatch *string,
) (PutEventResult, error) {
	return PutEventCiphertextCarrying(ctx, c, actorID, calendarID, uidHash,
		encryptedBody, encryptedIndexHint, timestamp, ciphertextSize, ifMatch, nil)
}

// PutEventCiphertextCarrying is PutEventCiphertext for a write that carries an
// event's existing Fauna sidecar along with it — the MDA's MOVE/COPY, which
// relocates a stored event's ciphertext as-is and must not drop the Fauna-only
// refinement riding beside it (caldav-server.md § Event resources). The MDA
// never MINTS a sidecar; `encryptedFaunaExt` is only ever one it read back. nil
// ⇒ omitted ⇒ the nest's preserve-on-update semantics, exactly as a PUT.
func PutEventCiphertextCarrying(
	ctx context.Context,
	c Caller,
	actorID, calendarID, uidHash, encryptedBody, encryptedIndexHint []byte,
	timestamp int64,
	ciphertextSize uint32,
	ifMatch *string,
	encryptedFaunaExt *[]byte,
) (PutEventResult, error) {
	req := putEventCiphertextRequest{
		ActorID:            actorID,
		CalendarID:         calendarID,
		UIDHash:            uidHash,
		EncryptedBody:      encryptedBody,
		EncryptedIndexHint: encryptedIndexHint,
		Timestamp:          timestamp,
		CiphertextSize:     ciphertextSize,
		IfMatch:            ifMatch,
		EncryptedFaunaExt:  encryptedFaunaExt,
	}
	var reply putEventCiphertextReply
	if err := c.Call(ctx, MethodPutEventCiphertext, req, &reply); err != nil {
		return PutEventResult{}, fmt.Errorf("put_event_ciphertext: %w", err)
	}
	switch PutEventCiphertextOutcome(reply.Outcome) {
	case PutEventCreated, PutEventUpdated:
		return PutEventResult{
			Outcome: PutEventCiphertextOutcome(reply.Outcome),
			EventID: reply.EventID,
			ETag:    reply.ETag,
			Modseq:  reply.Modseq,
		}, nil
	case PutEventPreconditionFailed:
		return PutEventResult{
			Outcome:     PutEventPreconditionFailed,
			CurrentETag: reply.CurrentETag,
		}, nil
	case PutEventCalendarNotFound:
		return PutEventResult{Outcome: PutEventCalendarNotFound}, nil
	default:
		return PutEventResult{}, fmt.Errorf("put_event_ciphertext: unknown outcome %q", reply.Outcome)
	}
}

// deleteEventRequest mirrors DeleteEventRequest.
type deleteEventRequest struct {
	ActorID    []byte  `cbor:"actor_id"`
	CalendarID []byte  `cbor:"calendar_id"`
	UIDHash    []byte  `cbor:"uid_hash"`
	IfMatch    *string `cbor:"if_match"`
}

// deleteEventReply flattens the Rust tagged union per the same
// pattern as putEventCiphertextReply.
type deleteEventReply struct {
	Outcome     string `cbor:"outcome"`
	EventID     []byte `cbor:"event_id,omitempty"`
	Modseq      int64  `cbor:"modseq,omitempty"`
	CurrentETag string `cbor:"current_etag,omitempty"`
}

// DeleteEventOutcome is the externally-meaningful classification of a
// `delete_event` reply.
type DeleteEventOutcome string

const (
	DeleteEventDeleted            DeleteEventOutcome = "deleted"
	DeleteEventNotFound           DeleteEventOutcome = "not_found"
	DeleteEventPreconditionFailed DeleteEventOutcome = "precondition_failed"
)

// DeleteEventResult is the typed payload of a successful delete_event
// call. On `DeleteEventDeleted`, `EventID` and `Modseq` are
// populated. On `DeleteEventPreconditionFailed`, `CurrentETag`
// carries the row's current etag. On `DeleteEventNotFound`, all
// fields are zero (Decision 7: the missing-event and missing-calendar
// paths are intentionally collapsed).
type DeleteEventResult struct {
	Outcome     DeleteEventOutcome
	EventID     []byte
	Modseq      int64
	CurrentETag string
}

// DeleteEvent issues `fauna.bridges.delete_event`. The MDA calls
// this from CalDAV DELETE. `ifMatch` is `nil` for unconditional
// delete, `&etag` for conditional (CalDAV `If-Match` header).
// Allowlisted server-side for `BridgeMda` only.
func DeleteEvent(
	ctx context.Context,
	c Caller,
	actorID, calendarID, uidHash []byte,
	ifMatch *string,
) (DeleteEventResult, error) {
	req := deleteEventRequest{
		ActorID:    actorID,
		CalendarID: calendarID,
		UIDHash:    uidHash,
		IfMatch:    ifMatch,
	}
	var reply deleteEventReply
	if err := c.Call(ctx, MethodDeleteEvent, req, &reply); err != nil {
		return DeleteEventResult{}, fmt.Errorf("delete_event: %w", err)
	}
	switch DeleteEventOutcome(reply.Outcome) {
	case DeleteEventDeleted:
		return DeleteEventResult{
			Outcome: DeleteEventDeleted,
			EventID: reply.EventID,
			Modseq:  reply.Modseq,
		}, nil
	case DeleteEventNotFound:
		return DeleteEventResult{Outcome: DeleteEventNotFound}, nil
	case DeleteEventPreconditionFailed:
		return DeleteEventResult{
			Outcome:     DeleteEventPreconditionFailed,
			CurrentETag: reply.CurrentETag,
		}, nil
	default:
		return DeleteEventResult{}, fmt.Errorf("delete_event: unknown outcome %q", reply.Outcome)
	}
}

// ── query_events / sync_calendar_since (Phase E.3 CalDAV REPORT) ──
//
// Mirrors libs/fauna-protocol/src/bridge_routing.rs
// QueryEventsRequest / QueryEventsReply (Phase D.3) and
// SyncCalendarSinceRequest / SyncCalendarSinceReply (Phase D.6).
// Both are allowlisted server-side for BridgeMda only (see
// bins/fauna-nest/src/bridge_method_allowlist.rs).

// EventEntry mirrors libs/fauna-protocol/src/bridge_routing.rs
// `EventEntry` — one sealed event row. `EncryptedBody` /
// `EncryptedIndexHint` are HPKE-sealed to the AUTH'd actor's MLS
// pubkey + index pubkey respectively; the MDA opens them in-session
// via `MlsCapability.OpenMailRecord`.
type EventEntry struct {
	EventID            []byte `cbor:"event_id"`
	UIDHash            []byte `cbor:"uid_hash"`
	EncryptedBody      []byte `cbor:"encrypted_body"`
	EncryptedIndexHint []byte `cbor:"encrypted_index_hint"`
	ETag               string `cbor:"etag"`
	Modseq             int64  `cbor:"modseq"`
	CiphertextSize     uint32 `cbor:"ciphertext_size"`
	InternalDate       int64  `cbor:"internal_date"`
	// EncryptedFaunaExt mirrors Rust `EventEntry.encrypted_fauna_ext`. Fauna
	// apps read it for the asymmetric `interested↔TENTATIVE` projection; the
	// MDA NEVER decrypts or serves it to a CalDAV MUA (the canonical VEVENT in
	// EncryptedBody is the only thing a MUA receives — caldav-server.md § Event
	// resources). Present only on Fauna-written events; nil otherwise.
	EncryptedFaunaExt *[]byte `cbor:"encrypted_fauna_ext,omitempty"`
}

// ExpungedEntry mirrors libs/fauna-protocol/src/bridge_routing.rs
// `ExpungedEntry` — one deletion tombstone. RFC 6578 VANISHED in
// CalDAV sync-collection reports.
type ExpungedEntry struct {
	EventID []byte `cbor:"event_id"`
	UIDHash []byte `cbor:"uid_hash"`
	Modseq  int64  `cbor:"modseq"`
}

// queryEventsRequest mirrors QueryEventsRequest. `SinceModseq` /
// `AfterEventID` are pointer-nullable so absent (`None` on the wire)
// and zero are distinguishable. `AfterEventID` is `*[]byte` (not a bare
// `[]byte`) specifically because the codec runs NilContainersAsEmpty
// (internal/dagcbor/codec.go): a nil *bare slice* would encode as an
// empty byte string (0x40 = `Some(empty)`), corrupting the
// `Option<ByteBuf>` None = "fresh page" semantics. A nil *pointer* still
// encodes as CBOR null = `None` regardless of NilContainersAsEmpty.
type queryEventsRequest struct {
	ActorID      []byte  `cbor:"actor_id"`
	CalendarID   []byte  `cbor:"calendar_id"`
	SinceModseq  *int64  `cbor:"since_modseq"`
	AfterEventID *[]byte `cbor:"after_event_id"`
	Limit        uint32  `cbor:"limit"`
}

// queryEventsReply flattens the Rust tagged union: every payload
// field rides in the same map, gated by `Outcome`. `Events` /
// `HighestModseq` / `More` are populated on `Ok`; nothing extra on
// `CalendarNotFound`.
type queryEventsReply struct {
	Outcome       string       `cbor:"outcome"`
	Events        []EventEntry `cbor:"events,omitempty"`
	HighestModseq int64        `cbor:"highestmodseq,omitempty"`
	More          bool         `cbor:"more,omitempty"`
}

// QueryEventsOutcome is the externally-meaningful classification of
// a `query_events` reply.
type QueryEventsOutcome string

const (
	QueryEventsOk               QueryEventsOutcome = "ok"
	QueryEventsCalendarNotFound QueryEventsOutcome = "calendar_not_found"
)

// QueryEventsResult is the typed payload of a successful query_events
// call. On `QueryEventsOk`, `Events` / `HighestModseq` / `More` are
// populated. On `QueryEventsCalendarNotFound`, all fields are zero.
type QueryEventsResult struct {
	Outcome       QueryEventsOutcome
	Events        []EventEntry
	HighestModseq int64
	More          bool
}

// QueryEvents issues `fauna.bridges.query_events`. The MDA calls
// this from CalDAV REPORT calendar-query / multiget — nest returns
// every event in the calendar (or the paginated subset since
// `sinceModseq`); the MDA decrypts in-session, expands recurrence
// locally via `mailfauna.ExpandRecurrence`, and filters by the
// REPORT's `<time-range>` window.
//
//   - `sinceModseq` = `nil` for full calendar-query / multiget; `&n`
//     for CONDSTORE incremental (events with `modseq > n` only).
//   - `afterEventID` = `nil` to start a fresh page; the prior reply's
//     `events.last().EventID` to resume.
//   - `limit` = 0 for unbounded (whole calendar in one reply).
func QueryEvents(
	ctx context.Context,
	c Caller,
	actorID, calendarID []byte,
	sinceModseq *int64,
	afterEventID []byte,
	limit uint32,
) (QueryEventsResult, error) {
	// nil afterEventID = "start a fresh page" → leave the pointer nil so it
	// encodes as CBOR null (`None`); a present cursor encodes as a byte
	// string (`Some`). See queryEventsRequest's doc on why this is a
	// pointer rather than a bare []byte under NilContainersAsEmpty.
	var afterEventIDPtr *[]byte
	if afterEventID != nil {
		afterEventIDPtr = &afterEventID
	}
	req := queryEventsRequest{
		ActorID:      actorID,
		CalendarID:   calendarID,
		SinceModseq:  sinceModseq,
		AfterEventID: afterEventIDPtr,
		Limit:        limit,
	}
	var reply queryEventsReply
	if err := c.Call(ctx, MethodQueryEvents, req, &reply); err != nil {
		return QueryEventsResult{}, fmt.Errorf("query_events: %w", err)
	}
	switch QueryEventsOutcome(reply.Outcome) {
	case QueryEventsOk:
		return QueryEventsResult{
			Outcome:       QueryEventsOk,
			Events:        reply.Events,
			HighestModseq: reply.HighestModseq,
			More:          reply.More,
		}, nil
	case QueryEventsCalendarNotFound:
		return QueryEventsResult{Outcome: QueryEventsCalendarNotFound}, nil
	default:
		return QueryEventsResult{}, fmt.Errorf("query_events: unknown outcome %q", reply.Outcome)
	}
}

// syncCalendarSinceRequest mirrors SyncCalendarSinceRequest. The
// `sync_token` field is intentionally a string on the wire (RFC 6578
// §3.1) even though its concrete representation is a decimal modseq;
// `"0"` performs a full sync.
type syncCalendarSinceRequest struct {
	ActorID    []byte  `cbor:"actor_id"`
	CalendarID []byte  `cbor:"calendar_id"`
	SyncToken  string  `cbor:"sync_token"`
	Limit      uint32  `cbor:"limit"`
	MuaID      *string `cbor:"mua_id,omitempty"`
}

// syncCalendarSinceReply flattens the tagged union. `Changed` /
// `Expunged` / `NewSyncToken` / `More` / `Stale` populate on `Ok`;
// nothing extra on `CalendarNotFound`; `ServerModseq` populates on
// `Stale` (the variant, distinct from the `Ok` `stale` flag).
type syncCalendarSinceReply struct {
	Outcome      string          `cbor:"outcome"`
	Changed      []EventEntry    `cbor:"changed,omitempty"`
	Expunged     []ExpungedEntry `cbor:"expunged,omitempty"`
	NewSyncToken string          `cbor:"new_sync_token,omitempty"`
	More         bool            `cbor:"more,omitempty"`
	// Stale on an `Ok` reply means the supplied sync-token is valid but
	// predates the tombstone-retention window (SyncCalendarSinceReply::Ok
	// `stale` field); the MDA emits DAV:valid-sync-token. Distinct from
	// the `Stale` *outcome* (MUA-ahead). Defaults false.
	Stale        bool  `cbor:"stale,omitempty"`
	ServerModseq int64 `cbor:"server_modseq,omitempty"`
}

// SyncCalendarSinceOutcome is the externally-meaningful
// classification of a `sync_calendar_since` reply.
type SyncCalendarSinceOutcome string

const (
	SyncCalendarSinceOk               SyncCalendarSinceOutcome = "ok"
	SyncCalendarSinceCalendarNotFound SyncCalendarSinceOutcome = "calendar_not_found"
	// SyncCalendarSinceStale is returned when the client's sync-token is
	// ahead of the calendar's current highestmodseq — the post-DR-restore
	// case (spec § D6 (γ)). Callers should fall through to a full PROPFIND.
	SyncCalendarSinceStale SyncCalendarSinceOutcome = "stale"
)

// SyncCalendarSinceResult is the typed payload of a sync_calendar_since call.
type SyncCalendarSinceResult struct {
	Outcome      SyncCalendarSinceOutcome
	Changed      []EventEntry
	Expunged     []ExpungedEntry
	NewSyncToken string
	More         bool
	// Stale is set when Outcome == SyncCalendarSinceOk AND the supplied
	// sync-token predates the tombstone-retention window (caldav-server.md
	// § Stale sync-token handling). The MDA treats it like the
	// SyncCalendarSinceStale outcome — emit DAV:valid-sync-token — but it
	// is a distinct nest-side condition (no restore-divergence row).
	Stale bool
	// ServerModseq is populated when Outcome == SyncCalendarSinceStale.
	// Zero for all other outcomes.
	ServerModseq int64
}

// SyncCalendarSince issues `fauna.bridges.sync_calendar_since`. The
// MDA calls this from CalDAV REPORT sync-collection. `syncToken` =
// `"0"` (or empty, normalized to `"0"`) is full sync; any other
// value is a decimal modseq returned by a prior call. `limit` = 0
// is unbounded (tombstones are never paginated regardless).
// `muaID` is the MUA's User-Agent string (empty if not provided);
// forwarded to nest for divergence-logging purposes.
func SyncCalendarSince(
	ctx context.Context,
	c Caller,
	actorID, calendarID []byte,
	syncToken string,
	limit uint32,
	muaID string,
) (SyncCalendarSinceResult, error) {
	if syncToken == "" {
		syncToken = "0"
	}
	req := syncCalendarSinceRequest{
		ActorID:    actorID,
		CalendarID: calendarID,
		SyncToken:  syncToken,
		Limit:      limit,
	}
	if muaID != "" {
		req.MuaID = &muaID
	}
	var reply syncCalendarSinceReply
	if err := c.Call(ctx, MethodSyncCalendarSince, req, &reply); err != nil {
		return SyncCalendarSinceResult{}, fmt.Errorf("sync_calendar_since: %w", err)
	}
	switch SyncCalendarSinceOutcome(reply.Outcome) {
	case SyncCalendarSinceOk:
		return SyncCalendarSinceResult{
			Outcome:      SyncCalendarSinceOk,
			Changed:      reply.Changed,
			Expunged:     reply.Expunged,
			NewSyncToken: reply.NewSyncToken,
			More:         reply.More,
			Stale:        reply.Stale,
		}, nil
	case SyncCalendarSinceCalendarNotFound:
		return SyncCalendarSinceResult{Outcome: SyncCalendarSinceCalendarNotFound}, nil
	case SyncCalendarSinceStale:
		return SyncCalendarSinceResult{
			Outcome:      SyncCalendarSinceStale,
			ServerModseq: reply.ServerModseq,
		}, nil
	default:
		return SyncCalendarSinceResult{}, fmt.Errorf("sync_calendar_since: unknown outcome %q", reply.Outcome)
	}
}

// ── WebDAV (`bridge webdav_*`) — the files terminator's data plane ──
//
// Every type mirrors libs/fauna-protocol/src/wrapped_blob.rs (DAG-CBOR, 32-byte
// ids via serde_bytes → []byte). Unlike CalDAV/CardDAV, WebDAV is a *view over
// the existing folder substrate* — no mirror store; a write is recorded as an
// ordinary folder change. All five are allowlisted server-side for BridgeMda.

// BulkByteAccess mirrors the Rust `BulkByteAccess` enum (serde snake_case). The
// MDA mints a `read` token for downloads (though those routes are open) and a
// `write` token for chunk/manifest uploads; a `read` token on a write route is
// a hard 403 (webdav-server.md § Bulk-byte plane).
const (
	BulkByteAccessRead  = "read"
	BulkByteAccessWrite = "write"
)

// fetchWebdavKeysBlobRequest mirrors FetchWebdavKeysBlobRequest.
type fetchWebdavKeysBlobRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// fetchWebdavKeysBlobReply mirrors FetchWebdavKeysBlobReply. `Blob` is nil when
// the actor has no provisioned served-set key blob (AUTH still succeeds; the
// per-set unseal surfaces the missing-keys error at serve time, fail-closed).
type fetchWebdavKeysBlobReply struct {
	Blob []byte `cbor:"blob"`
}

// FetchWebdavKeysBlob fetches the MSEK-sealed WebdavKeysBlob for an actor. The
// MDA calls this once per AUTH'd session and unseals it under the session's MLS
// capability (the MSEK never leaves the capability). nil = not provisioned.
func FetchWebdavKeysBlob(ctx context.Context, c Caller, actorID []byte) ([]byte, error) {
	req := fetchWebdavKeysBlobRequest{ActorID: actorID}
	var reply fetchWebdavKeysBlobReply
	if err := c.Call(ctx, MethodFetchWebdavKeysBlob, req, &reply); err != nil {
		return nil, fmt.Errorf("fetch_webdav_keys_blob: %w", err)
	}
	return reply.Blob, nil
}

// BulkByteMintPurpose mirrors the Rust `BulkByteMintPurpose` enum (serde
// snake_case). It selects which gate the nest applies to the mint — the byte
// routes themselves are purpose-blind, because the chunk store is a single
// global content-addressed store and the token is explicitly "not a chunk-hash
// ACL" (webdav-server.md § Bulk-byte plane): confidentiality is cryptographic,
// not transport-scoped. What the purpose decides is *who may ask*:
//
//   - Folder  → BridgeMda only; gate = the set is owned, non-reserved and served.
//   - MailBody → BridgeMta or BridgeMda; gate = the target actor is a mail
//     recipient on this nest (mail belongs to no folder, which is why
//     widening the folder gate would have been meaningless). An MTA still
//     may NOT mint a Folder token — admitting it to the byte plane for mail
//     did not open the WebDAV path to it.
//   - IndexSegment → BridgeMda only; gate = the same recipient-seal-key check
//     MailBody uses, because the MDA's *index* reach is exactly its *mail*
//     reach (key-material-hierarchy.md rule #7). Narrowed to the MDA because
//     only it is a ratified index-builder position (content-index.md
//     § Architectural rules #3); an MTA never builds an index.
//
// Omitted on the wire when Folder (the Rust default), so the existing WebDAV
// mint encodes byte-identically to its pre-field shape.
//
// ForeignFolderWrite is the third Rust variant. It is minted SOLELY by the
// nest's `fauna.federation.folder.write_token.mint` federation handler for a
// cross-nest writer — never by a bridge, which the nest's mint handler refuses
// outright. Mirrored here only to keep this enum faithful to the Rust source;
// this bridge never sends it. ForeignFolderRead is its read-scoped twin
// (`fauna.federation.folder.read_token.mint`), mirrored for the same reason
// and refused by the nest's mint handler the same way.
const (
	BulkByteMintPurposeFolder             = "folder"
	BulkByteMintPurposeMailBody           = "mail_body"
	BulkByteMintPurposeIndexSegment       = "index_segment"
	BulkByteMintPurposeForeignFolderWrite = "foreign_folder_write"
	BulkByteMintPurposeForeignFolderRead  = "foreign_folder_read"
)

// mintBulkByteTokenRequest mirrors MintBulkByteTokenRequest.
type mintBulkByteTokenRequest struct {
	ActorID []byte `cbor:"actor_id"`
	// NameHash is the set's whole address (`set_name_hash`): the bridge never
	// sends a set's plaintext name (`path-sealing.md` § the set-name plane).
	NameHash []byte `cbor:"name_hash,omitempty"`
	Access   string `cbor:"access"`
	// purpose (Rust `BulkByteMintPurpose`, `#[serde(default,
	// skip_serializing_if = "BulkByteMintPurpose::is_folder")]`). omitempty ⇒
	// omitted for a folder mint, the byte-identical pre-field shape.
	Purpose string `cbor:"purpose,omitempty"`
}

// mintBulkByteTokenReply mirrors MintBulkByteTokenReply.
type mintBulkByteTokenReply struct {
	Token     string `cbor:"token"`
	ExpiresAt uint64 `cbor:"expires_at"`
}

// MintBulkByteToken mints a short-TTL bearer the chunk/manifest byte routes
// accept for the set addressed by `nameHash` at `access` ("read"/"write"). Nest
// gates the mint on the set being served (owned + non-reserved +
// webdav_enabled) — else set_not_served. BridgeMda only.
func MintBulkByteToken(
	ctx context.Context,
	c Caller,
	actorID []byte,
	nameHash []byte,
	access string,
) (token string, expiresAt uint64, err error) {
	return mintBulkByteToken(ctx, c, mintBulkByteTokenRequest{
		ActorID:  actorID,
		NameHash: nameHash,
		Access:   access,
		// Purpose left empty ⇒ omitted ⇒ Rust's Folder default. The wire shape
		// is byte-identical to the pre-purpose mint.
	})
}

// MintMailBodyByteToken mints a short-TTL `write` bearer for staging a sealed
// mail body's chunks on the byte plane (smtp-server.md § Message size limits).
// Callable by BridgeMta *and* BridgeMda; nest gates it on `actorID` being a mail
// recipient here — the same recipient-seal-key presence the ingest handler
// already fails closed on. Mail belongs to no folder, so none is named.
//
// There is no read-side counterpart: the chunk download route is open
// (ciphertext-by-hash, and the key never leaves the recipient), so the consumer
// leg needs no token at all.
func MintMailBodyByteToken(
	ctx context.Context,
	c Caller,
	actorID []byte,
) (token string, expiresAt uint64, err error) {
	return mintBulkByteToken(ctx, c, mintBulkByteTokenRequest{
		ActorID: actorID,
		Access:  BulkByteAccessWrite,
		Purpose: BulkByteMintPurposeMailBody,
	})
}

func mintBulkByteToken(
	ctx context.Context,
	c Caller,
	req mintBulkByteTokenRequest,
) (token string, expiresAt uint64, err error) {
	var reply mintBulkByteTokenReply
	if err := c.Call(ctx, MethodMintBulkByteToken, req, &reply); err != nil {
		return "", 0, fmt.Errorf("mint_bulk_byte_token: %w", err)
	}
	return reply.Token, reply.ExpiresAt, nil
}

// webdavListFoldersRequest mirrors WebdavListFoldersRequest.
type webdavListFoldersRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// WebdavServedSet mirrors WebdavServedSet — the name and its set_name_hash; the
// MDA reads read_only and the display name of a sealed set (whose row holds no
// plaintext name) from the WebdavKeysBlob, not here. NameHash is empty when the
// folder row has no `name_hash` stamped.
type WebdavServedSet struct {
	Name     string `cbor:"name"`
	NameHash []byte `cbor:"name_hash,omitempty"`
}

type webdavListFoldersReply struct {
	Folders []WebdavServedSet `cbor:"folders"`
}

// WebdavListFolders enumerates the actor's WebDAV-served sets (non-reserved +
// webdav_enabled). Drives the MDA's root-collection PROPFIND.
func WebdavListFolders(ctx context.Context, c Caller, actorID []byte) ([]WebdavServedSet, error) {
	req := webdavListFoldersRequest{ActorID: actorID}
	var reply webdavListFoldersReply
	if err := c.Call(ctx, MethodWebdavListFolders, req, &reply); err != nil {
		return nil, fmt.Errorf("webdav_list_folders: %w", err)
	}
	return reply.Folders, nil
}

// webdavAdmitPrincipalRequest mirrors WebdavAdmitPrincipalRequest.
type webdavAdmitPrincipalRequest struct {
	Token      string   `cbor:"token"`
	DPoPProofs []string `cbor:"dpop_proofs"`
	HTM        string   `cbor:"htm"`
	HTU        string   `cbor:"htu"`
}

// WebdavAdmittedFolder mirrors WebdavAdmittedFolder — one `folder:read` scope's
// folder, re-resolved at the admission. The door serves it only when both
// GrantLive and Served hold.
type WebdavAdmittedFolder struct {
	FolderID  int64  `cbor:"folder_id"`
	NameHash  []byte `cbor:"name_hash"`
	GrantLive bool   `cbor:"grant_live"`
	Served    bool   `cbor:"served"`
}

// WebdavAdmittedPrincipal mirrors WebdavAdmittedPrincipal. Exp is the access
// token's `exp`, epoch seconds — the admission cache's ceiling.
type WebdavAdmittedPrincipal struct {
	ActorID      []byte                 `cbor:"actor_id"`
	HolderX25519 []byte                 `cbor:"holder_x25519"`
	Scopes       []string               `cbor:"scopes"`
	Exp          int64                  `cbor:"exp"`
	Folders      []WebdavAdmittedFolder `cbor:"folders"`
}

// WebdavAdmitPrincipalReply mirrors WebdavAdmitPrincipalReply: Admitted on
// success; else Admitted is nil and Status, WWWAuthenticate and DPoPNonce are
// the challenge the nest's own door renders, relayed to the DAV client verbatim.
type WebdavAdmitPrincipalReply struct {
	Admitted        *WebdavAdmittedPrincipal `cbor:"admitted,omitempty"`
	Status          uint16                   `cbor:"status"`
	WWWAuthenticate *string                  `cbor:"www_authenticate,omitempty"`
	DPoPNonce       *string                  `cbor:"dpop_nonce,omitempty"`
}

// WebdavAdmitPrincipal relays a principal's bearer presentation — the token from
// `Authorization: DPoP`, every `DPoP` proof header, the request's method (`htm`)
// and absolute `https` URL under `/webdav/` (`htu`) — to the nest's admission.
// A refused admission is a reply, not an error. BridgeMda only.
func WebdavAdmitPrincipal(ctx context.Context, c Caller, token string, proofs []string, htm, htu string) (WebdavAdmitPrincipalReply, error) {
	if proofs == nil {
		proofs = []string{}
	}
	req := webdavAdmitPrincipalRequest{Token: token, DPoPProofs: proofs, HTM: htm, HTU: htu}
	var reply WebdavAdmitPrincipalReply
	if err := c.Call(ctx, MethodWebdavAdmitPrincipal, req, &reply); err != nil {
		return WebdavAdmitPrincipalReply{}, fmt.Errorf("webdav_admit_principal: %w", err)
	}
	return reply, nil
}

// webdavQuotaRequest mirrors WebdavQuotaRequest.
type webdavQuotaRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// WebdavQuota mirrors WebdavQuotaReply — the actor's file-storage meter and
// tier ceiling (Limit nil = no ceiling), the pair webdav_record_change enforces.
type WebdavQuota struct {
	Used  uint64  `cbor:"storage_bytes_used"`
	Limit *uint64 `cbor:"storage_bytes_limit,omitempty"`
}

// WebdavQuotaFor fetches the actor's storage usage and ceiling for the MDA's
// RFC 4331 quota properties.
func WebdavQuotaFor(ctx context.Context, c Caller, actorID []byte) (WebdavQuota, error) {
	req := webdavQuotaRequest{ActorID: actorID}
	var reply WebdavQuota
	if err := c.Call(ctx, MethodWebdavQuota, req, &reply); err != nil {
		return WebdavQuota{}, fmt.Errorf("webdav_quota: %w", err)
	}
	return reply, nil
}

// webdavListFilesRequest mirrors WebdavListFilesRequest.
type webdavListFilesRequest struct {
	ActorID []byte `cbor:"actor_id"`
	// NameHash is the set's whole address (`set_name_hash`): the bridge never
	// sends a set's plaintext name (`path-sealing.md` § the set-name plane).
	NameHash []byte `cbor:"name_hash,omitempty"`
}

// WebdavFile mirrors WebdavFile — one served-set file, latest-per-path.
// `ManifestHash` is the hex ETag; `ContentKeyVersion` (nil for public/declassified rows) is
// the generation the MDA selects `key_for` to GET-decrypt.
type WebdavFile struct {
	Path              string  `cbor:"path"`
	ManifestHash      string  `cbor:"manifest_hash"`
	SizeBytes         int64   `cbor:"size_bytes"`
	UpdatedAt         int64   `cbor:"updated_at"`
	ContentKeyVersion *uint64 `cbor:"content_key_version,omitempty"`
	// PathSealed is the row's opaque SealedLabel envelope over Path, and
	// PathHash the convergent salt it opens under. The MDA renders them
	// sealed-first through shared Rust (faunaFfi.WebdavRenderPaths); the nest
	// can open neither. Both nil on public-audience rows, which render from the plaintext Path.
	PathSealed []byte `cbor:"path_sealed,omitempty"`
	PathHash   []byte `cbor:"path_hash,omitempty"`
}

type webdavListFilesReply struct {
	Files []WebdavFile `cbor:"files"`
}

// WebdavListFiles lists one served set's latest-per-path files. Gated on the
// set being served (else set_not_served).
func WebdavListFiles(ctx context.Context, c Caller, actorID []byte, nameHash []byte) ([]WebdavFile, error) {
	req := webdavListFilesRequest{ActorID: actorID, NameHash: nameHash}
	var reply webdavListFilesReply
	if err := c.Call(ctx, MethodWebdavListFiles, req, &reply); err != nil {
		return nil, fmt.Errorf("webdav_list_files: %w", err)
	}
	return reply.Files, nil
}

// webdavRecordChangeRequest mirrors WebdavRecordChangeRequest. Optional fields
// are pointers with `,omitempty` so nil ⇒ absent (Rust `#[serde(default)]`
// decodes absent → None), matching the skip_serializing_if wire discipline.
type webdavRecordChangeRequest struct {
	ActorID []byte `cbor:"actor_id"`
	// NameHash is the set's whole address (`set_name_hash`): the bridge never
	// sends a set's plaintext name (`path-sealing.md` § the set-name plane).
	NameHash          []byte  `cbor:"name_hash,omitempty"`
	Path              string  `cbor:"path"`
	ManifestHash      *string `cbor:"manifest_hash,omitempty"`
	SizeBytes         int64   `cbor:"size_bytes"`
	ChangeType        string  `cbor:"change_type"`
	ContentKeyVersion *uint64 `cbor:"content_key_version,omitempty"`
	IfMatch           *string `cbor:"if_match,omitempty"`
	IfNoneMatch       *string `cbor:"if_none_match,omitempty"`
	PathSealed        []byte  `cbor:"path_sealed,omitempty"`
}

type webdavRecordChangeReply struct {
	Seq int64 `cbor:"seq"`
}

// WebdavRecordChange records a WebDAV write as an ordinary folder change,
// attributed nest-side to the actor's stable "WebDAV" pseudo-device. `changeType`
// is "Created"/"Modified"/"Deleted"; `manifestHash` is nil for a delete.
// If-Match/If-None-Match are enforced authoritatively at the nest — a lost race
// surfaces as the typed `fauna.bridges.conflict` error (the MDA maps it to 412).
// `pathSealed` is the bridge-sealed SealedLabel over `path` (nil only if the set
// has no content keys), sealed under the SAME generation as `contentKeyVersion`.
// `nameHash` is the set's whole address.
func WebdavRecordChange(
	ctx context.Context,
	c Caller,
	actorID []byte,
	nameHash []byte,
	path string,
	manifestHash *string,
	sizeBytes int64,
	changeType string,
	contentKeyVersion *uint64,
	ifMatch, ifNoneMatch *string,
	pathSealed []byte,
) (int64, error) {
	req := webdavRecordChangeRequest{
		ActorID:           actorID,
		NameHash:          nameHash,
		Path:              path,
		ManifestHash:      manifestHash,
		SizeBytes:         sizeBytes,
		ChangeType:        changeType,
		ContentKeyVersion: contentKeyVersion,
		IfMatch:           ifMatch,
		IfNoneMatch:       ifNoneMatch,
		PathSealed:        pathSealed,
	}
	var reply webdavRecordChangeReply
	if err := c.Call(ctx, MethodWebdavRecordChange, req, &reply); err != nil {
		return 0, fmt.Errorf("webdav_record_change: %w", err)
	}
	return reply.Seq, nil
}

// ── CardDAV (`bridge_carddav_*`) — the contacts twin of the CalDAV block ──
//
// Every type below mirrors libs/fauna-protocol/src/bridge_routing.rs with
// calendar→addressbook and event→card; the wire discipline is identical (32-byte
// ids via serde_bytes → []byte, `#[serde(tag = "outcome")]` reply enums flattened
// to a single `Outcome`-gated Go struct, blake3 uid_hash, RFC 6578 sync tokens).
// CardDAV rides the existing CalDAV listener/port — there is no separate CardDAV
// method for config; ConfigSnapshot.CardDAVEnabled gates the handler. All six are
// allowlisted server-side for BridgeMda only.

// ── provision_addressbook / list_addressbooks ───────────────────

// provisionAddressbookRequest mirrors ProvisionAddressbookRequest. The
// `update_metadata` bool routes between the MKCOL insert path (`false`) and the
// PROPPATCH overwrite path (`true`); on the wire the field is always present,
// and the Rust side's `#[serde(default)]` keeps legacy-encoded bodies (no field)
// decoding to `false` for forward compat. Twin of provisionCalendarRequest.
type provisionAddressbookRequest struct {
	ActorID           []byte `cbor:"actor_id"`
	AddressbookID     []byte `cbor:"addressbook_id"`
	EncryptedMetadata []byte `cbor:"encrypted_metadata"`
	UpdateMetadata    bool   `cbor:"update_metadata"`
}

// provisionAddressbookReply mirrors ProvisionAddressbookReply.
type provisionAddressbookReply struct {
	Outcome string `cbor:"outcome"`
}

// ProvisionAddressbookOutcome is the typed result of a ProvisionAddressbook
// call. The first three variants (`created`, `already_exists`, `conflict`) fire
// on the MKCOL path (`updateMetadata=false`); the last two (`updated`,
// `not_found`) fire on the PROPPATCH overwrite path (`updateMetadata=true`).
type ProvisionAddressbookOutcome string

const (
	ProvisionAddressbookCreated       ProvisionAddressbookOutcome = "created"
	ProvisionAddressbookAlreadyExists ProvisionAddressbookOutcome = "already_exists"
	ProvisionAddressbookConflict      ProvisionAddressbookOutcome = "conflict"
	ProvisionAddressbookUpdated       ProvisionAddressbookOutcome = "updated"
	ProvisionAddressbookNotFound      ProvisionAddressbookOutcome = "not_found"
)

// ProvisionAddressbook issues `fauna.bridges.provision_addressbook` for the
// given (actor, addressbook_id) tuple. `updateMetadata=false` is the MKCOL /
// lazy-provision insert path — idempotent on byte-identical `encryptedMetadata`,
// returns `Conflict` on byte-different retry. `updateMetadata=true` is the
// PROPPATCH overwrite path: nest overwrites `encrypted_metadata`, bumps
// `highestmodseq`, and returns `Updated` (or `NotFound` if the row doesn't
// exist). Twin of ProvisionCalendar.
func ProvisionAddressbook(
	ctx context.Context,
	c Caller,
	actorID, addressbookID, encryptedMetadata []byte,
	updateMetadata bool,
) (ProvisionAddressbookOutcome, error) {
	req := provisionAddressbookRequest{
		ActorID:           actorID,
		AddressbookID:     addressbookID,
		EncryptedMetadata: encryptedMetadata,
		UpdateMetadata:    updateMetadata,
	}
	var reply provisionAddressbookReply
	if err := c.Call(ctx, MethodProvisionAddressbook, req, &reply); err != nil {
		return "", fmt.Errorf("provision_addressbook: %w", err)
	}
	switch ProvisionAddressbookOutcome(reply.Outcome) {
	case ProvisionAddressbookCreated, ProvisionAddressbookAlreadyExists, ProvisionAddressbookConflict,
		ProvisionAddressbookUpdated, ProvisionAddressbookNotFound:
		return ProvisionAddressbookOutcome(reply.Outcome), nil
	default:
		return "", fmt.Errorf("provision_addressbook: unknown outcome %q", reply.Outcome)
	}
}

// listAddressbooksRequest mirrors ListAddressbooksRequest.
type listAddressbooksRequest struct {
	ActorID []byte `cbor:"actor_id"`
}

// AddressbookEntry mirrors libs/fauna-protocol/src/bridge_routing.rs
// AddressbookEntry. `EncryptedMetadata` is the sealed-to-recipient blob the MDA
// decrypts in-session for the AUTH'd actor; nest treats it as opaque bytes.
// Twin of CalendarEntry (EventCount→CardCount).
type AddressbookEntry struct {
	AddressbookID     []byte `cbor:"addressbook_id"`
	EncryptedMetadata []byte `cbor:"encrypted_metadata"`
	CTag              int64  `cbor:"ctag"`
	HighestModseq     int64  `cbor:"highestmodseq"`
	CardCount         uint32 `cbor:"card_count"`
	CreatedAt         int64  `cbor:"created_at"`
}

// listAddressbooksReply mirrors ListAddressbooksReply.
type listAddressbooksReply struct {
	Addressbooks []AddressbookEntry `cbor:"addressbooks"`
}

// ListAddressbooks issues `fauna.bridges.list_addressbooks` for an actor. Empty
// reply is normal — the actor simply has no provisioned address books.
// Allowlisted server-side for `BridgeMda` only. Twin of ListCalendars.
func ListAddressbooks(ctx context.Context, c Caller, actorID []byte) ([]AddressbookEntry, error) {
	req := listAddressbooksRequest{ActorID: actorID}
	var reply listAddressbooksReply
	if err := c.Call(ctx, MethodListAddressbooks, req, &reply); err != nil {
		return nil, fmt.Errorf("list_addressbooks: %w", err)
	}
	return reply.Addressbooks, nil
}

// ── put_card_ciphertext / delete_card (CardDAV PUT/DELETE) ───────

// putCardCiphertextRequest mirrors PutCardCiphertextRequest. `IfMatch` is
// `*string` so absent (`None` on the wire) and empty string are
// distinguishable — CardDAV `If-Match: *` is normalized to absent on the
// bridge. Twin of putEventCiphertextRequest.
type putCardCiphertextRequest struct {
	ActorID            []byte  `cbor:"actor_id"`
	AddressbookID      []byte  `cbor:"addressbook_id"`
	UIDHash            []byte  `cbor:"uid_hash"`
	EncryptedBody      []byte  `cbor:"encrypted_body"`
	EncryptedIndexHint []byte  `cbor:"encrypted_index_hint"`
	Timestamp          int64   `cbor:"timestamp"`
	CiphertextSize     uint32  `cbor:"ciphertext_size"`
	IfMatch            *string `cbor:"if_match"`
	// EncryptedFaunaExt mirrors Rust `PutCardCiphertextRequest.encrypted_fauna_ext`
	// (Option<Vec<u8>>) — the Fauna-only vCard sidecar (e.g. the X-FAUNA-ACTOR-ID
	// linkage). The MDA NEVER writes a sidecar (a MUA PUT carries none), so this
	// stays nil ⇒ omitted ⇒ Rust `None` ⇒ nest PRESERVES the prior sidecar on
	// UPDATE. `*[]byte` not `[]byte`: a nil bare slice would encode as empty bytes
	// (= Some(empty)) under NilContainersAsEmpty; a nil pointer is omitted,
	// preserving the Option None semantics.
	EncryptedFaunaExt *[]byte `cbor:"encrypted_fauna_ext,omitempty"`
}

// putCardCiphertextReply flattens the Rust tagged union — every possible payload
// field rides in the same map, gated by `Outcome`. `CardID`/`ETag`/`Modseq` are
// populated on Created/Updated; `CurrentETag` on PreconditionFailed; nothing
// extra on AddressbookNotFound. Twin of putEventCiphertextReply.
type putCardCiphertextReply struct {
	Outcome     string `cbor:"outcome"`
	CardID      []byte `cbor:"card_id,omitempty"`
	ETag        string `cbor:"etag,omitempty"`
	Modseq      int64  `cbor:"modseq,omitempty"`
	CurrentETag string `cbor:"current_etag,omitempty"`
}

// PutCardCiphertextOutcome is the externally-meaningful classification of a
// `put_card_ciphertext` reply. Maps 1:1 to PutCardCiphertextReply variants.
type PutCardCiphertextOutcome string

const (
	PutCardCreated             PutCardCiphertextOutcome = "created"
	PutCardUpdated             PutCardCiphertextOutcome = "updated"
	PutCardPreconditionFailed  PutCardCiphertextOutcome = "precondition_failed"
	PutCardAddressbookNotFound PutCardCiphertextOutcome = "addressbook_not_found"
)

// PutCardResult is the typed payload of a successful put_card_ciphertext call.
// On `PutCardCreated` / `PutCardUpdated`, `CardID`, `ETag`, `Modseq` are
// populated. On `PutCardPreconditionFailed`, `CurrentETag` carries the row's
// current etag. On `PutCardAddressbookNotFound`, all fields are zero.
type PutCardResult struct {
	Outcome     PutCardCiphertextOutcome
	CardID      []byte
	ETag        string
	Modseq      int64
	CurrentETag string
}

// PutCardCiphertext issues `fauna.bridges.put_card_ciphertext`. The MDA calls
// this from CardDAV PUT after parsing + validating the vCard body,
// blake3-hashing the plaintext UID, and HPKE-sealing the raw body + tokenized
// index hint to the AUTH'd actor's MLS pubkey + index key. `ifMatch` is `nil`
// for unconditional PUT, `&etag` for conditional PUT (CardDAV `If-Match`
// header). The MDA never writes a Fauna-extension sidecar, so the request's
// `encrypted_fauna_ext` is always omitted. Twin of PutEventCiphertext.
func PutCardCiphertext(
	ctx context.Context,
	c Caller,
	actorID, addressbookID, uidHash, encryptedBody, encryptedIndexHint []byte,
	timestamp int64,
	ciphertextSize uint32,
	ifMatch *string,
) (PutCardResult, error) {
	req := putCardCiphertextRequest{
		ActorID:            actorID,
		AddressbookID:      addressbookID,
		UIDHash:            uidHash,
		EncryptedBody:      encryptedBody,
		EncryptedIndexHint: encryptedIndexHint,
		Timestamp:          timestamp,
		CiphertextSize:     ciphertextSize,
		IfMatch:            ifMatch,
	}
	var reply putCardCiphertextReply
	if err := c.Call(ctx, MethodPutCardCiphertext, req, &reply); err != nil {
		return PutCardResult{}, fmt.Errorf("put_card_ciphertext: %w", err)
	}
	switch PutCardCiphertextOutcome(reply.Outcome) {
	case PutCardCreated, PutCardUpdated:
		return PutCardResult{
			Outcome: PutCardCiphertextOutcome(reply.Outcome),
			CardID:  reply.CardID,
			ETag:    reply.ETag,
			Modseq:  reply.Modseq,
		}, nil
	case PutCardPreconditionFailed:
		return PutCardResult{
			Outcome:     PutCardPreconditionFailed,
			CurrentETag: reply.CurrentETag,
		}, nil
	case PutCardAddressbookNotFound:
		return PutCardResult{Outcome: PutCardAddressbookNotFound}, nil
	default:
		return PutCardResult{}, fmt.Errorf("put_card_ciphertext: unknown outcome %q", reply.Outcome)
	}
}

// deleteCardRequest mirrors DeleteCardRequest. Twin of deleteEventRequest.
type deleteCardRequest struct {
	ActorID       []byte  `cbor:"actor_id"`
	AddressbookID []byte  `cbor:"addressbook_id"`
	UIDHash       []byte  `cbor:"uid_hash"`
	IfMatch       *string `cbor:"if_match"`
}

// deleteCardReply flattens the Rust tagged union per the same pattern as
// putCardCiphertextReply. Twin of deleteEventReply.
type deleteCardReply struct {
	Outcome     string `cbor:"outcome"`
	CardID      []byte `cbor:"card_id,omitempty"`
	Modseq      int64  `cbor:"modseq,omitempty"`
	CurrentETag string `cbor:"current_etag,omitempty"`
}

// DeleteCardOutcome is the externally-meaningful classification of a
// `delete_card` reply.
type DeleteCardOutcome string

const (
	DeleteCardDeleted            DeleteCardOutcome = "deleted"
	DeleteCardNotFound           DeleteCardOutcome = "not_found"
	DeleteCardPreconditionFailed DeleteCardOutcome = "precondition_failed"
)

// DeleteCardResult is the typed payload of a successful delete_card call. On
// `DeleteCardDeleted`, `CardID` and `Modseq` are populated. On
// `DeleteCardPreconditionFailed`, `CurrentETag` carries the row's current etag.
// On `DeleteCardNotFound`, all fields are zero (the missing-card and
// missing-addressbook paths are intentionally collapsed nest-side).
type DeleteCardResult struct {
	Outcome     DeleteCardOutcome
	CardID      []byte
	Modseq      int64
	CurrentETag string
}

// DeleteCard issues `fauna.bridges.delete_card`. The MDA calls this from CardDAV
// DELETE. `ifMatch` is `nil` for unconditional delete, `&etag` for conditional
// (CardDAV `If-Match` header). Twin of DeleteEvent.
func DeleteCard(
	ctx context.Context,
	c Caller,
	actorID, addressbookID, uidHash []byte,
	ifMatch *string,
) (DeleteCardResult, error) {
	req := deleteCardRequest{
		ActorID:       actorID,
		AddressbookID: addressbookID,
		UIDHash:       uidHash,
		IfMatch:       ifMatch,
	}
	var reply deleteCardReply
	if err := c.Call(ctx, MethodDeleteCard, req, &reply); err != nil {
		return DeleteCardResult{}, fmt.Errorf("delete_card: %w", err)
	}
	switch DeleteCardOutcome(reply.Outcome) {
	case DeleteCardDeleted:
		return DeleteCardResult{
			Outcome: DeleteCardDeleted,
			CardID:  reply.CardID,
			Modseq:  reply.Modseq,
		}, nil
	case DeleteCardNotFound:
		return DeleteCardResult{Outcome: DeleteCardNotFound}, nil
	case DeleteCardPreconditionFailed:
		return DeleteCardResult{
			Outcome:     DeleteCardPreconditionFailed,
			CurrentETag: reply.CurrentETag,
		}, nil
	default:
		return DeleteCardResult{}, fmt.Errorf("delete_card: unknown outcome %q", reply.Outcome)
	}
}

// ── delete_addressbook (CardDAV DELETE on a collection URL) ──────

// deleteAddressbookRequest mirrors DeleteAddressbookRequest — book-level, so no
// uid_hash / if_match (a collection DELETE is unconditional).
type deleteAddressbookRequest struct {
	ActorID       []byte `cbor:"actor_id"`
	AddressbookID []byte `cbor:"addressbook_id"`
}

// deleteAddressbookReply flattens the Rust tagged union. `cards_deleted` is only
// meaningful on the `deleted` outcome (the number of cards cascade-removed).
type deleteAddressbookReply struct {
	Outcome      string `cbor:"outcome"`
	CardsDeleted uint32 `cbor:"cards_deleted,omitempty"`
}

// DeleteAddressbookOutcome is the externally-meaningful classification of a
// `delete_addressbook` reply.
type DeleteAddressbookOutcome string

const (
	DeleteAddressbookDeleted  DeleteAddressbookOutcome = "deleted"
	DeleteAddressbookNotFound DeleteAddressbookOutcome = "not_found"
)

// DeleteAddressbookResult is the typed payload of a delete_addressbook call. On
// `DeleteAddressbookDeleted`, `CardsDeleted` carries the number of cards
// cascade-removed. On `DeleteAddressbookNotFound`, all fields are zero.
type DeleteAddressbookResult struct {
	Outcome      DeleteAddressbookOutcome
	CardsDeleted uint32
}

// DeleteAddressbook issues `fauna.bridges.delete_addressbook`. The MDA calls
// this from CardDAV DELETE on an address-book *collection* URL (distinct from
// DeleteCard's DELETE on a single card resource). The whole book and every card
// it holds are cascade-deleted nest-side under one lock. No template on the
// CalDAV side (emersion's caldav.Backend omits DeleteCalendar).
func DeleteAddressbook(
	ctx context.Context,
	c Caller,
	actorID, addressbookID []byte,
) (DeleteAddressbookResult, error) {
	req := deleteAddressbookRequest{
		ActorID:       actorID,
		AddressbookID: addressbookID,
	}
	var reply deleteAddressbookReply
	if err := c.Call(ctx, MethodDeleteAddressbook, req, &reply); err != nil {
		return DeleteAddressbookResult{}, fmt.Errorf("delete_addressbook: %w", err)
	}
	switch DeleteAddressbookOutcome(reply.Outcome) {
	case DeleteAddressbookDeleted:
		return DeleteAddressbookResult{
			Outcome:      DeleteAddressbookDeleted,
			CardsDeleted: reply.CardsDeleted,
		}, nil
	case DeleteAddressbookNotFound:
		return DeleteAddressbookResult{Outcome: DeleteAddressbookNotFound}, nil
	default:
		return DeleteAddressbookResult{}, fmt.Errorf("delete_addressbook: unknown outcome %q", reply.Outcome)
	}
}

// ── query_cards / sync_addressbook_since (CardDAV REPORT) ────────

// CardEntry mirrors libs/fauna-protocol/src/bridge_routing.rs `CardEntry` — one
// sealed vCard row. `EncryptedBody` / `EncryptedIndexHint` are HPKE-sealed to
// the AUTH'd actor's MLS pubkey + index pubkey respectively; the MDA opens them
// in-session. Twin of EventEntry.
type CardEntry struct {
	CardID             []byte `cbor:"card_id"`
	UIDHash            []byte `cbor:"uid_hash"`
	EncryptedBody      []byte `cbor:"encrypted_body"`
	EncryptedIndexHint []byte `cbor:"encrypted_index_hint"`
	ETag               string `cbor:"etag"`
	Modseq             int64  `cbor:"modseq"`
	CiphertextSize     uint32 `cbor:"ciphertext_size"`
	InternalDate       int64  `cbor:"internal_date"`
	// EncryptedFaunaExt mirrors Rust `CardEntry.encrypted_fauna_ext`. Fauna
	// apps read the sidecar; the MDA NEVER decrypts or serves it to a CardDAV
	// MUA (the canonical vCard in EncryptedBody is all a MUA receives). Present
	// only on Fauna-written cards; nil otherwise.
	EncryptedFaunaExt *[]byte `cbor:"encrypted_fauna_ext,omitempty"`
}

// ExpungedCardEntry mirrors libs/fauna-protocol/src/bridge_routing.rs
// `ExpungedCardEntry` — one deletion tombstone (RFC 6578 VANISHED in CardDAV
// sync-collection reports). Twin of ExpungedEntry.
type ExpungedCardEntry struct {
	CardID  []byte `cbor:"card_id"`
	UIDHash []byte `cbor:"uid_hash"`
	Modseq  int64  `cbor:"modseq"`
}

// queryCardsRequest mirrors QueryCardsRequest. `SinceModseq` / `AfterCardID` are
// pointer-nullable so absent (`None` on the wire) and zero are distinguishable.
// `AfterCardID` is `*[]byte` (not a bare `[]byte`) specifically because the
// codec runs NilContainersAsEmpty (internal/dagcbor/codec.go): a nil bare slice
// would encode as an empty byte string (0x40 = `Some(empty)`), corrupting the
// `Option<ByteBuf>` None = "fresh page" semantics. A nil pointer still encodes
// as CBOR null = `None`. Twin of queryEventsRequest.
type queryCardsRequest struct {
	ActorID       []byte  `cbor:"actor_id"`
	AddressbookID []byte  `cbor:"addressbook_id"`
	SinceModseq   *int64  `cbor:"since_modseq"`
	AfterCardID   *[]byte `cbor:"after_card_id"`
	Limit         uint32  `cbor:"limit"`
}

// queryCardsReply flattens the Rust tagged union: every payload field rides in
// the same map, gated by `Outcome`. `Cards` / `HighestModseq` / `More` are
// populated on `Ok`; nothing extra on `AddressbookNotFound`. Twin of
// queryEventsReply.
type queryCardsReply struct {
	Outcome       string      `cbor:"outcome"`
	Cards         []CardEntry `cbor:"cards,omitempty"`
	HighestModseq int64       `cbor:"highestmodseq,omitempty"`
	More          bool        `cbor:"more,omitempty"`
}

// QueryCardsOutcome is the externally-meaningful classification of a
// `query_cards` reply.
type QueryCardsOutcome string

const (
	QueryCardsOk                  QueryCardsOutcome = "ok"
	QueryCardsAddressbookNotFound QueryCardsOutcome = "addressbook_not_found"
)

// QueryCardsResult is the typed payload of a successful query_cards call. On
// `QueryCardsOk`, `Cards` / `HighestModseq` / `More` are populated. On
// `QueryCardsAddressbookNotFound`, all fields are zero.
type QueryCardsResult struct {
	Outcome       QueryCardsOutcome
	Cards         []CardEntry
	HighestModseq int64
	More          bool
}

// QueryCards issues `fauna.bridges.query_cards`. The MDA calls this from CardDAV
// REPORT addressbook-query / multiget — nest returns every card in the address
// book (or the paginated subset since `sinceModseq`); the MDA decrypts in-session
// and filters locally.
//
//   - `sinceModseq` = `nil` for full addressbook-query / multiget; `&n` for
//     CONDSTORE incremental (cards with `modseq > n` only).
//   - `afterCardID` = `nil` to start a fresh page; the prior reply's
//     `cards.last().CardID` to resume.
//   - `limit` = 0 for unbounded (whole address book in one reply).
//
// Twin of QueryEvents.
func QueryCards(
	ctx context.Context,
	c Caller,
	actorID, addressbookID []byte,
	sinceModseq *int64,
	afterCardID []byte,
	limit uint32,
) (QueryCardsResult, error) {
	// nil afterCardID = "start a fresh page" → leave the pointer nil so it
	// encodes as CBOR null (`None`); a present cursor encodes as a byte string
	// (`Some`). See queryCardsRequest's doc on why this is a pointer rather than
	// a bare []byte under NilContainersAsEmpty.
	var afterCardIDPtr *[]byte
	if afterCardID != nil {
		afterCardIDPtr = &afterCardID
	}
	req := queryCardsRequest{
		ActorID:       actorID,
		AddressbookID: addressbookID,
		SinceModseq:   sinceModseq,
		AfterCardID:   afterCardIDPtr,
		Limit:         limit,
	}
	var reply queryCardsReply
	if err := c.Call(ctx, MethodQueryCards, req, &reply); err != nil {
		return QueryCardsResult{}, fmt.Errorf("query_cards: %w", err)
	}
	switch QueryCardsOutcome(reply.Outcome) {
	case QueryCardsOk:
		return QueryCardsResult{
			Outcome:       QueryCardsOk,
			Cards:         reply.Cards,
			HighestModseq: reply.HighestModseq,
			More:          reply.More,
		}, nil
	case QueryCardsAddressbookNotFound:
		return QueryCardsResult{Outcome: QueryCardsAddressbookNotFound}, nil
	default:
		return QueryCardsResult{}, fmt.Errorf("query_cards: unknown outcome %q", reply.Outcome)
	}
}

// syncAddressbookSinceRequest mirrors SyncAddressbookSinceRequest. The
// `sync_token` field is intentionally a string on the wire (RFC 6578 §3.1) even
// though its concrete representation is a decimal modseq; `"0"` performs a full
// sync. Twin of syncCalendarSinceRequest.
type syncAddressbookSinceRequest struct {
	ActorID       []byte  `cbor:"actor_id"`
	AddressbookID []byte  `cbor:"addressbook_id"`
	SyncToken     string  `cbor:"sync_token"`
	Limit         uint32  `cbor:"limit"`
	MuaID         *string `cbor:"mua_id,omitempty"`
}

// syncAddressbookSinceReply flattens the tagged union. `Changed` / `Expunged` /
// `NewSyncToken` / `More` / `Stale` populate on `Ok`; nothing extra on
// `AddressbookNotFound`; `ServerModseq` populates on the `Stale` outcome
// (distinct from the `Ok` `stale` flag). Twin of syncCalendarSinceReply.
type syncAddressbookSinceReply struct {
	Outcome      string              `cbor:"outcome"`
	Changed      []CardEntry         `cbor:"changed,omitempty"`
	Expunged     []ExpungedCardEntry `cbor:"expunged,omitempty"`
	NewSyncToken string              `cbor:"new_sync_token,omitempty"`
	More         bool                `cbor:"more,omitempty"`
	// Stale on an `Ok` reply means the supplied sync-token is valid but predates
	// the tombstone-retention window (SyncAddressbookSinceReply::Ok `stale`
	// field); the MDA emits DAV:valid-sync-token. Distinct from the `Stale`
	// *outcome* (MUA-ahead). Defaults false.
	Stale        bool  `cbor:"stale,omitempty"`
	ServerModseq int64 `cbor:"server_modseq,omitempty"`
}

// SyncAddressbookSinceOutcome is the externally-meaningful classification of a
// `sync_addressbook_since` reply.
type SyncAddressbookSinceOutcome string

const (
	SyncAddressbookSinceOk                  SyncAddressbookSinceOutcome = "ok"
	SyncAddressbookSinceAddressbookNotFound SyncAddressbookSinceOutcome = "addressbook_not_found"
	// SyncAddressbookSinceStale is returned when the client's sync-token is
	// ahead of the address book's current highestmodseq — the post-DR-restore
	// case. Callers should fall through to a full PROPFIND.
	SyncAddressbookSinceStale SyncAddressbookSinceOutcome = "stale"
)

// SyncAddressbookSinceResult is the typed payload of a sync_addressbook_since
// call. Twin of SyncCalendarSinceResult.
type SyncAddressbookSinceResult struct {
	Outcome      SyncAddressbookSinceOutcome
	Changed      []CardEntry
	Expunged     []ExpungedCardEntry
	NewSyncToken string
	More         bool
	// Stale is set when Outcome == SyncAddressbookSinceOk AND the supplied
	// sync-token predates the tombstone-retention window. The MDA treats it like
	// the SyncAddressbookSinceStale outcome — emit DAV:valid-sync-token — but it
	// is a distinct nest-side condition (no restore-divergence row).
	Stale bool
	// ServerModseq is populated when Outcome == SyncAddressbookSinceStale. Zero
	// for all other outcomes.
	ServerModseq int64
}

// SyncAddressbookSince issues `fauna.bridges.sync_addressbook_since`. The MDA
// calls this from CardDAV REPORT sync-collection. `syncToken` = `"0"` (or empty,
// normalized to `"0"`) is full sync; any other value is a decimal modseq
// returned by a prior call. `limit` = 0 is unbounded (tombstones are never
// paginated regardless). `muaID` is the MUA's User-Agent string (empty if not
// provided); forwarded to nest for divergence-logging. Twin of
// SyncCalendarSince.
func SyncAddressbookSince(
	ctx context.Context,
	c Caller,
	actorID, addressbookID []byte,
	syncToken string,
	limit uint32,
	muaID string,
) (SyncAddressbookSinceResult, error) {
	if syncToken == "" {
		syncToken = "0"
	}
	req := syncAddressbookSinceRequest{
		ActorID:       actorID,
		AddressbookID: addressbookID,
		SyncToken:     syncToken,
		Limit:         limit,
	}
	if muaID != "" {
		req.MuaID = &muaID
	}
	var reply syncAddressbookSinceReply
	if err := c.Call(ctx, MethodSyncAddressbookSince, req, &reply); err != nil {
		return SyncAddressbookSinceResult{}, fmt.Errorf("sync_addressbook_since: %w", err)
	}
	switch SyncAddressbookSinceOutcome(reply.Outcome) {
	case SyncAddressbookSinceOk:
		return SyncAddressbookSinceResult{
			Outcome:      SyncAddressbookSinceOk,
			Changed:      reply.Changed,
			Expunged:     reply.Expunged,
			NewSyncToken: reply.NewSyncToken,
			More:         reply.More,
			Stale:        reply.Stale,
		}, nil
	case SyncAddressbookSinceAddressbookNotFound:
		return SyncAddressbookSinceResult{Outcome: SyncAddressbookSinceAddressbookNotFound}, nil
	case SyncAddressbookSinceStale:
		return SyncAddressbookSinceResult{
			Outcome:      SyncAddressbookSinceStale,
			ServerModseq: reply.ServerModseq,
		}, nil
	default:
		return SyncAddressbookSinceResult{}, fmt.Errorf("sync_addressbook_since: unknown outcome %q", reply.Outcome)
	}
}

// ── report_log_events — the sidecar log plane's bridge leg ───────
//
// Mirrors libs/fauna-protocol/src/log_plane.rs. The Rust structs carry a
// `#[serde(flatten)] extra` map for additive evolution; Go omits it on the
// request (we never invent unknown keys) and ignores it on the reply, which
// is exactly what flatten-on-decode expects.

// LogEvent is one catalogued event, mirroring
// fauna_protocol::log_plane::SidecarLogEvent.
//
// Note what is NOT here: a source identifier. Nest stamps the ring entry's
// target as `<source>:<event>` using the *authenticated* identity behind this
// bridge's WS, so a compromised bridge cannot attribute a line to another
// service or to the nest itself.
type LogEvent struct {
	// TimestampMs is milliseconds since the Unix epoch, stamped at the source.
	// Nest clamps a future stamp to its own now (the merged admin view is
	// timestamp-ordered, so an unclamped far-future stamp would pin this source
	// permanently at the top of the admin's page).
	TimestampMs uint64 `cbor:"timestamp_ms"`
	// Level is "error", "warn", or "info". Anything else is dropped and counted
	// at admission — debug/trace detail belongs in the bridge's own stderr,
	// which the container log stream already captures.
	Level string `cbor:"level"`
	// Event is the stable catalogue identifier, `[a-z0-9_.-]`, <= 64 bytes.
	Event string `cbor:"event"`
	// Message is the rendered constant template. Interpolation is restricted to
	// the bounded value classes — see internal/logplane's package docs for the
	// rule and why it is load-bearing.
	Message string `cbor:"message"`
}

// reportLogEventsRequest mirrors ReportLogEventsRequest.
type reportLogEventsRequest struct {
	Events []LogEvent `cbor:"events"`
	// Dropped is how many events the source dropped from its own bounded queue
	// since the last accepted batch, so source-side loss is visible to the
	// admin rather than silent.
	Dropped uint64 `cbor:"dropped"`
}

// reportLogEventsReply mirrors ReportLogEventsReply — empty by design. The
// plane is strictly best-effort and must never back-pressure the bridge's real
// work, so nest reports nothing a source would act on; success is the Reply's
// ok=true, which Caller.Call already enforces.
type reportLogEventsReply struct{}

// ReportLogEvents ships one batch of catalogued events to nest.
//
// Callers are expected to be internal/logplane's flush, not application code:
// the plane's contract (bounded queue, drop-oldest accounting, disable on a
// server refusal) lives there in one audited copy. An error here is normal and
// non-fatal — a transport blip drops the batch, and an *error reply* is a
// permanent refusal (a same-image retry would fail identically), which
// logplane reads as "stop reporting for this session".
func ReportLogEvents(ctx context.Context, c Caller, events []LogEvent, dropped uint64) error {
	req := reportLogEventsRequest{Events: events, Dropped: dropped}
	var reply reportLogEventsReply
	if err := c.Call(ctx, MethodReportLogEvents, req, &reply); err != nil {
		return fmt.Errorf("report_log_events: %w", err)
	}
	return nil
}

// ── The `__index` rail, bridge plane (content-index.md § Where the index is built)

// ⚠ **`IndexBlobEntry` here is a tantivy index SEGMENT, not `IndexSegment`.**
// `IndexSegment` in this package means the pre-backend-2 per-message sealed
// *hint* (the thing `FetchIndexSegmentsSince` returns and `bodySearch` linearly
// scans). The two are unrelated mechanisms that happen to share the word, and
// they coexist during the S5 rollout — read the type, not the noun.
//
// This plane is `fauna.bridges.index_{record,list}`: BridgeMda-only and
// *explicitly targeted* (the MDA connects as its own service actor and names the
// MUA session's actor), unlike the self-scoped `fauna.index.{record,list}` a
// user's client calls. The MDA could not have been admitted to those: they carry
// no actor_id, so it would only ever have published into the bridge's own rail.
//
// Both kinds are mail/calendar-class only, enforced by the nest on the PATH —
// `record` refuses a master-class path and `list` omits master-class entries, so
// a leaked MUA credential reaches neither the master-key slices nor their
// per-kind segment counts.

// IndexBlobEntry mirrors the Rust `IndexBlobEntry`: one live `__index` virtual
// path and the sealed blob it currently points at.
type IndexBlobEntry struct {
	// Path is `__index`-relative *including* the `__index/` prefix — e.g.
	// `__index/mail/seg-00000001.idx` or `__index/manifest-mailcal.idx`.
	Path string `cbor:"path"`
	// BlobHash is the **hex** blake3 digest of the sealed blob, which is what
	// byteplane.DownloadBlob takes (the same bytes are also addressable by the
	// base32 CID they were PUT under; callers never convert between the two).
	BlobHash string `cbor:"blob_hash"`
	// SizeBytes is the sealed size, which the shared fold planner budgets on.
	SizeBytes int64 `cbor:"size_bytes"`
}

type bridgeIndexListRequest struct {
	ActorID []byte `cbor:"actor_id"`
	// Cursor resumes a paged walk; empty means the first page. Omitted from
	// the encoding when empty so a pre-paging nest sees exactly the request it
	// always saw.
	Cursor string `cbor:"cursor,omitempty"`
}

type bridgeIndexListReply struct {
	Entries []IndexBlobEntry `cbor:"entries"`
	// NextCursor is set iff more entries remain past this page. Empty on the
	// last page (including a single-page reply), so the walk below ends when
	// it is absent.
	NextCursor string `cbor:"next_cursor"`
}

// indexListPageCap bounds the walk below so a nest that echoed its own cursor
// back cannot spin this call forever. Far above any real rail: the nest cuts
// each page at the ~2 MiB frame budget, so this many pages is gigabytes of
// index listing.
const indexListPageCap = 4096

// BridgeIndexList enumerates `actorID`'s live mail/calendar-class `__index`
// blobs. An actor who has never published yields an empty slice — the first-run
// state the builder starts from with a fresh manifest, never an error.
//
// Walks the nest's pages to exhaustion and returns the complete set, so the
// FfiIndexRail contract ("everything the rail lists, in one call") holds. The
// nest pages this read because the `__index` segment-path count is deliberately
// unbounded — past the frame cap an unpaged reply was undeliverable, and a large
// enough index could never be listed again.
//
// Walk to the cursor's ABSENCE, never to the first empty page: this plane's
// master-class filter runs after the nest reads a page, so a page can legitimately
// come back empty with the cursor still advancing.
func BridgeIndexList(ctx context.Context, c Caller, actorID []byte) ([]IndexBlobEntry, error) {
	var out []IndexBlobEntry
	cursor := ""
	for page := 0; ; page++ {
		if page >= indexListPageCap {
			return nil, fmt.Errorf("bridges.index_list: walk exceeded %d pages", indexListPageCap)
		}
		req := bridgeIndexListRequest{ActorID: actorID, Cursor: cursor}
		var reply bridgeIndexListReply
		if err := c.Call(ctx, MethodBridgeIndexList, req, &reply); err != nil {
			return nil, fmt.Errorf("bridges.index_list: %w", err)
		}
		out = append(out, reply.Entries...)
		if reply.NextCursor == "" || reply.NextCursor == cursor {
			return out, nil
		}
		cursor = reply.NextCursor
	}
}

type bridgeIndexRecordRequest struct {
	ActorID   []byte `cbor:"actor_id"`
	Path      string `cbor:"path"`
	BlobHash  string `cbor:"blob_hash"`
	SizeBytes int64  `cbor:"size_bytes"`
}

type bridgeIndexRecordReply struct {
	Seq int64 `cbor:"seq"`
}
