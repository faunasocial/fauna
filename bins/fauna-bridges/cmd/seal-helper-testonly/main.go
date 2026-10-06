package main

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"os"
)

// main is the subprocess entry point used by tests/e2e-unified. It reads
// a single JSON params object on stdin, performs one client-side seal,
// and writes the resulting canonical-CBOR blob bytes as base64 (one line)
// to stdout. Any error goes to stderr and exits non-zero so the Python
// caller's subprocess.run(check=True) surfaces it.
//
// Usage: seal-helper-testonly <seal-tls-cert|seal-submission-token|seal-wrapped-msek|derive-recipient-pubkey|derive-recipient-mlkem-ek|derive-bridge-mlkem-ek|seal-mls-snapshot|seal-mail-record|mint-grant|mint-bounded-grant|mint-spam-model-grant|seal-spam-model|seal-spam-model-copy|publish-labeler>
func main() {
	if len(os.Args) != 2 {
		fail("usage: seal-helper-testonly <seal-tls-cert|seal-submission-token|seal-wrapped-msek|derive-recipient-pubkey|derive-recipient-mlkem-ek|derive-bridge-mlkem-ek|seal-mls-snapshot|seal-mail-record|mint-grant|mint-bounded-grant|mint-spam-model-grant|seal-spam-model|seal-spam-model-copy|publish-labeler> (JSON on stdin → base64 blob on stdout)")
	}
	raw, err := io.ReadAll(os.Stdin)
	if err != nil {
		fail("read stdin: %v", err)
	}

	var blob []byte
	switch os.Args[1] {
	case "seal-tls-cert":
		blob, err = runSealTLSCert(raw)
	case "seal-submission-token":
		blob, err = runSealSubmissionToken(raw)
	case "seal-wrapped-msek":
		blob, err = runSealWrappedMsek(raw)
	case "derive-recipient-pubkey":
		blob, err = runDeriveRecipientPubkey(raw)
	case "derive-recipient-mlkem-ek":
		blob, err = runDeriveRecipientMlkemEk(raw)
	case "derive-bridge-mlkem-ek":
		blob, err = runDeriveBridgeMlkemEk(raw)
	case "seal-mls-snapshot":
		blob, err = runSealMlsSnapshot(raw)
	case "mint-bounded-grant":
		blob, err = runMintBoundedGrant(raw)
	case "mint-grant":
		blob, err = runMintGrant(raw)
	case "mint-spam-model-grant":
		blob, err = runMintSpamModelGrant(raw)
	case "seal-mail-record":
		blob, err = runSealMailRecord(raw)
	case "seal-spam-model":
		blob, err = runSealSpamModel(raw)
	case "seal-spam-model-copy":
		blob, err = runSealSpamModelCopy(raw)
	case "publish-labeler":
		blob, err = runPublishLabeler(raw)
	case "encode-text-model-artifact":
		blob, err = runEncodeTextModelArtifact(raw)
	default:
		fail("unknown subcommand %q", os.Args[1])
	}
	if err != nil {
		fail("%v", err)
	}

	fmt.Println(base64.StdEncoding.EncodeToString(blob))
}

// ── Per-subcommand JSON param shapes ──────────────────────────────
//
// All binary fields arrive base64-encoded (std encoding) because JSON
// has no byte type; the Python caller base64-encodes raw bytes.

type tlsCertParams struct {
	BridgeRole            string `json:"bridge_role"`
	BridgeID              string `json:"bridge_id"`
	Domain                string `json:"domain"`
	RecipientX25519PubB64 string `json:"recipient_x25519_pubkey_b64"`
	CertChainPEMB64       string `json:"cert_chain_pem_b64"`
	PrivKeyPEMB64         string `json:"priv_key_pem_b64"`
	IssuedAt              uint64 `json:"issued_at"`
	ExpiresAt             uint64 `json:"expires_at"`
}

type submissionTokenParams struct {
	SigningSeedB64    string `json:"signing_seed_b64"` // 32-byte Ed25519 seed; actor_id derived from it
	CredentialID      string `json:"credential_id"`
	CredentialKind    string `json:"credential_kind"` // "plain" | "oauthbearer"
	CredentialB64     string `json:"credential_b64"`  // the MUA secret bytes
	IssuedAt          uint64 `json:"issued_at"`
	ExpiresAt         uint64 `json:"expires_at"`
	MaxRecipients     uint32 `json:"max_recipients"`
	MaxMessagesPerDay uint32 `json:"max_messages_per_day"`
}

func runSealTLSCert(raw []byte) ([]byte, error) {
	var p tlsCertParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-tls-cert params: %w", err)
	}
	pub, err := decodeB64(p.RecipientX25519PubB64, "recipient_x25519_pubkey_b64")
	if err != nil {
		return nil, err
	}
	certChain, err := decodeB64(p.CertChainPEMB64, "cert_chain_pem_b64")
	if err != nil {
		return nil, err
	}
	privKey, err := decodeB64(p.PrivKeyPEMB64, "priv_key_pem_b64")
	if err != nil {
		return nil, err
	}
	return sealTLSCert(certChain, privKey, p.BridgeRole, p.BridgeID, p.Domain, p.IssuedAt, p.ExpiresAt, pub)
}

// wrappedMsekParams seals a 32-byte MSEK under a MUA credential for the test
// MDA recipient's IMAP/CalDAV AUTH (mail-credentials.md § KDF choice). The
// resulting WrappedMsekBlob is uploaded via fauna.bridges.provision_wrapped_mls_blob
// and opened by the bridge at MUA-AUTH (imap-server.md § Authentication).
type wrappedMsekParams struct {
	MsekB64        string `json:"msek_b64"`        // 32-byte Master Secret
	ActorIDB64     string `json:"actor_id_b64"`    // 32-byte actor_id (Ed25519 verify key)
	CredentialID   string `json:"credential_id"`   // "default"
	CredentialKind string `json:"credential_kind"` // "plain" | "oauthbearer"
	CredentialB64  string `json:"credential_b64"`  // the MUA secret (password) bytes
}

func runSealWrappedMsek(raw []byte) ([]byte, error) {
	var p wrappedMsekParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-wrapped-msek params: %w", err)
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	actorID, err := decodeB64(p.ActorIDB64, "actor_id_b64")
	if err != nil {
		return nil, err
	}
	credential, err := decodeB64(p.CredentialB64, "credential_b64")
	if err != nil {
		return nil, err
	}
	return sealWrappedMsek(msek, actorID, p.CredentialID, p.CredentialKind, credential)
}

// deriveRecipientPubkeyParams derives the actor's standing MSEK-derived
// recipient HPKE pubkey (the value provisioned via
// `fauna.bridges.provision_recipient_mls_pubkey` — the MTA/APPEND inbound
// seal target). Output is the 32-byte pubkey (base64), NOT a sealed blob.
type deriveRecipientPubkeyParams struct {
	MsekB64 string `json:"msek_b64"` // 32-byte Master Secret
}

// mlsSnapshotParams seals a v1 MlsSnapshotPlaintext carrying the actor's
// MSEK-derived leaf init keypair, AEAD-bound to actor_id, ready for
// `fauna.bridges.provision_mls_snapshot_blob`. The MDA AEAD-unwraps it at
// MUA-AUTH (`cap.Decrypt`) and the leaf secret it carries opens the
// MTA/APPEND-sealed bodies the registered recipient pubkey was the target of.
type mlsSnapshotParams struct {
	MsekB64    string `json:"msek_b64"`     // 32-byte Master Secret (same as the wrapped-MSEK blob)
	ActorIDB64 string `json:"actor_id_b64"` // 32-byte actor_id the snapshot AAD binds to
}

func runDeriveRecipientPubkey(raw []byte) ([]byte, error) {
	var p deriveRecipientPubkeyParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse derive-recipient-pubkey params: %w", err)
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	return deriveRecipientPubkey(msek)
}

func runSealMlsSnapshot(raw []byte) ([]byte, error) {
	var p mlsSnapshotParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-mls-snapshot params: %w", err)
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	actorID, err := decodeB64(p.ActorIDB64, "actor_id_b64")
	if err != nil {
		return nil, err
	}
	return sealMlsSnapshot(msek, actorID)
}

// mintGrantParams mints a user-minted capability grant to an MDA capability
// holder (design § Phase 2 Step 2). Mirrors what the all-6-client capability
// settings page will do; the tier_3
// re-score-drain harness drives it as a test precondition (E2E rule 8 carve-out
// (b)). The mail-read payload is derived internally from the owner's MSEK — the
// caller never handles a raw content key — and the grant declares
// content.read{mail} (key-bearing) + content.label-write (keyless).
type mintGrantParams struct {
	OwnerActorIDB64 string `json:"owner_actor_id_b64"` // 32-byte owner actor_id (Ed25519 verify key)
	GrantIDB64      string `json:"grant_id_b64"`       // 16-byte caller-namespaced grant id
	HolderPubkeyB64 string `json:"holder_pubkey_b64"`  // 32-byte MDA holder x25519 pubkey (the wrap target)
	MsekB64         string `json:"msek_b64"`           // 32-byte owner MSEK (mail-read HPKE secret derived from it)
	// HolderMlkemEkB64 (optional) is the holder's published 1184-byte ML-KEM ek.
	// Present ⇒ the grant wrap rides the X-Wing suite (PQ-CAP-4); absent/"" ⇒ the
	// classical X25519 wrap. The mail payload is the 32+2400 superset either way.
	HolderMlkemEkB64 string `json:"holder_mlkem_ek_b64,omitempty"`
	EpochStart       uint64 `json:"epoch_start"` // advisory window start (unix seconds)
	EpochEnd         uint64 `json:"epoch_end"`   // advisory window end (unix seconds)
	// LabelerIDB64 (optional) is a 32-byte labeler id. Present ⇒ the grant is
	// the PER-LABELER shape (every tuple + wrap carries `factor =
	// labeler:<hex>`, licensing that labeler alone); absent ⇒ the composed
	// "read and filter my mail" role, which licenses no community labeler.
	LabelerIDB64 string `json:"labeler_id_b64,omitempty"`
}

func runMintGrant(raw []byte) ([]byte, error) {
	var p mintGrantParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse mint-grant params: %w", err)
	}
	owner, err := decodeB64(p.OwnerActorIDB64, "owner_actor_id_b64")
	if err != nil {
		return nil, err
	}
	grantID, err := decodeB64(p.GrantIDB64, "grant_id_b64")
	if err != nil {
		return nil, err
	}
	holderPub, err := decodeB64(p.HolderPubkeyB64, "holder_pubkey_b64")
	if err != nil {
		return nil, err
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	// Optional: absent/"" ⇒ nil ⇒ classical wrap (the base64 decode of "" is a
	// zero-length slice, which mintGrant treats as "no ek").
	holderMlkemEk, err := decodeB64(p.HolderMlkemEkB64, "holder_mlkem_ek_b64")
	if err != nil {
		return nil, err
	}
	labelerID, err := decodeB64(p.LabelerIDB64, "labeler_id_b64")
	if err != nil {
		return nil, err
	}
	return mintGrant(owner, grantID, holderPub, msek, holderMlkemEk, p.EpochStart, p.EpochEnd, labelerID)
}

// mintBoundedGrantParams mints a BOUNDED (epoch-wrapped) mail capability
// grant — the content-sealing-epochs § 2 bounded regime (`mintBoundedGrant`).
// Same field shapes as `mint-grant`; the window is the crypto-effective epoch
// bound, not just advisory.
type mintBoundedGrantParams struct {
	OwnerActorIDB64  string `json:"owner_actor_id_b64"` // 32-byte owner actor_id
	GrantIDB64       string `json:"grant_id_b64"`       // 16-byte caller-namespaced grant id
	HolderPubkeyB64  string `json:"holder_pubkey_b64"`  // 32-byte holder x25519 pubkey (wrap target)
	MsekB64          string `json:"msek_b64"`           // 32-byte owner MSEK (per-epoch payloads derive from it)
	HolderMlkemEkB64 string `json:"holder_mlkem_ek_b64,omitempty"`
	WindowStart      uint64 `json:"window_start"`             // unix seconds — bounds the wrapped epoch set
	WindowEnd        uint64 `json:"window_end"`               // unix seconds
	LabelerIDB64     string `json:"labeler_id_b64,omitempty"` // optional: the per-labeler shape (see mintGrantParams)
}

func runMintBoundedGrant(raw []byte) ([]byte, error) {
	var p mintBoundedGrantParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse mint-bounded-grant params: %w", err)
	}
	owner, err := decodeB64(p.OwnerActorIDB64, "owner_actor_id_b64")
	if err != nil {
		return nil, err
	}
	grantID, err := decodeB64(p.GrantIDB64, "grant_id_b64")
	if err != nil {
		return nil, err
	}
	holderPub, err := decodeB64(p.HolderPubkeyB64, "holder_pubkey_b64")
	if err != nil {
		return nil, err
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	holderMlkemEk, err := decodeB64(p.HolderMlkemEkB64, "holder_mlkem_ek_b64")
	if err != nil {
		return nil, err
	}
	labelerID, err := decodeB64(p.LabelerIDB64, "labeler_id_b64")
	if err != nil {
		return nil, err
	}
	return mintBoundedGrant(owner, grantID, holderPub, msek, holderMlkemEk, p.WindowStart, p.WindowEnd, labelerID)
}

// mintSpamModelGrantParams mints the KEYLESS `content.read{spam-model}` capability
// grant to an MDA aggregation holder — what the all-6-client contribute-baseline
// toggle mints in production (mail-spam.md § Encrypted-mode interaction, the
// keyless `content.read{spam-model}` shape). Unlike `mint-grant` it carries NO `msek` and NO key material:
// spam-model is a keyless read kind, so the grant wraps nothing — contributing to
// the deployment spam baseline conveys zero standing read of the contributor's
// mailbox. The tier_3 spam-baseline-drain harness drives it as a precondition (E2E
// rule 8 carve-out (b)) and asserts the negative: a holder bearing only this grant
// gets an empty rescore worklist and cannot open the contributor's mail. No holder
// ML-KEM ek: a keyless grant has nothing to wrap, so the X-Wing/classical suite is
// moot.
type mintSpamModelGrantParams struct {
	OwnerActorIDB64 string `json:"owner_actor_id_b64"` // 32-byte owner actor_id (Ed25519 verify key)
	GrantIDB64      string `json:"grant_id_b64"`       // 16-byte caller-namespaced grant id
	HolderPubkeyB64 string `json:"holder_pubkey_b64"`  // 32-byte MDA holder x25519 pubkey (the grant target)
	EpochStart      uint64 `json:"epoch_start"`        // advisory window start (unix seconds)
	EpochEnd        uint64 `json:"epoch_end"`          // advisory window end (unix seconds)
}

func runMintSpamModelGrant(raw []byte) ([]byte, error) {
	var p mintSpamModelGrantParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse mint-spam-model-grant params: %w", err)
	}
	owner, err := decodeB64(p.OwnerActorIDB64, "owner_actor_id_b64")
	if err != nil {
		return nil, err
	}
	grantID, err := decodeB64(p.GrantIDB64, "grant_id_b64")
	if err != nil {
		return nil, err
	}
	holderPub, err := decodeB64(p.HolderPubkeyB64, "holder_pubkey_b64")
	if err != nil {
		return nil, err
	}
	return mintSpamModelGrant(owner, grantID, holderPub, p.EpochStart, p.EpochEnd)
}

// deriveRecipientMlkemEkParams derives the actor's standing recipient-mail ML-KEM-768
// encapsulation key (1184 bytes) from MSEK — the post-quantum sibling of
// `derive-recipient-pubkey` published via `fauna.bridges.provision_recipient_mls_pubkey`
// (`mlkem_ek`, S3c) to make inbound mail X-Wing-sealed. Output is the raw ek (base64).
type deriveRecipientMlkemEkParams struct {
	MsekB64 string `json:"msek_b64"` // 32-byte Master Secret
}

func runDeriveRecipientMlkemEk(raw []byte) ([]byte, error) {
	var p deriveRecipientMlkemEkParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse derive-recipient-mlkem-ek params: %w", err)
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	return deriveRecipientMlkemEk(msek)
}

// deriveBridgeMlkemEkParams derives a bridge service-user's ML-KEM-768 encapsulation
// key (1184 bytes) from its keyfile Ed25519 seed (PQ-CAP-2) — the holder ek a tier_3
// harness needs to mint an X-Wing-wrapped grant to that holder. Output is the raw ek
// (base64).
type deriveBridgeMlkemEkParams struct {
	Ed25519SeedB64 string `json:"ed25519_seed_b64"` // 32-byte bridge keyfile Ed25519 seed
}

func runDeriveBridgeMlkemEk(raw []byte) ([]byte, error) {
	var p deriveBridgeMlkemEkParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse derive-bridge-mlkem-ek params: %w", err)
	}
	seed, err := decodeB64(p.Ed25519SeedB64, "ed25519_seed_b64")
	if err != nil {
		return nil, err
	}
	return deriveBridgeMlkemEk(seed)
}

// sealMailRecordParams seals arbitrary plaintext to a recipient X25519 pubkey —
// the canonical `MailRecordEnvelope` a sealed event/card/mail body is (see
// `sealMailRecord`).
type sealMailRecordParams struct {
	PlaintextB64          string `json:"plaintext_b64"`
	RecipientX25519PubB64 string `json:"recipient_x25519_pubkey_b64"` // 32 bytes
}

func runSealMailRecord(raw []byte) ([]byte, error) {
	var p sealMailRecordParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-mail-record params: %w", err)
	}
	plaintext, err := decodeB64(p.PlaintextB64, "plaintext_b64")
	if err != nil {
		return nil, err
	}
	pubkey, err := decodeB64(p.RecipientX25519PubB64, "recipient_x25519_pubkey_b64")
	if err != nil {
		return nil, err
	}
	return sealMailRecord(plaintext, pubkey)
}

// sealSpamModelParams seals a plaintext `fauna_mail::spam::SpamModel` serde_json
// blob to the actor's MSEK-derived recipient pubkey — the tier-1 at-rest seal the
// Fauna app (`apply_and_reseal`) / agent MDA (`store.go` trainJunkAgentSide)
// perform (mail-spam.md § Encrypted-mode interaction). A tier_3 test pre-seeds the
// output into `spam_models.model_json` so `fetch_spam_model` reports
// `stored_sealed=true` and the MDA `\Junk`-train takes the agent-side
// open→mutate→re-seal path.
type sealSpamModelParams struct {
	MsekB64      string `json:"msek_b64"`       // 32-byte Master Secret (the recipient pubkey derives from it)
	ModelJSONB64 string `json:"model_json_b64"` // plaintext fauna_mail::spam::SpamModel serde_json bytes
}

func runSealSpamModel(raw []byte) ([]byte, error) {
	var p sealSpamModelParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-spam-model params: %w", err)
	}
	msek, err := decodeB64(p.MsekB64, "msek_b64")
	if err != nil {
		return nil, err
	}
	modelJSON, err := decodeB64(p.ModelJSONB64, "model_json_b64")
	if err != nil {
		return nil, err
	}
	return sealSpamModel(msek, modelJSON)
}

// sealSpamModelCopyParams seals a plaintext `fauna_mail::spam::SpamModel`
// serde_json blob to the aggregation HOLDER's published X25519 pubkey — the
// `SpamModelCopyBlob` a contributor's client attaches to `put_spam_model`'s
// `holder_copy` (mail-spam.md § Encrypted-mode interaction, the keyless
// `content.read{spam-model}` shape). The tier_3 spam-baseline-drain test
// pre-seeds the output beside the contributor's sealed `spam_models` row so the
// publish drain's holder worklist serves it and the Go holder merges it — the
// seal side of the seal→drain→merge proof, mirroring how `seal-spam-model`
// seeds the sealed model itself. `owner_actor_id_b64` binds the copy's AAD
// (a mis-attributed copy fails the holder's owner cross-check); an empty
// `holder_mlkem_ek_b64` selects the classical X25519 seal (non-empty ⇒ X-Wing).
type sealSpamModelCopyParams struct {
	ModelJSONB64       string `json:"model_json_b64"`           // plaintext fauna_mail::spam::SpamModel serde_json bytes
	OwnerActorIDB64    string `json:"owner_actor_id_b64"`       // 32-byte contributing actor id (AAD binding)
	HolderX25519PubB64 string `json:"holder_x25519_pubkey_b64"` // 32-byte aggregation holder pubkey (fetch_bridge_pubkey)
	HolderMlkemEkB64   string `json:"holder_mlkem_ek_b64"`      // optional 1184-byte ML-KEM ek → X-Wing seal; empty ⇒ classical
}

func runSealSpamModelCopy(raw []byte) ([]byte, error) {
	var p sealSpamModelCopyParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-spam-model-copy params: %w", err)
	}
	modelJSON, err := decodeB64(p.ModelJSONB64, "model_json_b64")
	if err != nil {
		return nil, err
	}
	owner, err := decodeB64(p.OwnerActorIDB64, "owner_actor_id_b64")
	if err != nil {
		return nil, err
	}
	holderPub, err := decodeB64(p.HolderX25519PubB64, "holder_x25519_pubkey_b64")
	if err != nil {
		return nil, err
	}
	var holderEk []byte
	if p.HolderMlkemEkB64 != "" {
		holderEk, err = decodeB64(p.HolderMlkemEkB64, "holder_mlkem_ek_b64")
		if err != nil {
			return nil, err
		}
	}
	return sealSpamModelCopy(modelJSON, owner, holderPub, holderEk)
}

// publishLabelerParams builds a signed AlgorithmLabeler metadata_blob for
// `fauna.labelers.publish` (labeler-registry design § 5). `signing_seed_b64` is
// the publisher's 32-byte Ed25519 seed — its verifying key IS the labeler's
// `algorithm_id` (public-key-is-identity), so the same keypair should make the WS
// publish call. `content_kind` (`post`|`mail`) is NOT signed metadata — it is a
// separate `PublishLabelerRequest` envelope field the caller passes to the RPC;
// only the signed `AlgorithmLabeler` is built here. The `input_schema` +
// `resource_limits` flags below are declared-and-signed (the holder clamps the
// limits to a host ceiling, F4). The tier_3 re-score-drain-for-labelers test
// drives this as a precondition (mirrors the deferred all-6-client publish UI).
type publishLabelerParams struct {
	SigningSeedB64     string `json:"signing_seed_b64"` // 32-byte Ed25519 seed → algorithm_id
	WasmB64            string `json:"wasm_b64"`         // WASM (or wasmi-parseable WAT) module bytes
	Version            uint64 `json:"version"`
	NeedsText          bool   `json:"needs_text"`
	NeedsHashtags      bool   `json:"needs_hashtags"`
	NeedsMediaMetadata bool   `json:"needs_media_metadata"`
	NeedsAuthor        bool   `json:"needs_author"`
	MaxMemoryBytes     uint64 `json:"max_memory_bytes"`
	MaxCpuMicroseconds uint64 `json:"max_cpu_microseconds"`
	UpdatedAt          uint64 `json:"updated_at"`
}

func runPublishLabeler(raw []byte) ([]byte, error) {
	var p publishLabelerParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse publish-labeler params: %w", err)
	}
	seed, err := decodeB64(p.SigningSeedB64, "signing_seed_b64")
	if err != nil {
		return nil, err
	}
	wasmBytes, err := decodeB64(p.WasmB64, "wasm_b64")
	if err != nil {
		return nil, err
	}
	return publishLabeler(
		seed, wasmBytes, p.Version,
		p.NeedsText, p.NeedsHashtags, p.NeedsMediaMetadata, p.NeedsAuthor,
		p.MaxMemoryBytes, p.MaxCpuMicroseconds, p.UpdatedAt,
	)
}

// encodeTextModelArtifactParams mints `text-model` artifact bytes at a
// caller-chosen tokenizer `version` — the one artifact no app UI can publish,
// because the shared publish lifecycle stamps the current version itself. The
// tier_3 "needs a newer app" badge journey needs exactly that artifact as a
// precondition; the bytes returned ride as `wasm_bytes` in the ordinary
// `fauna.labelers.publish` request (with `artifact_kind: "text-model"`), signed
// by the existing `publish-labeler` mode, which signs whatever artifact bytes it
// is handed.
type textModelNgramParam struct {
	Ngram string `json:"ngram"`
	More  uint32 `json:"more"`
	Less  uint32 `json:"less"`
}

type encodeTextModelArtifactParams struct {
	Version  uint16                `json:"version"`   // the tokenizer contract to claim
	Name     *string               `json:"name"`      // publisher-chosen public name (optional)
	MoreDocs uint32                `json:"more_docs"` // corpus counters, unshrunk
	LessDocs uint32                `json:"less_docs"`
	Ngrams   []textModelNgramParam `json:"ngrams"`
}

func runEncodeTextModelArtifact(raw []byte) ([]byte, error) {
	var p encodeTextModelArtifactParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse encode-text-model-artifact params: %w", err)
	}
	return encodeTextModelArtifact(p.Version, p.Name, p.MoreDocs, p.LessDocs, p.Ngrams)
}

func runSealSubmissionToken(raw []byte) ([]byte, error) {
	var p submissionTokenParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, fmt.Errorf("parse seal-submission-token params: %w", err)
	}
	seed, err := decodeB64(p.SigningSeedB64, "signing_seed_b64")
	if err != nil {
		return nil, err
	}
	credential, err := decodeB64(p.CredentialB64, "credential_b64")
	if err != nil {
		return nil, err
	}
	return sealSubmissionToken(seed, p.CredentialID, p.IssuedAt, p.ExpiresAt, p.MaxRecipients, p.MaxMessagesPerDay, p.CredentialKind, credential)
}

func decodeB64(s, field string) ([]byte, error) {
	b, err := base64.StdEncoding.DecodeString(s)
	if err != nil {
		return nil, fmt.Errorf("decode %s: %w", field, err)
	}
	return b, nil
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "seal-helper-testonly: "+format+"\n", args...)
	os.Exit(1)
}
