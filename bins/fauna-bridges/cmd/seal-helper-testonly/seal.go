// Command seal-helper-testonly is a TEST-ONLY shim that performs the
// admin/user *client-side seal* step of the wrapped-blob provisioning
// flow, so the Python e2e harness (which has no in-process binding to
// the shared Rust crypto) can drive it as a subprocess.
//
// # Why this exists (and why it is NOT a production CLI)
//
// A product invariant: nest configuration — every key,
// cert, and policy — is set from a Fauna *client UI*, never from a CLI
// or config file. In production the seal step runs inside the admin's
// (or user's) client: the client calls the shared-Rust `seal_*_blob`
// UniFFI exports (`libs/fauna-ffi/src/mail.rs`, backed by
// `libs/fauna-mls/src/wrapped_blob/`) and provisions the sealed bytes
// over WS-RPC (`fauna.bridges.provision_tls_cert_blob` /
// `provision_wrapped_submission_token`). The admin's plaintext + private
// keys never transit the wire.
//
// This binary wraps the *same* production seal functions — they are
// present in the bridge's Go FFI binding only because uniffi-bindgen-go
// emits all of fauna-ffi's surface (the bridge itself only ever Opens
// these blobs, never Seals). It exists solely because the e2e test must
// perform the client's seal step at runtime against the test's
// *ephemeral* bridge X25519 pubkey, which a static fixture can't
// pre-bake. It never ships in any deployment artifact: `mail-bridge-build`
// builds only `./cmd/fauna-mail-bridge`; this binary is built by the
// `seal-helper-build` just recipe and invoked only from
// `tests/e2e-unified`.
//
// Zero test/prod divergence: the canonical DAG-CBOR bundle encoding goes
// through the bridge's own `internal/dagcbor` codec (length-first key
// sort, the same `cbor.SortLengthFirst` that matches Rust's
// `fauna_cbor::encode_canonical` / `serde_ipld_dagcbor`), and the seal +
// HPKE/AEAD steps are the shared-Rust functions via the FFI. See
// `docs/goal/behavior/mail-bridge-lifecycle.md` § TLS provisioning.
package main

import (
	"crypto/ed25519"
	"encoding/hex"
	"fmt"

	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// The two inner-bundle structs mirror the Rust plaintext shapes
// (`fauna_mls::wrapped_blob::format::TlsCertBundle` and
// `submission_token::SubmissionToken`) field-for-field. The `cbor`
// struct tags reproduce the serde field names exactly; `internal/dagcbor`
// then sorts the map keys length-first-then-bytewise, matching the Rust
// `to_canonical_bytes()` encoder.
//
// For TLS the canonical-ness of these bytes is not load-bearing:
// the Rust `seal_tls_cert_blob` FFI re-decodes (ciborium, order-agnostic) and
// re-encodes canonically before sealing. For the submission token it IS
// load-bearing — the Ed25519 signature below is computed over the
// canonical bytes, and the bridge verifies that signature against its own
// `to_canonical_bytes()` re-encode — so the Go and Rust encoders must
// agree byte-for-byte. `seal_test.go`'s round-trip pins that agreement.

type tlsCertBundle struct {
	CertChain []byte `cbor:"cert_chain"` // PEM cert chain (X509KeyPair input)
	PrivKey   []byte `cbor:"priv_key"`   // PEM private key
	ExpiresAt uint64 `cbor:"expires_at"`
	IssuedAt  uint64 `cbor:"issued_at"`
}

type submissionToken struct {
	ActorID           []byte `cbor:"actor_id"` // == the user's 32-byte Ed25519 verifying key
	CredentialID      string `cbor:"credential_id"`
	IssuedAt          uint64 `cbor:"issued_at"`
	ExpiresAt         uint64 `cbor:"expires_at"`
	MaxRecipients     uint32 `cbor:"max_recipients"`
	MaxMessagesPerDay uint32 `cbor:"max_messages_per_day"`
	UserSig           []byte `cbor:"user_sig"` // Ed25519 sig over the canonical bytes with this field zeroed
}

// sealTLSCert mirrors the admin client's TLS-cert seal: serialize a
// TlsCertBundle to canonical DAG-CBOR, then HPKE-seal it to the bridge's
// X25519 pubkey via the shared-Rust seal. Output is the canonical
// `TlsCertBlob` bytes ready for `fauna.bridges.provision_tls_cert_blob`.
func sealTLSCert(certChainPEM, privKeyPEM []byte, bridgeRole, bridgeID, domain string, issuedAt, expiresAt uint64, recipientPubkey []byte) ([]byte, error) {
	bundleBytes, err := dagcbor.Marshal(tlsCertBundle{
		CertChain: certChainPEM,
		PrivKey:   privKeyPEM,
		ExpiresAt: expiresAt,
		IssuedAt:  issuedAt,
	})
	if err != nil {
		return nil, fmt.Errorf("encode tls-cert bundle: %w", err)
	}
	return fauna_ffi.SealTlsCertBlob(bundleBytes, bridgeRole, bridgeID, domain, recipientPubkey)
}

// sealSubmissionToken mirrors the user client's submission-token seal:
// build a SubmissionToken, Ed25519-sign it with the user's identity key
// (the placeholder pattern — sign over the canonical bytes with user_sig
// zeroed), then AEAD-seal the signed token under the MUA credential via
// the shared-Rust seal.
//
// actor_id is derived from signingSeed so the codebase invariant
// (actor_id == the 32-byte Ed25519 verifying key) holds by construction:
// the bridge reconstructs the verifying key from the bridge-resolved
// actor_id to check the signature, so a mismatch would fail AUTH.
//
// credentialKind is "plain" (Argon2id, for AUTH=PLAIN) or "oauthbearer"
// (HKDF, for AUTH=OAUTHBEARER); the KDF params are the library defaults
// (kdf_params=nil), matching production exactly.
func sealSubmissionToken(signingSeed []byte, credentialID string, issuedAt, expiresAt uint64, maxRecipients, maxMessagesPerDay uint32, credentialKind string, credential []byte) ([]byte, error) {
	if len(signingSeed) != ed25519.SeedSize {
		return nil, fmt.Errorf("signing seed must be %d bytes, got %d", ed25519.SeedSize, len(signingSeed))
	}
	priv := ed25519.NewKeyFromSeed(signingSeed)
	actorID := []byte(priv.Public().(ed25519.PublicKey))

	tok := submissionToken{
		ActorID:           actorID,
		CredentialID:      credentialID,
		IssuedAt:          issuedAt,
		ExpiresAt:         expiresAt,
		MaxRecipients:     maxRecipients,
		MaxMessagesPerDay: maxMessagesPerDay,
		UserSig:           make([]byte, ed25519.SignatureSize), // 64 zero bytes placeholder
	}

	// Sign over the canonical bytes with user_sig zeroed (the placeholder
	// pattern from SubmissionToken::sign), then stamp the signature in.
	placeholderBytes, err := dagcbor.Marshal(tok)
	if err != nil {
		return nil, fmt.Errorf("encode token placeholder: %w", err)
	}
	tok.UserSig = ed25519.Sign(priv, placeholderBytes)
	signedBytes, err := dagcbor.Marshal(tok)
	if err != nil {
		return nil, fmt.Errorf("encode signed token: %w", err)
	}

	return fauna_ffi.SealSubmissionTokenBlob(signedBytes, actorID, credentialID, credentialKind, credential, nil)
}

// sealWrappedMsek mirrors the user client's wrapped-MSEK seal (mail-credentials.md
// § KDF choice): AEAD-seal the 32-byte MSEK under the MUA credential, keyed via
// Argon2id (credential_kind="plain") or HKDF-SHA-256 (oauthbearer), bound to
// (actor_id, credential_id). `nil` kdf_params selects the per-kind library
// default. The MDA bridge opens this blob at MUA-AUTH via UnwrapMsekBlob, which
// cross-checks the KDF family against the AUTH mechanism — so a "plain"-sealed
// blob is the one PLAIN/Argon2id auth resolves to (imap-server.md § Authentication).
func sealWrappedMsek(msek, actorID []byte, credentialID, credentialKind string, credential []byte) ([]byte, error) {
	return fauna_ffi.SealWrappedMsekBlob(msek, actorID, credentialID, credentialKind, credential, nil)
}

// deriveRecipientPubkey mirrors the user client's recipient-pubkey
// registration step (mail-credentials.md; the read-side recipient key is a
// standing MSEK-derived Fauna HPKE key): derive the keypair from MSEK and
// return its public half — the value the client provisions via
// `fauna.bridges.provision_recipient_mls_pubkey`, and the key the MTA / IMAP
// APPEND / CalDAV PUT seal recipient bodies to. The derivation is
// deterministic in MSEK, so the pubkey this returns matches the leaf secret
// `sealMlsSnapshot` (below) embeds for the same MSEK.
func deriveRecipientPubkey(msek []byte) ([]byte, error) {
	kp, err := fauna_ffi.DeriveRecipientHpkeKeypair(msek)
	if err != nil {
		return nil, fmt.Errorf("derive recipient hpke keypair: %w", err)
	}
	return kp.Pubkey, nil
}

// sealMlsSnapshot mirrors the user client's MLS-snapshot provisioning step:
// build the snapshot plaintext from the MSEK with the SAME shared builder the
// client uses (`build_mls_snapshot_plaintext`, via
// `EncodeMlsSnapshotPlaintextFromMseks`), then AEAD-seal it under MSEK bound to
// actor_id. Output is the canonical MlsSnapshotBlob bytes ready for
// `fauna.bridges.provision_mls_snapshot_blob`. Its leaf keypair is the SAME
// MSEK-derived one `deriveRecipientPubkey` returns, and it carries that MSEK's
// ML-KEM decaps half too, so the MDA opens a body sealed to the registered
// recipient either classical or X-Wing — the latter being every inbound message
// once the recipient's ML-KEM ek is published (`deriveRecipientMlkemEk`). A
// classical-only snapshot here blinded the MDA to all later mail for a
// recipient a test had upgraded to hybrid.
func sealMlsSnapshot(msek, actorID []byte) ([]byte, error) {
	plaintext, err := fauna_ffi.EncodeMlsSnapshotPlaintextFromMseks([][]byte{msek})
	if err != nil {
		return nil, fmt.Errorf("encode mls snapshot plaintext: %w", err)
	}
	return fauna_ffi.SealMlsSnapshotBlob(plaintext, actorID, msek)
}

// sealSpamModel mirrors the tier-1 spam-model client/agent seal (mail-spam.md
// § Encrypted-mode interaction): AEAD-seal a plaintext `fauna_mail::spam::SpamModel`
// serde_json blob to the actor's MSEK-derived recipient HPKE pubkey — the SAME
// seal the MDA's agent-side `\Junk`-train re-seal (`imap/store.go`
// trainJunkAgentSide → mailfauna.EncryptToRecipientHybrid) and the Fauna app's
// `apply_and_reseal` perform. This helper seals under the classical X25519
// suite to the recipient pubkey `deriveRecipientPubkey` returns; the reader
// opens either suite with the same MSEK-derived key material.
//
// Output is the opaque `spam_models.model_json` blob a tier_3 test pre-seeds so
// `fetch_spam_model` reports `stored_sealed=true` (nest `is_sealed_model_blob`:
// non-empty + not serde_json → sealed) and the MDA `\Junk`-train dispatches to
// the agent-side open→mutate→re-seal path, the session's
// `MailRecordOpener.Open(sealedModel)` recovering this exact plaintext under the
// session capability. The MSEK behind the recipient's MLS snapshot is the same one this
// seal targets, so the open succeeds — pinned in-process by
// `TestSealSpamModelRoundTrip`.
func sealSpamModel(msek, modelJSON []byte) ([]byte, error) {
	pubkey, err := deriveRecipientPubkey(msek)
	if err != nil {
		return nil, fmt.Errorf("derive recipient pubkey: %w", err)
	}
	return sealMailRecord(modelJSON, pubkey)
}

// sealMailRecord seals arbitrary plaintext to a 32-byte X25519 recipient
// pubkey, returning the canonical `MailRecordEnvelope` bytes — the one
// recipient seal every sealed mail/calendar/card body rides (the Go MDA's
// `EncryptToRecipient`, the client's `seal_event_body`), and the only body
// shape the nest's `put_event_ciphertext`/`put_card_ciphertext` accept
// (`SealedRecordBytes::verify`). A tier_3 test PUTs the output as an event
// body when what it asserts is the nest's handling of the bytes (their size,
// their placement), never their plaintext.
func sealMailRecord(plaintext, recipientPubkey []byte) ([]byte, error) {
	return fauna_ffi.SealToRecipient(plaintext, recipientPubkey)
}

// sealSpamModelCopy seals a plaintext `SpamModel` serde_json blob to the
// aggregation holder's X25519 pubkey, producing the canonical `SpamModelCopyBlob`
// bytes a contributor attaches to `put_spam_model.holder_copy` (mail-spam.md
// § Encrypted-mode interaction). The seal-side twin of `AggregateSpamModelCopies`
// (which the Go holder drain calls) — the tier_3 test seeds this beside the
// sealed model so the publish drain merges it. `ownerActorID` binds the AAD;
// `holderMlkemEk` (nil/empty ⇒ classical X25519) selects the X-Wing suite.
func sealSpamModelCopy(modelJSON, ownerActorID, holderX25519Pubkey, holderMlkemEk []byte) ([]byte, error) {
	var ek *[]byte
	if len(holderMlkemEk) > 0 {
		ek = &holderMlkemEk
	}
	return fauna_ffi.SealSpamModelCopy(modelJSON, ownerActorID, holderX25519Pubkey, ek)
}

// mintGrant mirrors the user client's capability-grant mint (design § Phase 2
// Step 2 § 2.1): derive the recipient-mail X-Wing capability secret from the
// owner's MSEK (the content.read{mail} payload — the `32 + 2400`-byte
// `x25519_secret ∥ ml-kem-dk` superset the production `derive_scope_payload`
// wraps; its X25519 half is the classical mail secret, so it opens BOTH
// classical and X-Wing-sealed mail records), then build a GrantBlob granting the
// MDA holder content.read{mail} (key-bearing) + content.label-write (keyless),
// HPKE-sealed to the holder. Output is the canonical GrantBlob bytes ready for
// `fauna.capabilities.mint` (`MintGrantRequest { grant_blob }`).
//
// `holderMlkemEk` selects the wrap suite (PQ-CAP-3/4), matching the client
// machine mint's holder-ek-present gate: a valid 1184-B ek ⇒ the X-Wing wrap (so
// a harvested `capability_grants` row is not a CRQC-openable bypass of the closed
// mail seal); nil/empty ⇒ the classical X25519 wrap. The mail payload is the
// `32 + 2400` superset EITHER way (matching production) — the ek gates only the
// wrap, not the payload.
//
// Master-key regime (design Q-expiry: ship honest master-key first): one
// WrappedScopeKey with epoch None; the window is advisory. This is the client
// mint the all-6-client capability settings page performs in production; the seal-helper drives it so the tier_3
// re-score-drain test has a real user-minted grant without the client UI.
//
// labelerID (optional, 32 bytes) confines the grant to ONE community labeler:
// both tuples — and so every wrap — carry `factor = labeler:<hex>`, the
// per-labeler grant a subscription over sealed mail mints in production
// (`fauna_client_capabilities::mint_bounded_mail_labeler_grant`). nil ⇒ the composed "read and filter my mail" role, whose wraps
// license the built-in perimeter factors and no labeler.
func mintGrant(ownerActorID, grantID, holderPubkey, msek, holderMlkemEk []byte, epochStart, epochEnd uint64, labelerID []byte) ([]byte, error) {
	material, err := fauna_ffi.DeriveRecipientMailXwingMaterial(msek)
	if err != nil {
		return nil, fmt.Errorf("derive recipient mail xwing material: %w", err)
	}
	mailReadKey := material.CapabilitySecret // the 32+2400-byte content.read{mail} payload
	mailKind := "mail"
	factor, err := labelerFactorOf(labelerID)
	if err != nil {
		return nil, err
	}
	scopes := []fauna_ffi.CapabilityScopeInput{
		{
			Class:   "content.read",
			Kind:    &mailKind,
			Tier:    nil,
			Factor:  factor,
			Payload: &mailReadKey,
		},
		{
			Class:   "content.label-write",
			Kind:    nil,
			Tier:    nil,
			Factor:  factor,
			Payload: nil,
		},
	}
	var holderEk *[]byte
	if len(holderMlkemEk) > 0 {
		holderEk = &holderMlkemEk
	}
	return fauna_ffi.BuildCapabilityGrantBlob(ownerActorID, grantID, holderPubkey, holderEk, epochStart, epochEnd, scopes)
}

// labelerFactorOf maps an optional 32-byte labeler id to the `labeler:<hex>`
// bus factor a per-labeler grant's tuples carry (`fauna_core::scoring::
// labeler_factor`), or nil for the composed role.
func labelerFactorOf(labelerID []byte) (*string, error) {
	if len(labelerID) == 0 {
		return nil, nil
	}
	if len(labelerID) != 32 {
		return nil, fmt.Errorf("labeler_id must be 32 bytes, got %d", len(labelerID))
	}
	factor := "labeler:" + hex.EncodeToString(labelerID)
	return &factor, nil
}

// mintBoundedGrant mirrors the user client's BOUNDED mail mint
// (`fauna_client_capabilities::mint_bounded_mail_grant`; content-sealing-epochs
// design § 2): one wrapped per-epoch capability secret per epoch intersecting
// the `[windowStart, windowEnd]` unix-seconds window (`WrappedScopeKey.epoch =
// Some(e)`), plus the keyless content.label-write scope — and NEVER the
// standing secret (the bounded-XOR-master-key mint policy). Built so the Go
// side can unseal a REAL bounded blob and pin the generated
// `UnsealedScopeKey.Epoch` lift end-to-end (previously the
// only Go-side grants were master-key-shaped, leaving a fail-open-capable
// bounded→master masquerade unpinned), and so tier_3 harnesses can drive a
// real bounded user-mint without the client UI.
//
// labelerID (optional) confines the grant to one community labeler exactly as
// in mintGrant.
func mintBoundedGrant(ownerActorID, grantID, holderPubkey, msek, holderMlkemEk []byte, windowStart, windowEnd uint64, labelerID []byte) ([]byte, error) {
	var holderEk *[]byte
	if len(holderMlkemEk) > 0 {
		holderEk = &holderMlkemEk
	}
	factor, err := labelerFactorOf(labelerID)
	if err != nil {
		return nil, err
	}
	// No prior MSEK generations: the seal-helper's fixtures model the
	// no-rotation case (the rotation-heal amendment's boundary coverage is
	// pinned Rust-side; a Go-side rotated-mint fixture can pass priors here).
	return fauna_ffi.BuildBoundedMailGrantBlob(
		ownerActorID, grantID, holderPubkey, holderEk, windowStart, windowEnd, msek,
		[]fauna_ffi.BoundedMailPriorGeneration{}, true, factor)
}

// mintSpamModelGrant builds the KEYLESS `content.read{spam-model}` capability
// grant to the aggregation holder — the grant the all-6-client
// contribute-baseline toggle mints in production (mail-spam.md § Encrypted-mode
// interaction, the keyless `content.read{spam-model}` shape). Unlike mintGrant it
// wraps NO key material: `spam-model` is a keyless read kind
// (`derive_scope_payload → None`, like `content.label-write`), so the grant is
// pure audit/revocation surface and carries zero `WrappedScopeKey`s. The holder
// opens the contributor's `SpamModelCopyBlob` with its OWN service-user key halves
// (aggregate_spam_model_copies), never a wrapped contributor secret — so
// contributing to the deployment spam baseline conveys no standing read of the
// contributor's mailbox. That is the whole security point of this scope, proven
// end-to-end by the tier_3 spam-baseline-drain negative (the holder bearing only
// this grant cannot open the contributor's mail).
//
// The wrap suite is moot for a keyless grant (there is nothing to wrap), so this
// takes no holder ML-KEM ek — unlike mintGrant, whose key-bearing content.read{mail}
// tuple the ek would upgrade to X-Wing. The nest's build_grant_blob REJECTS any
// key-bearing spam-model tuple (a spam-model grant with a payload never mints), so
// keyless is the only valid shape here.
func mintSpamModelGrant(ownerActorID, grantID, holderPubkey []byte, epochStart, epochEnd uint64) ([]byte, error) {
	spamModelKind := "spam-model"
	scopes := []fauna_ffi.CapabilityScopeInput{
		{
			Class:   "content.read",
			Kind:    &spamModelKind,
			Tier:    nil,
			Payload: nil, // keyless — the security-critical property
		},
	}
	return fauna_ffi.BuildCapabilityGrantBlob(ownerActorID, grantID, holderPubkey, nil, epochStart, epochEnd, scopes)
}

// deriveRecipientMlkemEk returns the 1184-byte ML-KEM-768 encapsulation key of the
// actor's standing recipient-mail X-Wing keypair (derived from MSEK). The user's
// client publishes it alongside its X25519 recipient pubkey via
// `fauna.bridges.provision_recipient_mls_pubkey` (`mlkem_ek` field, S3c), so the
// MTA seals inbound mail to it under the X-Wing suite. The e2e harness's one
// recipient-key writer (`helpers/recipient_seal_key.py`) drives this for every
// recipient it provisions.
func deriveRecipientMlkemEk(msek []byte) ([]byte, error) {
	material, err := fauna_ffi.DeriveRecipientMailXwingMaterial(msek)
	if err != nil {
		return nil, fmt.Errorf("derive recipient mail xwing material: %w", err)
	}
	return material.MlkemEk, nil
}

// deriveBridgeMlkemEk returns the 1184-byte ML-KEM-768 encapsulation key a bridge
// service-user (a capability-grant holder) derives from its keyfile Ed25519 seed
// (PQ-CAP-2 — the SAME derivation the bridge publishes at `register_service_user`
// and re-derives its dk from to open hybrid wraps). The tier_3 hybrid-drain harness
// drives this to obtain the MDA holder's ek so it can mint an X-Wing-wrapped grant
// to it, faithful to the production `fetch_bridge_pubkey`-then-mint path.
func deriveBridgeMlkemEk(ed25519Seed []byte) ([]byte, error) {
	kp, err := fauna_ffi.DeriveBridgeServiceUserMlkem768(ed25519Seed)
	if err != nil {
		return nil, fmt.Errorf("derive bridge service-user mlkem: %w", err)
	}
	return kp.MlkemEk, nil
}

// publishLabeler mirrors the (deferred) all-6-client publish UI's build-and-sign
// step (labeler-registry design § 5): given the publisher's 32-byte Ed25519
// signing seed (whose verifying key IS the labeler's `algorithm_id` —
// public-key-is-identity) and the WASM module bytes, produce the canonical-CBOR
// `AlgorithmLabeler` metadata_blob the `fauna.labelers.publish` RPC carries.
//
// All crypto + canonical encoding runs in shared Rust via
// `BuildSignedLabelerMetadata` (it sets `algorithm_id` from the seed, computes
// `wasm_hash`/`wasm_size`, and signs the signature-zeroed canonical dag-cbor —
// the same `sign_labeler_metadata` twin of `verify_labeler_metadata`), so the
// blob passes the nest's `validate_labeler_publish` → `verify_labeler_metadata`
// by construction, with no Go-side CBOR to drift. The tier_3
// `test_capability_labeler_drain` harness drives it so the drain test has a real
// signed labeler without the client UI (E2E rule 8 carve-out (b) — arranging the
// world, not the mutation under test).
func publishLabeler(signingSeed, wasmBytes []byte, version uint64, needsText, needsHashtags, needsMediaMetadata, needsAuthor bool, maxMemoryBytes, maxCpuMicroseconds, updatedAt uint64) ([]byte, error) {
	// The fifth declared input, attachment bytes, is a community-room
	// facet no mail-drain fixture asks for (the mail holder hands an empty
	// facet); the tier_3 drain publishes flagless modules.
	return fauna_ffi.BuildSignedLabelerMetadata(
		signingSeed, wasmBytes, version,
		needsText, needsHashtags, needsMediaMetadata, needsAuthor, false,
		maxMemoryBytes, maxCpuMicroseconds, updatedAt,
	)
}

// encodeTextModelArtifact mints the canonical dag-cbor `text-model` artifact
// bytes that ride in `wasm_bytes` — at a caller-chosen tokenizer `version`.
//
// The version is the point: the "needs a newer app" badge can only be proven by
// an artifact THIS build does not implement, and no app UI can publish one (the
// shared publish lifecycle stamps the current version itself, deliberately). So
// the tier_3 journey mints it here and publishes it through the ordinary raw
// `fauna.labelers.publish` path — E2E convention 8 carve-out (b) again, since
// the mutation under test is the catalog *render*, not the publish.
//
// All canonicalization and validation stay in shared Rust
// (`EncodeTextModelArtifactAtVersion` delegates to the production
// `build_text_model_artifact` and then overwrites only the version), so there is
// no Go-side CBOR to drift from the shape the nest gate enforces.
func encodeTextModelArtifact(version uint16, name *string, moreDocs, lessDocs uint32, ngrams []textModelNgramParam) ([]byte, error) {
	rows := make([]fauna_ffi.FfiTextModelNgram, 0, len(ngrams))
	for _, n := range ngrams {
		rows = append(rows, fauna_ffi.FfiTextModelNgram{
			Ngram: n.Ngram,
			More:  n.More,
			Less:  n.Less,
		})
	}
	return fauna_ffi.EncodeTextModelArtifactAtVersion(version, name, moreDocs, lessDocs, rows)
}
