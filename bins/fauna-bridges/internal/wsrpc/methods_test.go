// Tests for the typed WS-RPC method wrappers (methods.go).
//
// Each subtest substitutes a *recordingCaller for the underlying
// *Client, captures the method name + encoded body the wrapper would
// have written to the wire, and asserts:
//  1. Method string matches `fauna.bridges.<name>` exactly.
//  2. Body shape decodes back into a struct that mirrors the
//     libs/fauna-protocol Rust wire shape — field names and types.
//  3. The wrapper unpacks a canned reply payload correctly.
//
// A whoami round-trip integration test against a real httptest WS
// server exercises the full envelope+payload path; the other
// wrappers use the fake Caller pattern since their nest-side handlers
// are either already covered by Rust unit tests (the ones that exist
// today) or not yet implemented (Phase C/D scope).
package wsrpc

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/confinement"
	"github.com/fxamacker/cbor/v2"
	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
)

// recordingCaller is a fake Caller that captures every Call() and
// returns a caller-supplied reply payload. Failure modes:
//   - returnErr is non-nil → Call returns it unmodified.
//   - replyBody is non-nil → encoded as canonical CBOR and unmarshaled
//     into the caller's reply target.
type recordingCaller struct {
	gotMethod string
	gotBody   []byte // canonical CBOR of the body the wrapper passed
	replyBody any    // payload to encode into `reply`
	returnErr error
}

func (r *recordingCaller) Call(_ context.Context, method string, body, reply any) error {
	r.gotMethod = method
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	r.gotBody = enc
	if r.returnErr != nil {
		return r.returnErr
	}
	if reply == nil || r.replyBody == nil {
		return nil
	}
	repBytes, err := dagcbor.Marshal(r.replyBody)
	if err != nil {
		return err
	}
	return cbor.Unmarshal(repBytes, reply)
}

func TestMethodWrappers(t *testing.T) {
	ctx := context.Background()

	t.Run("Whoami", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: WhoamiReply{
				Role:             "mta",
				BridgeID:         "mta-eu-1",
				Status:           "approved",
				Ed25519PubkeyHex: strings.Repeat("ab", 32),
				X25519PubkeyHex:  strings.Repeat("cd", 32),
			},
		}
		got, err := Whoami(ctx, caller)
		if err != nil {
			t.Fatalf("Whoami: %v", err)
		}
		if caller.gotMethod != MethodWhoami {
			t.Errorf("method: got %q, want fauna.bridges.whoami", caller.gotMethod)
		}
		// Body must decode as an empty map (zero-field struct).
		var body map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(body) != 0 {
			t.Errorf("body: got %v, want empty map", body)
		}
		if got.Role != "mta" || got.BridgeID != "mta-eu-1" || got.Status != "approved" {
			t.Errorf("reply unpack: got %+v", got)
		}
	})

	// requestEnrollmentWire is the full RequestEnrollmentRequest wire shape,
	// including the slice-2 proof-of-possession fields (omitempty on both sides).
	type requestEnrollmentWire struct {
		Ed25519Pubkey []byte `cbor:"ed25519_pubkey"`
		RoleHint      string `cbor:"role_hint"`
		BridgeID      string `cbor:"bridge_id"`
		X25519Pubkey  []byte `cbor:"x25519_pubkey,omitempty"`
		EnrollmentSig []byte `cbor:"enrollment_sig,omitempty"`
	}

	t.Run("RequestEnrollment_signed", func(t *testing.T) {
		// Image path: the artifact-minted bridge presents its x25519 pubkey + a
		// proof-of-possession signature over EnrollmentSignedMessage. Assert the
		// wire carries both and that the signature verifies — exactly what nest's
		// check_enrollment_authorization checks against the blessed key.
		edPub, edPriv, err := ed25519.GenerateKey(nil)
		if err != nil {
			t.Fatalf("GenerateKey: %v", err)
		}
		x25519 := bytes.Repeat([]byte{0x5c}, 32)
		id := EnrollmentIdentity{
			Ed25519Pub:    edPub,
			X25519Pub:     x25519,
			EnrollmentSig: SignEnrollment(edPriv, "mta", edPub, x25519),
			RoleHint:      "mta",
			BridgeID:      "unresolved-bridge",
		}
		caller := &recordingCaller{
			replyBody: requestEnrollmentReply{Status: StatusPending},
		}
		status, err := RequestEnrollment(ctx, caller, id)
		if err != nil {
			t.Fatalf("RequestEnrollment: %v", err)
		}
		if caller.gotMethod != MethodRequestEnrollment {
			t.Errorf("method: got %q, want fauna.bridges.request_enrollment", caller.gotMethod)
		}
		var body requestEnrollmentWire
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytes.Equal(body.Ed25519Pubkey, edPub) {
			t.Errorf("ed25519_pubkey: got %x, want %x", body.Ed25519Pubkey, edPub)
		}
		if body.RoleHint != "mta" || body.BridgeID != "unresolved-bridge" {
			t.Errorf("body: got role_hint=%q bridge_id=%q", body.RoleHint, body.BridgeID)
		}
		if !bytes.Equal(body.X25519Pubkey, x25519) {
			t.Errorf("x25519_pubkey: got %x, want %x", body.X25519Pubkey, x25519)
		}
		if len(body.EnrollmentSig) != ed25519.SignatureSize {
			t.Fatalf("enrollment_sig length: got %d, want %d", len(body.EnrollmentSig), ed25519.SignatureSize)
		}
		// The signature on the wire must verify against the single-source message
		// — the same bytes (and key) nest's verifier reconstructs.
		wantMsg := EnrollmentSignedMessage("mta", edPub, x25519)
		if !ed25519.Verify(edPub, wantMsg, body.EnrollmentSig) {
			t.Errorf("enrollment_sig does not verify over EnrollmentSignedMessage(mta, edPub, x25519)")
		}
		if status != StatusPending {
			t.Errorf("status: got %q, want %q", status, StatusPending)
		}
	})

	t.Run("RequestEnrollment_noPoP_omitsPoPFields", func(t *testing.T) {
		// Dev/binary-only bridge: no PoP material → x25519_pubkey + enrollment_sig
		// must be OMITTED from the wire (omitempty), matching the cross-language
		// fixture's three-field shape.
		pubkey := bytes.Repeat([]byte{0x9a}, 32)
		caller := &recordingCaller{
			replyBody: requestEnrollmentReply{Status: StatusPending},
		}
		status, err := RequestEnrollment(ctx, caller, EnrollmentIdentity{
			Ed25519Pub: pubkey,
			RoleHint:   "mta",
			BridgeID:   "unresolved-bridge",
		})
		if err != nil {
			t.Fatalf("RequestEnrollment: %v", err)
		}
		var raw map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &raw); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if _, ok := raw["x25519_pubkey"]; ok {
			t.Errorf("x25519_pubkey present on a no-PoP enrollment; want omitted")
		}
		if _, ok := raw["enrollment_sig"]; ok {
			t.Errorf("enrollment_sig present on a no-PoP enrollment; want omitted")
		}
		if status != StatusPending {
			t.Errorf("status: got %q, want %q", status, StatusPending)
		}
	})

	t.Run("RegisterServiceUser", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: registerServiceUserReply{
				EnrollmentRequestID: "enrollment-deadbeef-approved",
			},
		}
		ed := []byte{0xab, 0xcd}
		x := []byte{0x12, 0x34}
		// PQ-CAP-2: a 1184-byte ML-KEM ek is published alongside the x25519 key.
		ek := make([]byte, 1184)
		ek[0], ek[1183] = 0x5a, 0x5a
		conf := &confinement.Report{
			UID:         1001,
			SealedStore: confinement.SealedStoreDenied,
			Landlock:    "partial",
			Seccomp:     confinement.SeccompFilter,
		}
		gotID, err := RegisterServiceUser(ctx, caller, ed, x, ek, "mta", "mta-eu-1", conf)
		if err != nil {
			t.Fatalf("RegisterServiceUser: %v", err)
		}
		if caller.gotMethod != MethodRegisterServiceUser {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		// Body shape: keys ed25519_pubkey (bstr), x25519_pubkey (bstr),
		// mlkem_ek (bstr, PQ-CAP-2), role (tstr), bridge_id (tstr),
		// confinement (map, the startup self-probe).
		var body struct {
			Ed25519Pubkey []byte `cbor:"ed25519_pubkey"`
			X25519Pubkey  []byte `cbor:"x25519_pubkey"`
			MlkemEk       []byte `cbor:"mlkem_ek"`
			Role          string `cbor:"role"`
			BridgeID      string `cbor:"bridge_id"`
			Confinement   *struct {
				UID         uint32 `cbor:"uid"`
				SealedStore string `cbor:"sealed_store"`
				Landlock    string `cbor:"landlock"`
				Seccomp     string `cbor:"seccomp"`
			} `cbor:"confinement"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.Ed25519Pubkey, ed) || !bytesEq(body.X25519Pubkey, x) {
			t.Errorf("pubkeys: got ed=%x x=%x, want %x %x", body.Ed25519Pubkey, body.X25519Pubkey, ed, x)
		}
		if !bytesEq(body.MlkemEk, ek) {
			t.Errorf("mlkem_ek: got %d bytes, want 1184", len(body.MlkemEk))
		}
		if body.Role != "mta" || body.BridgeID != "mta-eu-1" {
			t.Errorf("role/bridge_id: got %q/%q", body.Role, body.BridgeID)
		}
		// The confinement self-probe must reach the wire with snake_case keys
		// matching BridgeConfinement — nest reads these to answer "is this
		// deployed box actually sandboxed?" without an SSH session.
		if body.Confinement == nil {
			t.Fatalf("confinement missing from the wire; body=%x", caller.gotBody)
		}
		if body.Confinement.UID != 1001 ||
			body.Confinement.SealedStore != "denied" ||
			body.Confinement.Landlock != "partial" ||
			body.Confinement.Seccomp != "filter" {
			t.Errorf("confinement: got %+v, want {1001 denied partial filter}", *body.Confinement)
		}
		if gotID != "enrollment-deadbeef-approved" {
			t.Errorf("reply: got %q", gotID)
		}
	})

	t.Run("RegisterServiceUser_nonProbing_omitsConfinement", func(t *testing.T) {
		// A bridge that does not probe (nil report) must emit no `confinement`
		// key at all (omitempty, the mechanism under test). Same omitempty discipline as
		// mlkem_ek.
		caller := &recordingCaller{
			replyBody: registerServiceUserReply{EnrollmentRequestID: "enrollment-x-approved"},
		}
		if _, err := RegisterServiceUser(ctx, caller, []byte{0x01}, []byte{0x02}, nil, "mda", "mda-1", nil); err != nil {
			t.Fatalf("RegisterServiceUser: %v", err)
		}
		var raw map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &raw); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if _, present := raw["confinement"]; present {
			t.Errorf("confinement key present for a non-probing bridge: %v", raw)
		}
		if _, present := raw["mlkem_ek"]; present {
			t.Errorf("mlkem_ek key present for a classical-only bridge: %v", raw)
		}
	})

	t.Run("FetchTLSCertBlob", func(t *testing.T) {
		blob := []byte{0x01, 0x02, 0x03}
		caller := &recordingCaller{
			replyBody: fetchTLSCertBlobReply{Blob: &blob},
		}
		got, err := FetchTLSCertBlob(ctx, caller, "mta", "mta-eu-1", "example.com")
		if err != nil {
			t.Fatalf("FetchTLSCertBlob: %v", err)
		}
		if caller.gotMethod != MethodFetchTLSCertBlob {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			BridgeRole string `cbor:"bridge_role"`
			BridgeID   string `cbor:"bridge_id"`
			Domain     string `cbor:"domain"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.BridgeRole != "mta" || body.BridgeID != "mta-eu-1" || body.Domain != "example.com" {
			t.Errorf("body: %+v", body)
		}
		if !bytesEq(got, blob) {
			t.Errorf("blob: got %x, want %x", got, blob)
		}
	})

	t.Run("FetchTLSCertBlob_NoBlob", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: fetchTLSCertBlobReply{Blob: nil},
		}
		got, err := FetchTLSCertBlob(ctx, caller, "mta", "mta-eu-1", "example.com")
		if err != nil {
			t.Fatalf("FetchTLSCertBlob: %v", err)
		}
		if got != nil {
			t.Errorf("expected nil blob, got %x", got)
		}
	})

	t.Run("FetchBridgePubkey", func(t *testing.T) {
		ed := []byte{0xaa, 0xbb}
		x := []byte{0xcc, 0xdd}
		caller := &recordingCaller{
			replyBody: fetchBridgePubkeyReply{Ed25519Pubkey: ed, X25519Pubkey: x},
		}
		gotEd, gotX, err := FetchBridgePubkey(ctx, caller, "mda", "mda-eu-1")
		if err != nil {
			t.Fatalf("FetchBridgePubkey: %v", err)
		}
		if caller.gotMethod != MethodFetchBridgePubkey {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			BridgeRole string `cbor:"bridge_role"`
			BridgeID   string `cbor:"bridge_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.BridgeRole != "mda" || body.BridgeID != "mda-eu-1" {
			t.Errorf("body: %+v", body)
		}
		if !bytesEq(gotEd, ed) || !bytesEq(gotX, x) {
			t.Errorf("pubkeys: got ed=%x x=%x", gotEd, gotX)
		}
	})

	t.Run("FetchConfig", func(t *testing.T) {
		// Catalog defaults sourced from DefaultConfigSnapshot, which
		// mirrors the Rust `FetchConfigReply::default()` (authority:
		// libs/fauna-protocol/src/bridge_routing.rs).
		snap := DefaultConfigSnapshot()
		caller := &recordingCaller{replyBody: snap}
		got, err := FetchConfig(ctx, caller, "all")
		if err != nil {
			t.Fatalf("FetchConfig: %v", err)
		}
		if caller.gotMethod != MethodFetchConfig {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			Scope string `cbor:"scope"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.Scope != "all" {
			t.Errorf("scope: got %q, want all", body.Scope)
		}
		if !got.MailEnabled ||
			got.Spam.MaxScoreBeforeSpamFolder != 5 ||
			got.Spam.MaxScoreBeforeReject != 0 ||
			got.Spam.MaxConnPerMin != 10 ||
			got.Spam.FCrDNSMode != "score_signal" ||
			!got.Spam.HELOIdentityRequired ||
			got.Spam.RejectFCrDNSFail ||
			got.Spam.MaxMessageBytes != 50_000_000 ||
			got.Auth.EnforceDmarc != true ||
			got.Auth.EnforceDkim != false ||
			got.Submission.MaxPerDay != 1000 ||
			got.IMAP.IdleTimeoutSecs != 1740 ||
			got.IMAP.TombstoneRetentionDays != 30 ||
			got.IMAP.DeleteNonempty != "forbidden" ||
			got.IMAP.BodyStructureCacheMax != 4096 ||
			len(got.Outbound.RetryScheduleSeconds) != 10 ||
			got.Outbound.RetryScheduleSeconds[0] != 0 ||
			got.Outbound.RetryScheduleSeconds[1] != 300 ||
			got.Outbound.PermanentFailureTimeoutHours != 120 ||
			got.Outbound.DelayWarningAtHours != 4 ||
			got.Outbound.NDRRateLimitDays != 7 ||
			!got.Outbound.SuppressNDRSPFHardfail ||
			!got.Outbound.SuppressNDRDMARCReject ||
			got.Outbound.PostmasterCCBounces ||
			!got.Outbound.TLSRPTSendReports ||
			!got.Outbound.IPv6Enabled ||
			got.Bridge.ShutdownGraceSeconds != 30 {
			t.Errorf("snapshot: %+v", got)
		}
	})

	t.Run("ReportAuthEvent", func(t *testing.T) {
		caller := &recordingCaller{replyBody: reportAuthEventReply{OK: true}}
		actor := bytes32(0x42)
		err := ReportAuthEvent(ctx, caller, actor[:], "cred-1", "ok", "10.0.0.1", "first-attempt", 1_700_000_000)
		if err != nil {
			t.Fatalf("ReportAuthEvent: %v", err)
		}
		if caller.gotMethod != MethodReportAuthEvent {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID      []byte  `cbor:"actor_id"`
			CredentialID string  `cbor:"credential_id"`
			Result       string  `cbor:"result"`
			SourceIP     string  `cbor:"source_ip"`
			OccurredAt   uint64  `cbor:"occurred_at"`
			Reason       *string `cbor:"reason"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: got %x", body.ActorID)
		}
		if body.CredentialID != "cred-1" || body.Result != "ok" || body.SourceIP != "10.0.0.1" {
			t.Errorf("body: %+v", body)
		}
		if body.OccurredAt != 1_700_000_000 {
			t.Errorf("occurred_at: got %d", body.OccurredAt)
		}
		if body.Reason == nil || *body.Reason != "first-attempt" {
			t.Errorf("reason: got %v", body.Reason)
		}
	})

	t.Run("ReportAuthEvent_EmptyReasonIsNone", func(t *testing.T) {
		caller := &recordingCaller{replyBody: reportAuthEventReply{OK: true}}
		actor := bytes32(0x01)
		if err := ReportAuthEvent(ctx, caller, actor[:], "cred-1", "fail", "10.0.0.1", "", 1); err != nil {
			t.Fatalf("ReportAuthEvent: %v", err)
		}
		var body struct {
			Reason *string `cbor:"reason"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.Reason != nil {
			t.Errorf("empty reason must encode as None, got %v", body.Reason)
		}
	})

	t.Run("ReportAuthEvent_ServerNotOK", func(t *testing.T) {
		caller := &recordingCaller{replyBody: reportAuthEventReply{OK: false}}
		actor := bytes32(0x02)
		err := ReportAuthEvent(ctx, caller, actor[:], "cred-1", "ok", "10.0.0.1", "", 1)
		if err == nil {
			t.Fatalf("expected error on ok=false reply")
		}
	})

	t.Run("ValidateRecipient_Resolved", func(t *testing.T) {
		actor := bytes32(0x42)
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":  "resolved",
				"actor_id": actor[:],
			},
		}
		gotID, isRole, err := ValidateRecipient(ctx, caller, "alice", "example.com")
		if err != nil {
			t.Fatalf("ValidateRecipient: %v", err)
		}
		if caller.gotMethod != MethodValidateRecipient {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			LocalPart string `cbor:"local_part"`
			Domain    string `cbor:"domain"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.LocalPart != "alice" || body.Domain != "example.com" {
			t.Errorf("body: %+v", body)
		}
		if !bytesEq(gotID, actor[:]) {
			t.Errorf("actor_id: got %x, want %x", gotID, actor[:])
		}
		if isRole {
			t.Errorf("a normal alias hit is not a role address; got isRole=true")
		}
	})

	t.Run("ValidateRecipient_RoleAddress", func(t *testing.T) {
		// A role-address recipient (postmaster@ → admin mailbox): nest sets
		// is_role_address so the caller bypasses the per-mailbox quota on
		// ingest (smtp-server.md :204).
		admin := bytes32(0x07)
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":         "resolved",
				"actor_id":        admin[:],
				"is_role_address": true,
			},
		}
		gotID, isRole, err := ValidateRecipient(ctx, caller, "postmaster", "example.com")
		if err != nil {
			t.Fatalf("ValidateRecipient: %v", err)
		}
		if !bytesEq(gotID, admin[:]) {
			t.Errorf("actor_id: got %x, want %x", gotID, admin[:])
		}
		if !isRole {
			t.Errorf("postmaster@ resolved via the role-address route; want isRole=true")
		}
	})

	t.Run("ValidateRecipient_Reject", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome": "reject",
				"reason":  "no such recipient",
			},
		}
		gotID, _, err := ValidateRecipient(ctx, caller, "ghost", "example.com")
		if err == nil {
			t.Fatalf("expected reject error, got id=%x", gotID)
		}
		if !errors.Is(err, ErrRecipientRejected) {
			t.Errorf("reject error should match ErrRecipientRejected sentinel; got: %v", err)
		}
		if gotID != nil {
			t.Errorf("rejected call returned id=%x (want nil)", gotID)
		}
	})

	t.Run("CheckGreylist_Pass", func(t *testing.T) {
		caller := &recordingCaller{replyBody: checkGreylistReply{Pass: true}}
		pass, err := CheckGreylist(ctx, caller, "sender@remote.test", "bob@example.com", "203.0.113.9")
		if err != nil {
			t.Fatalf("CheckGreylist: %v", err)
		}
		if caller.gotMethod != MethodCheckGreylist {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			From     string `cbor:"from"`
			To       string `cbor:"to"`
			ClientIP string `cbor:"client_ip"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.From != "sender@remote.test" || body.To != "bob@example.com" || body.ClientIP != "203.0.113.9" {
			t.Errorf("body: %+v", body)
		}
		if !pass {
			t.Error("expected pass=true")
		}
	})

	t.Run("CheckGreylist_Defer", func(t *testing.T) {
		caller := &recordingCaller{replyBody: checkGreylistReply{Pass: false}}
		pass, err := CheckGreylist(ctx, caller, "sender@remote.test", "bob@example.com", "203.0.113.9")
		if err != nil {
			t.Fatalf("CheckGreylist: %v", err)
		}
		if pass {
			t.Error("expected pass=false (defer → 451)")
		}
	})

	t.Run("CheckGreylist_FailsOpen", func(t *testing.T) {
		// A nest transport error must fail OPEN (pass=true) so we never
		// tempfail legitimate mail on our own backend blip.
		caller := &recordingCaller{returnErr: errors.New("transport failure")}
		pass, err := CheckGreylist(ctx, caller, "sender@remote.test", "bob@example.com", "203.0.113.9")
		if err == nil {
			t.Fatal("expected the transport error to be surfaced")
		}
		if !pass {
			t.Error("greylist check must fail OPEN (pass=true) on error")
		}
	})

	t.Run("FetchWrappedSubmissionToken", func(t *testing.T) {
		blob := []byte{0x99}
		caller := &recordingCaller{replyBody: fetchWrappedSubmissionTokenReply{Blob: &blob}}
		actor := bytes32(0x01)
		got, err := FetchWrappedSubmissionToken(ctx, caller, actor[:], "cred-x")
		if err != nil {
			t.Fatalf("FetchWrappedSubmissionToken: %v", err)
		}
		if caller.gotMethod != MethodFetchWrappedSubmissionToken {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID      []byte `cbor:"actor_id"`
			CredentialID string `cbor:"credential_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || body.CredentialID != "cred-x" {
			t.Errorf("body: %+v", body)
		}
		if !bytesEq(got, blob) {
			t.Errorf("blob: got %x", got)
		}
	})

	t.Run("CheckSubmissionQuota_Allowed", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{"outcome": "allowed"},
		}
		actor := bytes32(0x07)
		allowed, remaining, err := CheckSubmissionQuota(ctx, caller, actor[:], 5, true)
		if err != nil {
			t.Fatalf("CheckSubmissionQuota: %v", err)
		}
		if caller.gotMethod != MethodCheckSubmissionQuota {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID          []byte `cbor:"actor_id"`
			RecipientCount   uint32 `cbor:"recipient_count"`
			RecipientIsLocal bool   `cbor:"recipient_is_local"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || body.RecipientCount != 5 || !body.RecipientIsLocal {
			t.Errorf("body: %+v", body)
		}
		if !allowed || remaining != 0 {
			t.Errorf("allowed=%v remaining=%d", allowed, remaining)
		}
	})

	t.Run("CheckSubmissionQuota_OverQuota", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{"outcome": "over_quota", "remaining": uint32(3)},
		}
		actor := bytes32(0x08)
		allowed, remaining, err := CheckSubmissionQuota(ctx, caller, actor[:], 10, false)
		if err != nil {
			t.Fatalf("CheckSubmissionQuota: %v", err)
		}
		if allowed || remaining != 3 {
			t.Errorf("allowed=%v remaining=%d", allowed, remaining)
		}
	})

	t.Run("IngestInboundMail", func(t *testing.T) {
		mid := bytes32(0xab)
		caller := &recordingCaller{
			replyBody: ingestInboundMailReply{MessageID: mid[:]},
		}
		actor := bytes32(0x99)
		got, err := IngestInboundMail(ctx, caller, IngestInboundMailParams{
			ActorID:            actor[:],
			EncryptedBody:      []byte{0x01, 0x02},
			EncryptedIndexHint: []byte{0x03, 0x04},
			PublicMetadata: PublicMailMetadata{
				Timestamp:      1_700_000_000,
				CiphertextSize: 2,
				SenderDomain:   "example.com",
			},
			Verdicts: AuthVerdicts{
				Dkim: DkimVerdict{Kind: "pass"},
				Spf:  SpfVerdict{Kind: "pass"},
				Dmarc: DmarcVerdict{
					Kind: "fail",
					Data: &DmarcVerdictFail{Policy: "reject"},
				},
				Arc: ArcVerdict{Kind: "none"},
			},
			SpamScore:       0,
			SpamDisposition: "accept",
			// T1.4 scan verdict — adjacently-tagged ClamavVerdict +
			// optional RspamdScore (validates the hand-written cbor mirror).
			ClamavVerdict: ClamavVerdict{
				Kind: "infected",
				Data: &ClamavVerdictData{Signature: "Eicar-Test"},
			},
			RspamdScore: &RspamdScore{
				RawMilli:     2400,
				ScaledMilli:  1200,
				FlaggedRules: []string{"BAYES_HAM"},
				Breakdown:    []RspamdRuleContribution{{Rule: "BAYES_HAM", ScoreMilli: -2900}},
			},
			DedupKey:    "msgid:v1:inbound@example.com",
			EnvelopeKey: "env:v1:inbound",
		})
		if err != nil {
			t.Fatalf("IngestInboundMail: %v", err)
		}
		if caller.gotMethod != MethodIngestInboundMail {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		// Round-trip the body through the same struct the Rust side
		// expects (mirrors IngestInboundMailRequest field tags).
		var body struct {
			ActorID            []byte             `cbor:"actor_id"`
			EncryptedBody      []byte             `cbor:"encrypted_body"`
			EncryptedIndexHint []byte             `cbor:"encrypted_index_hint"`
			PublicMetadata     PublicMailMetadata `cbor:"public_metadata"`
			Verdicts           AuthVerdicts       `cbor:"verdicts"`
			SpamScore          uint32             `cbor:"spam_score"`
			SpamDisposition    string             `cbor:"spam_disposition"`
			ClamavVerdict      ClamavVerdict      `cbor:"clamav_verdict"`
			RspamdScore        *RspamdScore       `cbor:"rspamd_score"`
			DedupKey           string             `cbor:"dedup_key"`
			EnvelopeKey        string             `cbor:"envelope_key"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", body.ActorID)
		}
		if body.DedupKey != "msgid:v1:inbound@example.com" || body.EnvelopeKey != "env:v1:inbound" {
			t.Errorf("dedup pair: (%q, %q)", body.DedupKey, body.EnvelopeKey)
		}
		if body.PublicMetadata.SenderDomain != "example.com" {
			t.Errorf("sender_domain: %q", body.PublicMetadata.SenderDomain)
		}
		if body.Verdicts.Spf.Kind != "pass" || body.Verdicts.Dmarc.Kind != "fail" {
			t.Errorf("verdicts: %+v", body.Verdicts)
		}
		if body.Verdicts.Dmarc.Data == nil || body.Verdicts.Dmarc.Data.Policy != "reject" {
			t.Errorf("dmarc policy: %+v", body.Verdicts.Dmarc.Data)
		}
		if body.SpamDisposition != "accept" {
			t.Errorf("spam_disposition: %q", body.SpamDisposition)
		}
		if body.ClamavVerdict.Kind != "infected" {
			t.Errorf("clamav_verdict.kind: %q, want infected", body.ClamavVerdict.Kind)
		}
		if body.ClamavVerdict.Data == nil || body.ClamavVerdict.Data.Signature != "Eicar-Test" {
			t.Errorf("clamav_verdict.data: %+v, want signature Eicar-Test", body.ClamavVerdict.Data)
		}
		if body.RspamdScore == nil || body.RspamdScore.ScaledMilli != 1200 {
			t.Errorf("rspamd_score: %+v, want scaled 1200", body.RspamdScore)
		} else if len(body.RspamdScore.FlaggedRules) != 1 || body.RspamdScore.FlaggedRules[0] != "BAYES_HAM" {
			t.Errorf("rspamd flagged_rules: %+v", body.RspamdScore.FlaggedRules)
		}
		if !bytesEq(got, mid[:]) {
			t.Errorf("message_id: %x, want %x", got, mid[:])
		}
	})

	t.Run("SubmitInboundMail", func(t *testing.T) {
		mid := bytes32(0xcd)
		caller := &recordingCaller{
			replyBody: ingestInboundMailReply{MessageID: mid[:]},
		}
		actor := bytes32(0x77)
		got, err := SubmitInboundMail(ctx, caller, SubmitInboundMailParams{
			ActorID:            actor[:],
			EncryptedBody:      []byte{0x10},
			EncryptedIndexHint: []byte{0x11},
			PublicMetadata: PublicMailMetadata{
				Timestamp:      1_700_000_001,
				CiphertextSize: 1,
				SenderDomain:   "example.com",
			},
			Verdicts: AuthVerdicts{
				Dkim:  DkimVerdict{Kind: "pass"},
				Spf:   SpfVerdict{Kind: "pass"},
				Dmarc: DmarcVerdict{Kind: "pass"},
				Arc:   ArcVerdict{Kind: "none"},
			},
			SpamDisposition: "accept",
		})
		if err != nil {
			t.Fatalf("SubmitInboundMail: %v", err)
		}
		if caller.gotMethod != MethodSubmitInboundMail {
			t.Errorf("method: got %q (want submit, not ingest)", caller.gotMethod)
		}
		if !bytesEq(got, mid[:]) {
			t.Errorf("message_id: %x", got)
		}
	})

	t.Run("FetchRecipientMLSPubkey", func(t *testing.T) {
		pk := bytes32(0x44)
		pkSlice := pk[:]
		caller := &recordingCaller{
			replyBody: fetchRecipientMLSPubkeyReply{Key: &recipientSealKeyHalves{MLSPubkey: pkSlice, MlkemEk: wsrpctest.RecipientMlkemEk()}},
		}
		actor := bytes32(0x33)
		got, err := FetchRecipientMLSPubkey(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchRecipientMLSPubkey: %v", err)
		}
		if caller.gotMethod != MethodFetchRecipientMLSPubkey {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", body.ActorID)
		}
		if !bytesEq(got, pk[:]) {
			t.Errorf("pubkey: %x", got)
		}
	})

	t.Run("FetchRecipientMLSPubkey_NoPubkey", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: fetchRecipientMLSPubkeyReply{Key: nil},
		}
		actor := bytes32(0x33)
		got, err := FetchRecipientMLSPubkey(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchRecipientMLSPubkey: %v", err)
		}
		if got != nil {
			t.Errorf("expected nil, got %x", got)
		}
	})

	// A key on file carries both halves (post-quantum.md § Capability
	// negotiation and default-selection policy). A reply whose key lacks a
	// well-formed half is refused at this one door — never handed on as a
	// recipient to seal classically, which a misbehaving nest could otherwise
	// select as a downgrade.
	t.Run("FetchRecipientMLSPubkeyHybrid_RefusesAKeyWithoutBothHalves", func(t *testing.T) {
		pk := bytes32(0x44)
		ek := wsrpctest.RecipientMlkemEk()
		for name, key := range map[string]recipientSealKeyHalves{
			"no mlkem_ek":           {MLSPubkey: pk[:]},
			"empty mlkem_ek":        {MLSPubkey: pk[:], MlkemEk: []byte{}},
			"short mlkem_ek":        {MLSPubkey: pk[:], MlkemEk: ek[:len(ek)-1]},
			"long mlkem_ek":         {MLSPubkey: pk[:], MlkemEk: append(ek, 0)},
			"no mls_pubkey":         {MlkemEk: ek},
			"wrong-size mls_pubkey": {MLSPubkey: pk[:31], MlkemEk: ek},
		} {
			caller := &recordingCaller{replyBody: fetchRecipientMLSPubkeyReply{Key: &key}}
			actor := bytes32(0x33)
			pubkey, mlkemEk, _, err := FetchRecipientMLSPubkeyHybrid(ctx, caller, actor[:], true)
			if err == nil {
				t.Errorf("%s: expected a refusal, got pubkey len=%d ek len=%d", name, len(pubkey), len(mlkemEk))
				continue
			}
			if pubkey != nil || mlkemEk != nil {
				t.Errorf("%s: a refused key must not be returned, got pubkey len=%d ek len=%d", name, len(pubkey), len(mlkemEk))
			}
			if _, err := ResolveRecipientSealKeys(ctx, caller, actor[:]); err == nil {
				t.Errorf("%s: ResolveRecipientSealKeys must surface the refusal", name)
			}
		}
	})

	t.Run("FetchRecipientMLSPubkeyHybrid_ReturnsBothHalves", func(t *testing.T) {
		pk := bytes32(0x44)
		ek := wsrpctest.RecipientMlkemEk()
		caller := &recordingCaller{
			replyBody: fetchRecipientMLSPubkeyReply{Key: &recipientSealKeyHalves{MLSPubkey: pk[:], MlkemEk: ek}},
		}
		actor := bytes32(0x33)
		pubkey, mlkemEk, _, err := FetchRecipientMLSPubkeyHybrid(ctx, caller, actor[:], true)
		if err != nil {
			t.Fatalf("FetchRecipientMLSPubkeyHybrid: %v", err)
		}
		if !bytesEq(pubkey, pk[:]) || !bytesEq(mlkemEk, ek) {
			t.Errorf("halves: pubkey %x, ek len=%d", pubkey, len(mlkemEk))
		}
	})

	// smtp-server.md § Error / tempfail strategy: a `succession_pending`
	// reply must ride through FetchRecipientMLSPubkeyHybrid AND
	// ResolveRecipientSealKeys unchanged, so the MTA can tell "this
	// recipient is a successor awaiting a key" from "never onboarded".
	t.Run("FetchRecipientMLSPubkeyHybrid_SuccessionPending", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: fetchRecipientMLSPubkeyReply{Key: nil, SuccessionPending: true},
		}
		actor := bytes32(0x33)
		pubkey, mlkemEk, successionPending, err := FetchRecipientMLSPubkeyHybrid(ctx, caller, actor[:], true)
		if err != nil {
			t.Fatalf("FetchRecipientMLSPubkeyHybrid: %v", err)
		}
		if pubkey != nil || mlkemEk != nil {
			t.Errorf("expected nil pubkey/ek, got %x / %x", pubkey, mlkemEk)
		}
		if !successionPending {
			t.Error("expected successionPending=true")
		}
	})

	t.Run("ResolveRecipientSealKeys_SuccessionPending", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: fetchRecipientMLSPubkeyReply{Key: nil, SuccessionPending: true},
		}
		actor := bytes32(0x33)
		keys, err := ResolveRecipientSealKeys(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("ResolveRecipientSealKeys: %v", err)
		}
		if keys.MLSPubkey != nil {
			t.Errorf("expected nil MLSPubkey, got %x", keys.MLSPubkey)
		}
		if !keys.SuccessionPending {
			t.Error("expected keys.SuccessionPending=true")
		}
		// Must not call fetch_recipient_index_key when there is no MLS
		// pubkey to pair it with.
		if caller.gotMethod == MethodFetchRecipientIndexKey {
			t.Error("ResolveRecipientSealKeys must not fetch the index key when MLSPubkey is nil")
		}
	})

	// mailNewIngest must ride the wire
	// exactly as the caller passes it — the nest gates its epoch seam on
	// this field, so a wrong value here would silently widen or narrow which
	// callers can reach epoch-sealed keys.
	t.Run("FetchRecipientMLSPubkeyHybrid_mailNewIngestOnWire", func(t *testing.T) {
		for _, mailNewIngest := range []bool{true, false} {
			pk := bytes32(0x44)
			pkSlice := pk[:]
			caller := &recordingCaller{
				replyBody: fetchRecipientMLSPubkeyReply{Key: &recipientSealKeyHalves{MLSPubkey: pkSlice, MlkemEk: wsrpctest.RecipientMlkemEk()}},
			}
			actor := bytes32(0x33)
			if _, _, _, err := FetchRecipientMLSPubkeyHybrid(ctx, caller, actor[:], mailNewIngest); err != nil {
				t.Fatalf("FetchRecipientMLSPubkeyHybrid(%v): %v", mailNewIngest, err)
			}
			var body struct {
				MailNewIngest bool `cbor:"mail_new_ingest"`
			}
			if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
				t.Fatalf("decode body: %v", err)
			}
			if body.MailNewIngest != mailNewIngest {
				t.Errorf("mail_new_ingest on wire: got %v, want %v", body.MailNewIngest, mailNewIngest)
			}
		}
	})

	t.Run("FetchRecipientIndexKey", func(t *testing.T) {
		pk := bytes32(0x55)
		pkSlice := pk[:]
		caller := &recordingCaller{
			replyBody: fetchRecipientIndexKeyReply{Pubkey: &pkSlice},
		}
		actor := bytes32(0x33)
		got, err := FetchRecipientIndexKey(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchRecipientIndexKey: %v", err)
		}
		if caller.gotMethod != MethodFetchRecipientIndexKey {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", body.ActorID)
		}
		if !bytesEq(got, pk[:]) {
			t.Errorf("pubkey: %x", got)
		}
	})

	t.Run("FetchRecipientIndexKey_NoPubkey", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: fetchRecipientIndexKeyReply{Pubkey: nil},
		}
		actor := bytes32(0x33)
		got, err := FetchRecipientIndexKey(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchRecipientIndexKey: %v", err)
		}
		if got != nil {
			t.Errorf("expected nil, got %x", got)
		}
	})

	t.Run("ReportSessionClose", func(t *testing.T) {
		caller := &recordingCaller{replyBody: reportSessionCloseReply{OK: true}}
		actor := bytes32(0x55)
		if err := ReportSessionClose(ctx, caller, actor[:], "cred-x", "logout", 1_700_000_000); err != nil {
			t.Fatalf("ReportSessionClose: %v", err)
		}
		if caller.gotMethod != MethodReportSessionClose {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID      []byte `cbor:"actor_id"`
			CredentialID string `cbor:"credential_id"`
			Reason       string `cbor:"reason"`
			OccurredAt   int64  `cbor:"occurred_at"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) ||
			body.CredentialID != "cred-x" ||
			body.Reason != "logout" ||
			body.OccurredAt != 1_700_000_000 {
			t.Errorf("body: %+v", body)
		}
	})

	t.Run("ReportSessionClose_ServerNotOK", func(t *testing.T) {
		caller := &recordingCaller{replyBody: reportSessionCloseReply{OK: false}}
		actor := bytes32(0x55)
		err := ReportSessionClose(ctx, caller, actor[:], "cred-x", "logout", 1)
		if err == nil {
			t.Fatalf("expected error on ok=false reply")
		}
	})

	t.Run("SelectMailbox_Selected", func(t *testing.T) {
		first := uint32(7)
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":          "selected",
				"uid_validity":     uint32(1234),
				"uid_next":         uint32(99),
				"highestmodseq":    int64(42),
				"exists":           uint32(10),
				"recent":           uint32(0),
				"unseen":           uint32(3),
				"first_unseen_uid": first,
			},
		}
		actor := bytes32(0x21)
		got, err := SelectMailbox(ctx, caller, actor[:], "INBOX", nil)
		if err != nil {
			t.Fatalf("SelectMailbox: %v", err)
		}
		if caller.gotMethod != MethodSelectMailbox {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
			Mailbox string `cbor:"mailbox"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || body.Mailbox != "INBOX" {
			t.Errorf("body: %+v", body)
		}
		if got == nil {
			t.Fatalf("expected SelectedMailbox, got nil")
		}
		if got.UIDValidity != 1234 || got.UIDNext != 99 || got.HighestModseq != 42 ||
			got.Exists != 10 || got.Recent != 0 || got.Unseen != 3 {
			t.Errorf("payload: %+v", got)
		}
		if got.FirstUnseenUID == nil || *got.FirstUnseenUID != 7 {
			t.Errorf("first_unseen_uid: %v", got.FirstUnseenUID)
		}
	})

	t.Run("SelectMailbox_NoFirstUnseen", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":       "selected",
				"uid_validity":  uint32(1),
				"uid_next":      uint32(1),
				"highestmodseq": int64(1),
				"exists":        uint32(0),
				"recent":        uint32(0),
				"unseen":        uint32(0),
				// first_unseen_uid omitted = None
			},
		}
		actor := bytes32(0x22)
		got, err := SelectMailbox(ctx, caller, actor[:], "Drafts", nil)
		if err != nil {
			t.Fatalf("SelectMailbox: %v", err)
		}
		if got.FirstUnseenUID != nil {
			t.Errorf("first_unseen_uid should be nil, got %v", *got.FirstUnseenUID)
		}
	})

	t.Run("SelectMailbox_NoSuchMailbox", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{"outcome": "no_such_mailbox"},
		}
		actor := bytes32(0x23)
		got, err := SelectMailbox(ctx, caller, actor[:], "Ghost", nil)
		if err == nil {
			t.Fatalf("expected ErrNoSuchMailbox, got payload %+v", got)
		}
		if !errors.Is(err, ErrNoSuchMailbox) {
			t.Errorf("err is not ErrNoSuchMailbox: %v", err)
		}
		if got != nil {
			t.Errorf("payload should be nil on rejected, got %+v", got)
		}
	})

	t.Run("SelectMailbox_ForwardsQResyncHint", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":       "selected",
				"uid_validity":  uint32(1),
				"uid_next":      uint32(1),
				"highestmodseq": int64(1),
				"exists":        uint32(0),
				"recent":        uint32(0),
				"unseen":        uint32(0),
			},
		}
		actor := bytes32(0x24)
		_, err := SelectMailbox(ctx, caller, actor[:], "INBOX",
			&QResyncHint{LastUIDValidity: 1700, LastModseq: 99})
		if err != nil {
			t.Fatalf("SelectMailbox: %v", err)
		}
		var body struct {
			ClientQresync *QResyncHint `cbor:"client_qresync"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.ClientQresync == nil {
			t.Fatalf("client_qresync must be encoded when the hint is non-nil")
		}
		if body.ClientQresync.LastUIDValidity != 1700 || body.ClientQresync.LastModseq != 99 {
			t.Errorf("client_qresync: %+v", body.ClientQresync)
		}
	})

	t.Run("SelectMailbox_OmitsQResyncHintWhenNil", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":       "selected",
				"uid_validity":  uint32(1),
				"uid_next":      uint32(1),
				"highestmodseq": int64(1),
				"exists":        uint32(0),
				"recent":        uint32(0),
				"unseen":        uint32(0),
			},
		}
		actor := bytes32(0x25)
		if _, err := SelectMailbox(ctx, caller, actor[:], "INBOX", nil); err != nil {
			t.Fatalf("SelectMailbox: %v", err)
		}
		// The omitempty tag must drop client_qresync entirely when nil so
		// the nest's restore-divergence seam never fires for a plain SELECT.
		var raw map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &raw); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if _, present := raw["client_qresync"]; present {
			t.Errorf("client_qresync must be omitted for a nil hint, got %v", raw["client_qresync"])
		}
	})

	t.Run("ListMessages_BasicAndPagination", func(t *testing.T) {
		mid1 := bytes32(0xaa)
		mid2 := bytes32(0xbb)
		caller := &recordingCaller{
			replyBody: ListMessagesReply{
				Messages: []MessageMeta{
					{
						UID:            10,
						MessageID:      mid1[:],
						Modseq:         100,
						Flags:          []string{"\\Seen"},
						InternalDate:   1_700_000_000,
						CiphertextSize: 4096,
					},
					{
						UID:            11,
						MessageID:      mid2[:],
						Modseq:         101,
						Flags:          []string{"\\Seen", "\\Flagged"},
						InternalDate:   1_700_000_100,
						CiphertextSize: 8192,
					},
				},
				ExpungedUIDs:  []uint32{},
				HighestModseq: 101,
				More:          true,
			},
		}
		actor := bytes32(0x44)
		since := int64(50)
		after := uint32(9)
		got, err := ListMessages(ctx, caller, ListMessagesParams{
			ActorID:     actor[:],
			Mailbox:     "INBOX",
			SinceModseq: &since,
			Limit:       2,
			AfterUID:    &after,
		})
		if err != nil {
			t.Fatalf("ListMessages: %v", err)
		}
		if caller.gotMethod != MethodListMessages {
			t.Errorf("method: %q", caller.gotMethod)
		}
		var body struct {
			ActorID     []byte  `cbor:"actor_id"`
			Mailbox     string  `cbor:"mailbox"`
			SinceModseq *int64  `cbor:"since_modseq"`
			Limit       uint32  `cbor:"limit"`
			AfterUID    *uint32 `cbor:"after_uid"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || body.Mailbox != "INBOX" || body.Limit != 2 {
			t.Errorf("body: %+v", body)
		}
		if body.SinceModseq == nil || *body.SinceModseq != 50 {
			t.Errorf("since_modseq: %v", body.SinceModseq)
		}
		if body.AfterUID == nil || *body.AfterUID != 9 {
			t.Errorf("after_uid: %v", body.AfterUID)
		}
		if len(got.Messages) != 2 || got.HighestModseq != 101 || !got.More {
			t.Errorf("reply: %+v", got)
		}
		if got.Messages[0].UID != 10 || got.Messages[1].UID != 11 ||
			!bytesEq(got.Messages[0].MessageID, mid1[:]) ||
			got.Messages[1].CiphertextSize != 8192 {
			t.Errorf("messages: %+v", got.Messages)
		}
	})

	t.Run("ListMessages_NoneOptions", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: ListMessagesReply{
				Messages:      []MessageMeta{},
				ExpungedUIDs:  []uint32{},
				HighestModseq: 1,
				More:          false,
			},
		}
		actor := bytes32(0x45)
		_, err := ListMessages(ctx, caller, ListMessagesParams{
			ActorID: actor[:],
			Mailbox: "Drafts",
			// SinceModseq nil, AfterUID nil — must encode as CBOR null.
		})
		if err != nil {
			t.Fatalf("ListMessages: %v", err)
		}
		var body struct {
			SinceModseq *int64  `cbor:"since_modseq"`
			AfterUID    *uint32 `cbor:"after_uid"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.SinceModseq != nil {
			t.Errorf("since_modseq: should be None, got %v", *body.SinceModseq)
		}
		if body.AfterUID != nil {
			t.Errorf("after_uid: should be None, got %v", *body.AfterUID)
		}
	})

	t.Run("FetchMessageMetadata_ExplicitUIDs", func(t *testing.T) {
		mid := bytes32(0x77)
		caller := &recordingCaller{
			replyBody: fetchMessageMetadataReply{
				Messages: []MessageMeta{
					{
						UID:            5,
						MessageID:      mid[:],
						Modseq:         42,
						Flags:          []string{},
						InternalDate:   1_700_000_500,
						CiphertextSize: 1024,
					},
				},
			},
		}
		actor := bytes32(0x46)
		got, err := FetchMessageMetadata(ctx, caller, actor[:], "INBOX", []uint32{5, 6, 7})
		if err != nil {
			t.Fatalf("FetchMessageMetadata: %v", err)
		}
		if caller.gotMethod != MethodFetchMessageMetadata {
			t.Errorf("method: %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte   `cbor:"actor_id"`
			Mailbox string   `cbor:"mailbox"`
			Uids    []uint32 `cbor:"uids"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || body.Mailbox != "INBOX" {
			t.Errorf("body: %+v", body)
		}
		if len(body.Uids) != 3 || body.Uids[0] != 5 || body.Uids[2] != 7 {
			t.Errorf("uids: %+v", body.Uids)
		}
		if len(got) != 1 || got[0].UID != 5 || got[0].Modseq != 42 {
			t.Errorf("messages: %+v", got)
		}
	})

	t.Run("FetchMessageCiphertext_Found", func(t *testing.T) {
		mid := bytes32(0x88)
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":         "found",
				"encrypted_body":  []byte{0x01, 0x02, 0x03, 0x04},
				"ciphertext_size": uint32(4),
				"internal_date":   int64(1_700_000_777),
			},
		}
		actor := bytes32(0x47)
		got, err := FetchMessageCiphertext(ctx, caller, actor[:], mid[:])
		if err != nil {
			t.Fatalf("FetchMessageCiphertext: %v", err)
		}
		if caller.gotMethod != MethodFetchMessageCiphertext {
			t.Errorf("method: %q", caller.gotMethod)
		}
		var body struct {
			ActorID   []byte `cbor:"actor_id"`
			MessageID []byte `cbor:"message_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || !bytesEq(body.MessageID, mid[:]) {
			t.Errorf("body: %+v", body)
		}
		if got == nil {
			t.Fatalf("expected FetchedCiphertext, got nil")
		}
		if !bytesEq(got.EncryptedBody, []byte{0x01, 0x02, 0x03, 0x04}) {
			t.Errorf("encrypted_body: %x", got.EncryptedBody)
		}
		if got.CiphertextSize != 4 || got.InternalDate != 1_700_000_777 {
			t.Errorf("payload: %+v", got)
		}
	})

	t.Run("FetchMessageCiphertext_NotFound", func(t *testing.T) {
		mid := bytes32(0x89)
		caller := &recordingCaller{
			replyBody: map[string]any{"outcome": "not_found"},
		}
		actor := bytes32(0x48)
		got, err := FetchMessageCiphertext(ctx, caller, actor[:], mid[:])
		if err == nil {
			t.Fatalf("expected ErrMessageNotFound, got payload %+v", got)
		}
		if !errors.Is(err, ErrMessageNotFound) {
			t.Errorf("err is not ErrMessageNotFound: %v", err)
		}
		if got != nil {
			t.Errorf("payload should be nil on not_found, got %+v", got)
		}
	})

	t.Run("ListMailboxes", func(t *testing.T) {
		entries := []MailboxEntry{
			{Name: "INBOX", UIDValidity: 100, UIDNext: 5, HighestModseq: 42, Exists: 4, Unseen: 2},
			{Name: "Drafts", UIDValidity: 101, UIDNext: 1, HighestModseq: 1, Exists: 0, Unseen: 0},
		}
		caller := &recordingCaller{
			replyBody: listMailboxesReply{Mailboxes: entries},
		}
		actor := bytes32(0x11)
		got, err := ListMailboxes(ctx, caller, actor[:], false)
		if err != nil {
			t.Fatalf("ListMailboxes: %v", err)
		}
		if caller.gotMethod != MethodListMailboxes {
			t.Errorf("method: got %q, want fauna.bridges.list_mailboxes", caller.gotMethod)
		}
		var body struct {
			ActorID        []byte `cbor:"actor_id"`
			SubscribedOnly bool   `cbor:"subscribed_only"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: got %x, want %x", body.ActorID, actor[:])
		}
		if body.SubscribedOnly {
			t.Errorf("subscribed_only: got true, want false")
		}
		if len(got) != 2 {
			t.Fatalf("entries: got %d, want 2", len(got))
		}
		if got[0].Name != "INBOX" || got[0].UIDValidity != 100 || got[0].UIDNext != 5 ||
			got[0].HighestModseq != 42 || got[0].Exists != 4 || got[0].Unseen != 2 {
			t.Errorf("entries[0]: %+v", got[0])
		}
		if got[1].Name != "Drafts" {
			t.Errorf("entries[1]: %+v", got[1])
		}
	})

	t.Run("ListMailboxes_SubscribedOnly", func(t *testing.T) {
		entries := []MailboxEntry{
			{Name: "INBOX", UIDValidity: 100, UIDNext: 5, HighestModseq: 42, Exists: 4, Unseen: 2},
		}
		caller := &recordingCaller{
			replyBody: listMailboxesReply{Mailboxes: entries},
		}
		actor := bytes32(0x12)
		got, err := ListMailboxes(ctx, caller, actor[:], true)
		if err != nil {
			t.Fatalf("ListMailboxes: %v", err)
		}
		var body struct {
			ActorID        []byte `cbor:"actor_id"`
			SubscribedOnly bool   `cbor:"subscribed_only"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !body.SubscribedOnly {
			t.Errorf("subscribed_only: got false, want true")
		}
		if len(got) != 1 || got[0].Name != "INBOX" {
			t.Fatalf("entries: %+v", got)
		}
	})

	t.Run("SubscribeMailbox_Subscribed", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: subscribeMailboxReply{Outcome: "subscribed"},
		}
		actor := bytes32(0x13)
		if err := SubscribeMailbox(ctx, caller, actor[:], "Saved Searches"); err != nil {
			t.Fatalf("SubscribeMailbox: %v", err)
		}
		if caller.gotMethod != "fauna.bridges.subscribe_mailbox" {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
			Mailbox string `cbor:"mailbox"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: got %x, want %x", body.ActorID, actor[:])
		}
		if body.Mailbox != "Saved Searches" {
			t.Errorf("mailbox: got %q", body.Mailbox)
		}
	})

	t.Run("SubscribeMailbox_PropagatesTransportError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("transport failure")}
		actor := bytes32(0x14)
		if err := SubscribeMailbox(ctx, caller, actor[:], "INBOX"); err == nil {
			t.Fatalf("expected transport error to propagate")
		}
	})

	t.Run("SubscribeMailbox_UnknownOutcomeIsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: subscribeMailboxReply{Outcome: "future_variant"},
		}
		actor := bytes32(0x15)
		err := SubscribeMailbox(ctx, caller, actor[:], "INBOX")
		if err == nil || !strings.Contains(err.Error(), "unknown outcome") {
			t.Fatalf("expected unknown-outcome error, got %v", err)
		}
	})

	t.Run("UnsubscribeMailbox_Unsubscribed", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: unsubscribeMailboxReply{Outcome: "unsubscribed"},
		}
		actor := bytes32(0x16)
		if err := UnsubscribeMailbox(ctx, caller, actor[:], "Saved Searches"); err != nil {
			t.Fatalf("UnsubscribeMailbox: %v", err)
		}
		if caller.gotMethod != "fauna.bridges.unsubscribe_mailbox" {
			t.Errorf("method: got %q", caller.gotMethod)
		}
	})

	t.Run("UnsubscribeMailbox_PropagatesTransportError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("transport failure")}
		actor := bytes32(0x17)
		if err := UnsubscribeMailbox(ctx, caller, actor[:], "INBOX"); err == nil {
			t.Fatalf("expected transport error to propagate")
		}
	})

	t.Run("WrapperPropagatesTransportError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("transport failure")}
		_, err := Whoami(ctx, caller)
		if err == nil {
			t.Fatalf("expected transport error to propagate")
		}
	})

	t.Run("FetchIndexSegmentsSince_AllMailboxes", func(t *testing.T) {
		mid := bytes32(0xaa)
		caller := &recordingCaller{
			replyBody: FetchIndexSegmentsSinceReply{
				Segments: []IndexSegment{
					{
						MessageID:          mid[:],
						Mailbox:            "INBOX",
						Modseq:             2,
						EncryptedIndexHint: []byte("hint-a"),
					},
				},
				HighestModseq: 5,
				More:          false,
			},
		}
		actor := bytes32(0x60)
		got, err := FetchIndexSegmentsSince(ctx, caller, FetchIndexSegmentsSinceParams{
			ActorID:     actor[:],
			Mailbox:     nil,
			SinceModseq: 0,
			Limit:       100,
		})
		if err != nil {
			t.Fatalf("FetchIndexSegmentsSince: %v", err)
		}
		if caller.gotMethod != MethodFetchIndexSegmentsSince {
			t.Errorf("method: %q", caller.gotMethod)
		}
		// Body shape: actor_id bstr, mailbox null, since_modseq i64, limit u32.
		var body struct {
			ActorID     []byte `cbor:"actor_id"`
			Mailbox     any    `cbor:"mailbox"`
			SinceModseq int64  `cbor:"since_modseq"`
			Limit       uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: %x vs %x", body.ActorID, actor[:])
		}
		if body.Mailbox != nil {
			t.Errorf("mailbox: got %v, want nil (Option::None)", body.Mailbox)
		}
		if body.SinceModseq != 0 || body.Limit != 100 {
			t.Errorf("body: %+v", body)
		}
		if len(got.Segments) != 1 || got.Segments[0].Modseq != 2 || got.HighestModseq != 5 {
			t.Errorf("reply: %+v", got)
		}
	})

	t.Run("FetchIndexSegmentsSince_ScopedMailbox", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: FetchIndexSegmentsSinceReply{Segments: nil, HighestModseq: 1, More: false},
		}
		actor := bytes32(0x61)
		mailbox := "Sent"
		_, err := FetchIndexSegmentsSince(ctx, caller, FetchIndexSegmentsSinceParams{
			ActorID:     actor[:],
			Mailbox:     &mailbox,
			SinceModseq: 42,
			Limit:       0,
		})
		if err != nil {
			t.Fatalf("FetchIndexSegmentsSince: %v", err)
		}
		var body struct {
			Mailbox     *string `cbor:"mailbox"`
			SinceModseq int64   `cbor:"since_modseq"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.Mailbox == nil || *body.Mailbox != "Sent" {
			t.Errorf("mailbox: %+v (want \"Sent\")", body.Mailbox)
		}
		if body.SinceModseq != 42 {
			t.Errorf("since_modseq: %d", body.SinceModseq)
		}
	})

	t.Run("SearchMessages_FlagAndHeaderTerms", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: searchMessagesReply{UIDs: []uint32{2, 5, 9}},
		}
		actor := bytes32(0x62)
		terms := []SearchTerm{
			NewSearchTermHasFlag("\\Seen"),
			NewSearchTermHeaderContains(HeaderFieldFrom, "example.com"),
		}
		uids, err := SearchMessages(ctx, caller, actor[:], "INBOX", terms)
		if err != nil {
			t.Fatalf("SearchMessages: %v", err)
		}
		if caller.gotMethod != MethodSearchMessages {
			t.Errorf("method: %q", caller.gotMethod)
		}
		// Wire body shape: actor_id, mailbox, terms (list of map with kind).
		var body struct {
			ActorID []byte `cbor:"actor_id"`
			Mailbox string `cbor:"mailbox"`
			Terms   []struct {
				Kind  string `cbor:"kind"`
				Flag  string `cbor:"flag"`
				Field string `cbor:"field"`
				Value string `cbor:"value"`
			} `cbor:"terms"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || body.Mailbox != "INBOX" {
			t.Errorf("body: %+v", body)
		}
		if len(body.Terms) != 2 {
			t.Fatalf("terms: got %d, want 2", len(body.Terms))
		}
		if body.Terms[0].Kind != "has_flag" || body.Terms[0].Flag != "\\Seen" {
			t.Errorf("terms[0]: %+v", body.Terms[0])
		}
		if body.Terms[1].Kind != "header_contains" ||
			body.Terms[1].Field != "from" ||
			body.Terms[1].Value != "example.com" {
			t.Errorf("terms[1]: %+v", body.Terms[1])
		}
		if len(uids) != 3 || uids[0] != 2 || uids[2] != 9 {
			t.Errorf("uids: %+v", uids)
		}
	})

	t.Run("SearchMessages_NilTermsEncodesEmptyListNotNull", func(t *testing.T) {
		// `UID SEARCH ALL` reaches SearchMessages with no server-side terms (a
		// nil slice). The wire MUST carry the canonical empty list `[]` (0x80),
		// not `null` (0xf6): the Rust `SearchMessagesRequest.terms: Vec<...>`
		// strict-decodes and rejects null for a list, which surfaced as
		// `UID SEARCH ALL` → tagged NO (nest replied ok=false). Regression guard
		// for the nil→[] coercion in SearchMessages.
		caller := &recordingCaller{replyBody: searchMessagesReply{UIDs: []uint32{1, 2, 3}}}
		actor := bytes32(0x71)
		uids, err := SearchMessages(ctx, caller, actor[:], "INBOX", nil)
		if err != nil {
			t.Fatalf("SearchMessages(nil terms): %v", err)
		}
		var raw map[string]cbor.RawMessage
		if err := cbor.Unmarshal(caller.gotBody, &raw); err != nil {
			t.Fatalf("decode body map: %v", err)
		}
		termsRaw, ok := raw["terms"]
		if !ok {
			t.Fatalf("body must carry a `terms` key; got % x", caller.gotBody)
		}
		// 0x80 = empty CBOR array; 0xf6 = null. Reject null explicitly.
		if len(termsRaw) != 1 || termsRaw[0] != 0x80 {
			t.Errorf("terms must encode as canonical empty array (0x80), got % x", []byte(termsRaw))
		}
		if len(uids) != 3 {
			t.Errorf("uids: %+v", uids)
		}
	})

	t.Run("SearchMessages_DateAndSizeTerms", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: searchMessagesReply{UIDs: nil},
		}
		actor := bytes32(0x63)
		terms := []SearchTerm{
			NewSearchTermSinceInternalDate(1_700_000_000),
			NewSearchTermBeforeInternalDate(1_800_000_000),
			NewSearchTermLarger(1024),
			NewSearchTermSmaller(1_000_000),
			NewSearchTermLacksFlag("\\Deleted"),
		}
		uids, err := SearchMessages(ctx, caller, actor[:], "INBOX", terms)
		if err != nil {
			t.Fatalf("SearchMessages: %v", err)
		}
		if len(uids) != 0 {
			t.Errorf("uids: %+v, want empty", uids)
		}
		var body struct {
			Terms []map[string]any `cbor:"terms"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(body.Terms) != 5 {
			t.Fatalf("terms count: %d", len(body.Terms))
		}
		// Spot-check each kind plus its payload field.
		want := []struct {
			kind  string
			field string
			val   any
		}{
			{"since_internal_date", "ts", int64(1_700_000_000)},
			{"before_internal_date", "ts", int64(1_800_000_000)},
			{"larger", "size", uint64(1024)},
			{"smaller", "size", uint64(1_000_000)},
			{"lacks_flag", "flag", "\\Deleted"},
		}
		for i, w := range want {
			got := body.Terms[i]
			if got["kind"] != w.kind {
				t.Errorf("term[%d].kind: %v want %s", i, got["kind"], w.kind)
			}
			// CBOR decoding of unknown maps lifts ints to uint64. Compare loosely.
			gv := got[w.field]
			switch wv := w.val.(type) {
			case int64:
				if g, ok := gv.(int64); ok {
					if g != wv {
						t.Errorf("term[%d].%s: %v want %d", i, w.field, gv, wv)
					}
				} else if g, ok := gv.(uint64); ok {
					if int64(g) != wv {
						t.Errorf("term[%d].%s: %v want %d", i, w.field, gv, wv)
					}
				}
			case uint64:
				if g, ok := gv.(uint64); ok {
					if g != wv {
						t.Errorf("term[%d].%s: %v want %d", i, w.field, gv, wv)
					}
				}
			case string:
				if gv != wv {
					t.Errorf("term[%d].%s: %v want %s", i, w.field, gv, wv)
				}
			}
		}
	})

	t.Run("SearchMessages_EmptyTermsValid", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: searchMessagesReply{UIDs: []uint32{1, 2}},
		}
		actor := bytes32(0x64)
		uids, err := SearchMessages(ctx, caller, actor[:], "INBOX", nil)
		if err != nil {
			t.Fatalf("SearchMessages: %v", err)
		}
		if len(uids) != 2 {
			t.Errorf("uids: %+v", uids)
		}
	})

	t.Run("GetQuota_TranslatesReplyAndEncodesActor", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: GetQuotaReply{
				StorageBytesUsed:  600,
				MessageCountUsed:  3,
				StorageBytesLimit: 1 << 30,
				MessageCountLimit: 50_000,
			},
		}
		actor := bytes32(0x70)
		got, err := GetQuota(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("GetQuota: %v", err)
		}
		if caller.gotMethod != MethodGetQuota {
			t.Errorf("method: %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", body.ActorID)
		}
		if got.StorageBytesUsed != 600 || got.MessageCountUsed != 3 {
			t.Errorf("used: %+v", got)
		}
		if got.StorageBytesLimit != 1<<30 || got.MessageCountLimit != 50_000 {
			t.Errorf("limits: %+v", got)
		}
	})

	t.Run("GetQuota_PropagatesUnderlyingError", func(t *testing.T) {
		wantErr := errors.New("synthetic-failure")
		caller := &recordingCaller{returnErr: wantErr}
		actor := bytes32(0x71)
		_, err := GetQuota(ctx, caller, actor[:])
		if err == nil || !strings.Contains(err.Error(), "synthetic-failure") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("StoreFlags_AddSeenNoCondstore", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: StoreFlagsReply{
				Updated: []StoreFlagsResultEntry{
					{UID: 1, Flags: []string{"\\Seen"}, ModSeq: 11},
					{UID: 2, Flags: []string{"\\Seen"}, ModSeq: 11},
				},
				HighestModSeq: 11,
			},
		}
		actor := bytes32(0x90)
		reply, err := StoreFlags(ctx, caller, StoreFlagsParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
			UIDs:    []uint32{1, 2},
			Op:      StoreFlagsOpAdd,
			Flags:   []string{"\\Seen"},
		})
		if err != nil {
			t.Fatalf("StoreFlags: %v", err)
		}
		if caller.gotMethod != MethodStoreFlags {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if len(reply.Updated) != 2 || reply.HighestModSeq != 11 {
			t.Errorf("reply: %+v", reply)
		}
		if len(reply.Modified) != 0 {
			t.Errorf("modified must be empty when no UNCHANGEDSINCE: %v", reply.Modified)
		}
		// `op` must encode as the snake_case string "add".
		// `unchanged_since` must be absent from the wire when nil.
		var got struct {
			Op             string `cbor:"op"`
			UnchangedSince *int64 `cbor:"unchanged_since"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if got.Op != "add" {
			t.Errorf("op: %q want add", got.Op)
		}
		if got.UnchangedSince != nil {
			t.Errorf("unchanged_since must be omitted (got %v)", *got.UnchangedSince)
		}
	})

	t.Run("StoreFlags_UnchangedSincePresentEncoded", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: StoreFlagsReply{
				Updated:       []StoreFlagsResultEntry{{UID: 1, Flags: []string{"\\Seen"}, ModSeq: 12}},
				HighestModSeq: 12,
				Modified:      []uint32{2},
			},
		}
		actor := bytes32(0x91)
		unchanged := int64(3)
		reply, err := StoreFlags(ctx, caller, StoreFlagsParams{
			ActorID:        actor[:],
			Mailbox:        "INBOX",
			UIDs:           []uint32{1, 2},
			Op:             StoreFlagsOpAdd,
			Flags:          []string{"\\Seen"},
			UnchangedSince: &unchanged,
		})
		if err != nil {
			t.Fatalf("StoreFlags: %v", err)
		}
		if len(reply.Modified) != 1 || reply.Modified[0] != 2 {
			t.Errorf("modified: %v want [2]", reply.Modified)
		}
		var got struct {
			UnchangedSince *int64 `cbor:"unchanged_since"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if got.UnchangedSince == nil || *got.UnchangedSince != 3 {
			t.Errorf("unchanged_since: %v want 3", got.UnchangedSince)
		}
	})

	t.Run("StoreFlags_OpRemoveEncodesSnakeCase", func(t *testing.T) {
		caller := &recordingCaller{replyBody: StoreFlagsReply{}}
		actor := bytes32(0x92)
		if _, err := StoreFlags(ctx, caller, StoreFlagsParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
			UIDs:    []uint32{1},
			Op:      StoreFlagsOpRemove,
			Flags:   []string{"\\Seen"},
		}); err != nil {
			t.Fatalf("StoreFlags: %v", err)
		}
		var got struct {
			Op string `cbor:"op"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if got.Op != "remove" {
			t.Errorf("op: %q want remove", got.Op)
		}
	})

	t.Run("StoreFlags_PropagatesUnderlyingError", func(t *testing.T) {
		wantErr := errors.New("synthetic-store-failure")
		caller := &recordingCaller{returnErr: wantErr}
		actor := bytes32(0x93)
		_, err := StoreFlags(ctx, caller, StoreFlagsParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
			UIDs:    []uint32{1},
			Op:      StoreFlagsOpSet,
			Flags:   []string{"\\Seen"},
		})
		if err == nil || !strings.Contains(err.Error(), "synthetic-store-failure") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("FetchSpamModel_TrainedReturnsSealedBlob", func(t *testing.T) {
		want := []byte("sealed-model-bytes")
		caller := &recordingCaller{
			replyBody: fetchSpamModelReply{Blob: want},
		}
		actor := bytes32(0x8c)
		got, storedSealed, _, _, _, err := FetchSpamModel(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchSpamModel: %v", err)
		}
		if storedSealed {
			t.Errorf("stored_sealed: absent from the reply must decode false")
		}
		if caller.gotMethod != MethodFetchSpamModel {
			t.Errorf("method: got %q, want fauna.bridges.fetch_spam_model", caller.gotMethod)
		}
		// actor_id is `#[serde(with = "serde_bytes")]` on the nest side — a
		// CBOR byte string, not a Vec<u8> array. Assert the wrapper lowered
		// it to a byte string (fxamacker decodes a byte string into []byte)
		// carrying the exact actor bytes.
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytes.Equal(body.ActorID, actor[:]) {
			t.Errorf("actor_id wire: got %x, want %x", body.ActorID, actor[:])
		}
		if !bytes.Equal(got, want) {
			t.Errorf("blob: got %x, want %x", got, want)
		}
	})

	t.Run("FetchSpamModel_UntrainedReturnsNil", func(t *testing.T) {
		// nest serializes `blob: None`; the wrapper surfaces a nil/empty
		// slice ⇒ the caller treats it as cold start.
		caller := &recordingCaller{
			replyBody: fetchSpamModelReply{Blob: nil},
		}
		actor := bytes32(0x8d)
		got, _, _, _, _, err := FetchSpamModel(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchSpamModel: %v", err)
		}
		if len(got) != 0 {
			t.Errorf("untrained blob: got %x, want empty", got)
		}
	})

	t.Run("FetchSpamModel_SurfacesStoredSealed", func(t *testing.T) {
		// The leg-2 dispatch signal: a nest holding a client-sealed model
		// at rest returns it verbatim with `stored_sealed: true`.
		caller := &recordingCaller{
			replyBody: fetchSpamModelReply{Blob: []byte("opaque"), StoredSealed: true},
		}
		actor := bytes32(0x8f)
		_, storedSealed, _, _, _, err := FetchSpamModel(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchSpamModel: %v", err)
		}
		if !storedSealed {
			t.Errorf("stored_sealed: got false, want true")
		}
	})

	t.Run("FetchSpamModel_SurfacesBaseline", func(t *testing.T) {
		// The published deployment baseline rides the reply only for a
		// client-sealed stored model (the nest folds a plaintext-stored one
		// itself — the no-double-fold rule). The SCORING caller folds it
		// locally; the TRAIN caller discards it (read-time-only). Absent
		// key ⇒ nil (covered by the subtests above, whose replies omit it).
		caller := &recordingCaller{
			replyBody: fetchSpamModelReply{
				Blob:         []byte("opaque"),
				StoredSealed: true,
				Baseline:     []byte("baseline-aggregate"),
			},
		}
		actor := bytes32(0x93)
		_, _, baseline, _, _, err := FetchSpamModel(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchSpamModel: %v", err)
		}
		if string(baseline) != "baseline-aggregate" {
			t.Errorf("baseline: got %q, want baseline-aggregate", baseline)
		}
	})

	t.Run("PutSpamModel_EncodesTrustedNamingAndHistoryOp", func(t *testing.T) {
		// The leg-2 write-back: actor_id names the served actor
		// (byte-string), the history_op is the externally-tagged
		// `{"Insert": {...}}` map, and sealed bytes ride as byte strings.
		caller := &recordingCaller{replyBody: putSpamModelReply{}}
		actor := bytes32(0x90)
		mid := bytes32(0x91)
		insert := &SpamHistoryInsert{
			MessageID:     mid[:],
			Mailbox:       "Junk",
			SealedSubject: []byte("sealed-subject"),
			SealedDelta:   []byte("sealed-delta"),
			Label:         SpamLabelSpam,
			Source:        TrainingSourceImapJunkFlag,
		}
		outcome, err := PutSpamModel(ctx, caller, actor[:], []byte("resealed-model"), 0, insert, nil)
		if err != nil {
			t.Fatalf("PutSpamModel: %v", err)
		}
		if outcome != PutOutcomeWritten {
			t.Errorf("a bare ack (a nest from before the outcome field) reads as written; got %q", outcome)
		}
		if caller.gotMethod != MethodPutSpamModel {
			t.Errorf("method: got %q, want fauna.bridges.put_spam_model", caller.gotMethod)
		}
		var body struct {
			ActorID     []byte `cbor:"actor_id"`
			SealedModel []byte `cbor:"sealed_model"`
			HistoryOp   struct {
				Insert struct {
					MessageID     []byte `cbor:"message_id"`
					Mailbox       string `cbor:"mailbox"`
					SealedSubject []byte `cbor:"sealed_subject"`
					SealedDelta   []byte `cbor:"sealed_delta"`
					Label         string `cbor:"label"`
					Source        string `cbor:"source"`
				} `cbor:"Insert"`
			} `cbor:"history_op"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytes.Equal(body.ActorID, actor[:]) {
			t.Errorf("actor_id wire: got %x, want %x", body.ActorID, actor[:])
		}
		if string(body.SealedModel) != "resealed-model" {
			t.Errorf("sealed_model: got %q", body.SealedModel)
		}
		ins := body.HistoryOp.Insert
		if ins.Mailbox != "Junk" || ins.Label != "spam" || ins.Source != "imap_junk_flag" {
			t.Errorf("insert row: %+v", ins)
		}
		if string(ins.SealedSubject) != "sealed-subject" || string(ins.SealedDelta) != "sealed-delta" {
			t.Errorf("sealed columns: %+v", ins)
		}
	})

	t.Run("PutSpamModel_NilInsertOmitsHistoryOp", func(t *testing.T) {
		// A model-only write must OMIT the history_op key entirely (the
		// nest decodes absent ⇒ None ⇒ no history mutation).
		caller := &recordingCaller{replyBody: putSpamModelReply{}}
		actor := bytes32(0x92)
		if _, err := PutSpamModel(ctx, caller, actor[:], []byte("m"), 0, nil, nil); err != nil {
			t.Fatalf("PutSpamModel: %v", err)
		}
		var body map[string]cbor.RawMessage
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if _, present := body["history_op"]; present {
			t.Errorf("history_op key must be absent for a model-only write")
		}
	})

	t.Run("FetchSpamModel_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-failure")}
		actor := bytes32(0x8e)
		if _, _, _, _, _, err := FetchSpamModel(ctx, caller, actor[:]); err == nil ||
			!strings.Contains(err.Error(), "synthetic-failure") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("Expunge_FullFormEncodesEmptyUIDs", func(t *testing.T) {
		// Plain EXPUNGE: the request UID list is empty; nest filters
		// \Deleted server-side. Wire shape mirrors
		// `bridge_routing.rs:ExpungeRequest` — actor_id, mailbox, uids.
		caller := &recordingCaller{
			replyBody: ExpungeReply{
				ExpungedUIDs:  []uint32{1, 3, 5},
				HighestModseq: 12,
			},
		}
		actor := bytes32(0xA0)
		reply, err := Expunge(ctx, caller, ExpungeParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
		})
		if err != nil {
			t.Fatalf("Expunge: %v", err)
		}
		if caller.gotMethod != MethodExpunge {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if len(reply.ExpungedUIDs) != 3 || reply.HighestModseq != 12 {
			t.Errorf("reply: %+v", reply)
		}
		var got struct {
			ActorID []byte   `cbor:"actor_id"`
			Mailbox string   `cbor:"mailbox"`
			UIDs    []uint32 `cbor:"uids"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(got.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", got.ActorID)
		}
		if got.Mailbox != "INBOX" {
			t.Errorf("mailbox: %q", got.Mailbox)
		}
		if len(got.UIDs) != 0 {
			t.Errorf("uids must be empty for plain EXPUNGE, got %v", got.UIDs)
		}
	})

	t.Run("Expunge_UIDExpungeEncodesSet", func(t *testing.T) {
		// RFC 4315 UID EXPUNGE: client passes a UID set; nest intersects
		// with \Deleted server-side. The wrapper forwards the request
		// uids verbatim.
		caller := &recordingCaller{
			replyBody: ExpungeReply{
				ExpungedUIDs:  []uint32{4},
				HighestModseq: 13,
			},
		}
		actor := bytes32(0xA1)
		reply, err := Expunge(ctx, caller, ExpungeParams{
			ActorID: actor[:],
			Mailbox: "Drafts",
			UIDs:    []uint32{2, 4, 6},
		})
		if err != nil {
			t.Fatalf("Expunge: %v", err)
		}
		if len(reply.ExpungedUIDs) != 1 || reply.ExpungedUIDs[0] != 4 {
			t.Errorf("reply: %+v", reply)
		}
		var got struct {
			UIDs []uint32 `cbor:"uids"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(got.UIDs) != 3 || got.UIDs[0] != 2 || got.UIDs[1] != 4 || got.UIDs[2] != 6 {
			t.Errorf("uids: %v want [2 4 6]", got.UIDs)
		}
	})

	t.Run("Expunge_PropagatesUnderlyingError", func(t *testing.T) {
		wantErr := errors.New("synthetic-expunge-failure")
		caller := &recordingCaller{returnErr: wantErr}
		actor := bytes32(0xA2)
		_, err := Expunge(ctx, caller, ExpungeParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
		})
		if err == nil || !strings.Contains(err.Error(), "synthetic-expunge-failure") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("Copy_EncodesPairsByOrder", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: CopyMessagesReply{
				DestUIDValidity: 7,
				Copied: []CopyPair{
					{SourceUID: 1, DestUID: 10},
					{SourceUID: 2, DestUID: 11},
				},
				DestHighestmodseq: 5,
			},
		}
		actor := bytes32(0xB0)
		reply, err := Copy(ctx, caller, CopyParams{
			ActorID:       actor[:],
			SourceMailbox: "INBOX",
			UIDs:          []uint32{1, 2},
			DestMailbox:   "Archive",
		})
		if err != nil {
			t.Fatalf("Copy: %v", err)
		}
		if caller.gotMethod != MethodCopy {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if reply.DestUIDValidity != 7 || len(reply.Copied) != 2 {
			t.Errorf("reply: %+v", reply)
		}
		var got struct {
			SourceMailbox string   `cbor:"source_mailbox"`
			DestMailbox   string   `cbor:"dest_mailbox"`
			UIDs          []uint32 `cbor:"uids"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if got.SourceMailbox != "INBOX" || got.DestMailbox != "Archive" {
			t.Errorf("mailbox fields: src=%q dst=%q", got.SourceMailbox, got.DestMailbox)
		}
		if len(got.UIDs) != 2 || got.UIDs[0] != 1 || got.UIDs[1] != 2 {
			t.Errorf("uids: %v", got.UIDs)
		}
	})

	t.Run("Move_EncodesAndDecodesBothModseqs", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: MoveMessagesReply{
				DestUIDValidity: 6,
				Moved: []CopyPair{
					{SourceUID: 1, DestUID: 30},
				},
				SourceHighestmodseq: 5,
				DestHighestmodseq:   2,
			},
		}
		actor := bytes32(0xB1)
		reply, err := Move(ctx, caller, MoveParams{
			ActorID:       actor[:],
			SourceMailbox: "INBOX",
			UIDs:          []uint32{1},
			DestMailbox:   "Archive",
		})
		if err != nil {
			t.Fatalf("Move: %v", err)
		}
		if caller.gotMethod != MethodMove {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if reply.SourceHighestmodseq != 5 || reply.DestHighestmodseq != 2 {
			t.Errorf("modseqs: src=%d dst=%d", reply.SourceHighestmodseq, reply.DestHighestmodseq)
		}
	})

	t.Run("Copy_PropagatesUnderlyingError", func(t *testing.T) {
		wantErr := errors.New("synthetic-copy-fail")
		caller := &recordingCaller{returnErr: wantErr}
		actor := bytes32(0xB2)
		_, err := Copy(ctx, caller, CopyParams{
			ActorID:       actor[:],
			SourceMailbox: "INBOX",
			UIDs:          []uint32{1},
			DestMailbox:   "Archive",
		})
		if err == nil || !strings.Contains(err.Error(), "synthetic-copy-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("Move_PropagatesUnderlyingError", func(t *testing.T) {
		wantErr := errors.New("synthetic-move-fail")
		caller := &recordingCaller{returnErr: wantErr}
		actor := bytes32(0xB3)
		_, err := Move(ctx, caller, MoveParams{
			ActorID:       actor[:],
			SourceMailbox: "INBOX",
			UIDs:          []uint32{1},
			DestMailbox:   "Archive",
		})
		if err == nil || !strings.Contains(err.Error(), "synthetic-move-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("Append_EncodesAndDecodesAllFields", func(t *testing.T) {
		// Wire shape mirrors libs/fauna-protocol/src/bridge_routing.rs:
		// AppendMessageRequest — 8 fields, in the order documented there.
		// The reply carries the server-assigned message_id plus the
		// landed (uid_validity, uid) — those flow back as
		// `*imap.AppendData` from Session.Append for the UIDPLUS
		// `OK [APPENDUID ...]` tagged response.
		want := AppendReply{
			MessageID:   []byte{0xCA, 0xFE, 0xBA, 0xBE},
			UID:         42,
			UIDValidity: 17,
		}
		caller := &recordingCaller{replyBody: want}
		actor := bytes32(0xC0)
		got, err := Append(ctx, caller, AppendParams{
			ActorID:            actor[:],
			Mailbox:            "INBOX",
			Flags:              []string{"\\Seen", "\\Draft"},
			EncryptedBody:      []byte("encrypted-body-bytes"),
			EncryptedIndexHint: []byte("encrypted-hint"),
			Timestamp:          1_700_000_000,
			CiphertextSize:     20,
			SenderDomain:       "example.org",
			DedupKey:           "msgid:v1:append@example.org",
			EnvelopeKey:        "env:v1:append",
		})
		if err != nil {
			t.Fatalf("Append: %v", err)
		}
		if caller.gotMethod != MethodAppend {
			t.Errorf("method: %q want fauna.bridges.append", caller.gotMethod)
		}
		if got.UID != 42 || got.UIDValidity != 17 {
			t.Errorf("reply uid=(%d,%d), want (42,17)", got.UID, got.UIDValidity)
		}
		if !bytesEq(got.MessageID, want.MessageID) {
			t.Errorf("message_id: %x", got.MessageID)
		}
		var body struct {
			ActorID            []byte   `cbor:"actor_id"`
			Mailbox            string   `cbor:"mailbox"`
			Flags              []string `cbor:"flags"`
			EncryptedBody      []byte   `cbor:"encrypted_body"`
			EncryptedIndexHint []byte   `cbor:"encrypted_index_hint"`
			Timestamp          int64    `cbor:"timestamp"`
			CiphertextSize     uint32   `cbor:"ciphertext_size"`
			SenderDomain       string   `cbor:"sender_domain"`
			DedupKey           string   `cbor:"dedup_key"`
			EnvelopeKey        string   `cbor:"envelope_key"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", body.ActorID)
		}
		if body.DedupKey != "msgid:v1:append@example.org" || body.EnvelopeKey != "env:v1:append" {
			t.Errorf("dedup pair: (%q, %q)", body.DedupKey, body.EnvelopeKey)
		}
		if body.Mailbox != "INBOX" {
			t.Errorf("mailbox: %q", body.Mailbox)
		}
		if len(body.Flags) != 2 || body.Flags[0] != "\\Seen" || body.Flags[1] != "\\Draft" {
			t.Errorf("flags: %v", body.Flags)
		}
		if !bytesEq(body.EncryptedBody, []byte("encrypted-body-bytes")) {
			t.Errorf("encrypted_body: %q", body.EncryptedBody)
		}
		if !bytesEq(body.EncryptedIndexHint, []byte("encrypted-hint")) {
			t.Errorf("encrypted_index_hint: %q", body.EncryptedIndexHint)
		}
		if body.Timestamp != 1_700_000_000 {
			t.Errorf("timestamp: %d", body.Timestamp)
		}
		if body.CiphertextSize != 20 {
			t.Errorf("ciphertext_size: %d", body.CiphertextSize)
		}
		if body.SenderDomain != "example.org" {
			t.Errorf("sender_domain: %q", body.SenderDomain)
		}
	})

	t.Run("Append_EmptyFlagsRoundTrip", func(t *testing.T) {
		// APPEND with no initial flags is the common case for new
		// drafts.  The wire still carries the field (Vec<String> on the
		// Rust side has no default-skip), encoded as an empty array.
		caller := &recordingCaller{replyBody: AppendReply{
			MessageID:   make([]byte, 32),
			UID:         1,
			UIDValidity: 1,
		}}
		actor := bytes32(0xC1)
		if _, err := Append(ctx, caller, AppendParams{
			ActorID:            actor[:],
			Mailbox:            "Drafts",
			EncryptedBody:      []byte("body"),
			EncryptedIndexHint: []byte("hint"),
			Timestamp:          1,
			CiphertextSize:     4,
			DedupKey:           "env:v1:draft",
			EnvelopeKey:        "env:v1:draft",
		}); err != nil {
			t.Fatalf("Append: %v", err)
		}
		var body struct {
			Flags []string `cbor:"flags"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(body.Flags) != 0 {
			t.Errorf("flags: %v want empty", body.Flags)
		}
	})

	t.Run("Append_PropagatesUnderlyingError", func(t *testing.T) {
		wantErr := errors.New("synthetic-append-fail")
		caller := &recordingCaller{returnErr: wantErr}
		actor := bytes32(0xC2)
		_, err := Append(ctx, caller, AppendParams{
			ActorID:            actor[:],
			Mailbox:            "INBOX",
			EncryptedBody:      []byte("b"),
			EncryptedIndexHint: []byte("h"),
			Timestamp:          1,
			CiphertextSize:     1,
			DedupKey:           "env:v1:b",
			EnvelopeKey:        "env:v1:b",
		})
		if err == nil || !strings.Contains(err.Error(), "synthetic-append-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── I4 Phase D.5 outbound queue ────────────────────────────────

	t.Run("FetchOutboundDue", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: fetchOutboundDueReply{
				Units: []OutboundUnit{
					{
						ID:             7,
						MessageID:      "abc@ex.com",
						OriginalSender: "alice@ex.com",
						Recipient:      "bob@dest.test",
						RawMessage:     []byte("body\r\n"),
						AttemptCount:   0,
					},
				},
			},
		}
		units, err := FetchOutboundDue(ctx, caller, 16, 60)
		if err != nil {
			t.Fatalf("FetchOutboundDue: %v", err)
		}
		if caller.gotMethod != MethodFetchOutboundDue {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			Max          uint32 `cbor:"max"`
			LeaseSeconds uint32 `cbor:"lease_seconds"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.Max != 16 || body.LeaseSeconds != 60 {
			t.Errorf("body: %+v", body)
		}
		if len(units) != 1 || units[0].ID != 7 || units[0].Recipient != "bob@dest.test" {
			t.Errorf("units: %+v", units)
		}
		if string(units[0].RawMessage) != "body\r\n" {
			t.Errorf("raw_message: %q", units[0].RawMessage)
		}
	})

	t.Run("MarkOutboundDelivered", func(t *testing.T) {
		caller := &recordingCaller{replyBody: markOutboundDeliveredReply{OK: true}}
		if err := MarkOutboundDelivered(ctx, caller, 42); err != nil {
			t.Fatalf("MarkOutboundDelivered: %v", err)
		}
		if caller.gotMethod != MethodMarkOutboundDelivered {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ID int64 `cbor:"id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.ID != 42 {
			t.Errorf("id: %d", body.ID)
		}
	})

	t.Run("MarkOutboundDelivered_OKFalseErrors", func(t *testing.T) {
		caller := &recordingCaller{replyBody: markOutboundDeliveredReply{OK: false}}
		err := MarkOutboundDelivered(ctx, caller, 42)
		if err == nil || !strings.Contains(err.Error(), "ok=false") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("MarkOutboundFailed", func(t *testing.T) {
		caller := &recordingCaller{replyBody: markOutboundFailedReply{OK: true}}
		if err := MarkOutboundFailed(ctx, caller, 8, 600, "451 4.7.1 temp"); err != nil {
			t.Fatalf("MarkOutboundFailed: %v", err)
		}
		if caller.gotMethod != MethodMarkOutboundFailed {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ID                int64  `cbor:"id"`
			RetryAfterSeconds uint32 `cbor:"retry_after_seconds"`
			LastError         string `cbor:"last_error"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.ID != 8 || body.RetryAfterSeconds != 600 || body.LastError != "451 4.7.1 temp" {
			t.Errorf("body: %+v", body)
		}
	})

	t.Run("MarkOutboundBounced", func(t *testing.T) {
		caller := &recordingCaller{replyBody: markOutboundBouncedReply{OK: true}}
		if err := MarkOutboundBounced(ctx, caller, 9, "550 5.1.1 unknown"); err != nil {
			t.Fatalf("MarkOutboundBounced: %v", err)
		}
		if caller.gotMethod != MethodMarkOutboundBounced {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ID     int64  `cbor:"id"`
			Reason string `cbor:"reason"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.ID != 9 || body.Reason != "550 5.1.1 unknown" {
			t.Errorf("body: %+v", body)
		}
	})

	t.Run("FetchOutboundDue_PropagatesError", func(t *testing.T) {
		wantErr := errors.New("synthetic-fetch-fail")
		caller := &recordingCaller{returnErr: wantErr}
		_, err := FetchOutboundDue(ctx, caller, 1, 1)
		if err == nil || !strings.Contains(err.Error(), "synthetic-fetch-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── decode_srs_bounce (mail-forwarding N4) ───────────────────

	t.Run("DecodeSrsBounce_OkCarriesForwarderAndAddresses", func(t *testing.T) {
		forwarder := bytes32(0xF0)
		caller := &recordingCaller{
			replyBody: decodeSrsBounceReply{
				Outcome:             "ok",
				ForwarderActorID:    forwarder[:],
				OriginalSender:      "carol@example.org",
				OriginalDestination: "bob@example.com",
			},
		}
		got, err := DecodeSrsBounce(ctx, caller, "SRS0=abcd=ef=42=example.org=carol")
		if err != nil {
			t.Fatalf("DecodeSrsBounce: %v", err)
		}
		if caller.gotMethod != MethodDecodeSrsBounce {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			LocalPart string `cbor:"local_part"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.LocalPart != "SRS0=abcd=ef=42=example.org=carol" {
			t.Errorf("local_part: got %q", body.LocalPart)
		}
		if got.Outcome != SrsBounceOutcomeOk {
			t.Errorf("outcome: got %q, want ok", got.Outcome)
		}
		if !bytes.Equal(got.ForwarderActorID, forwarder[:]) {
			t.Errorf("forwarder: got %x, want %x", got.ForwarderActorID, forwarder[:])
		}
		if got.OriginalSender != "carol@example.org" || got.OriginalDestination != "bob@example.com" {
			t.Errorf("addresses: %+v", got)
		}
	})

	t.Run("DecodeSrsBounce_MacFailOutcomeNoAddresses", func(t *testing.T) {
		caller := &recordingCaller{replyBody: decodeSrsBounceReply{Outcome: "mac_fail"}}
		got, err := DecodeSrsBounce(ctx, caller, "SRS0=zzzz=ef=42=example.org=carol")
		if err != nil {
			t.Fatalf("DecodeSrsBounce: %v", err)
		}
		if got.Outcome != SrsBounceOutcomeMacFail {
			t.Errorf("outcome: got %q, want mac_fail", got.Outcome)
		}
		if len(got.ForwarderActorID) != 0 {
			t.Errorf("mac_fail must carry no forwarder: %x", got.ForwarderActorID)
		}
	})

	t.Run("DecodeSrsBounce_PropagatesTransportError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-decode-fail")}
		_, err := DecodeSrsBounce(ctx, caller, "SRS0=x")
		if err == nil || !strings.Contains(err.Error(), "synthetic-decode-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── I5 Phase D.6 — CREATE / DELETE / RENAME ──────────────────

	t.Run("CreateMailbox_CreatedCarriesUIDValidity", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: createMailboxReply{
				Outcome:     "created",
				UIDValidity: 0xCAFE,
			},
		}
		actor := bytes32(0xC0)
		outcome, err := CreateMailbox(ctx, caller, actor[:], "Projects")
		if err != nil {
			t.Fatalf("CreateMailbox: %v", err)
		}
		if caller.gotMethod != MethodCreateMailbox {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if outcome.Kind != CreateMailboxOutcomeCreated {
			t.Errorf("kind: %q want created", outcome.Kind)
		}
		if outcome.UIDValidity != 0xCAFE {
			t.Errorf("uid_validity: %d want 0xCAFE", outcome.UIDValidity)
		}
		var got struct {
			ActorID []byte `cbor:"actor_id"`
			Name    string `cbor:"name"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(got.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", got.ActorID)
		}
		if got.Name != "Projects" {
			t.Errorf("name: %q want Projects", got.Name)
		}
	})

	t.Run("CreateMailbox_InvalidNameCarriesReason", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: createMailboxReply{
				Outcome: "invalid_name",
				Reason:  "too long",
			},
		}
		actor := bytes32(0xC1)
		outcome, err := CreateMailbox(ctx, caller, actor[:], "X")
		if err != nil {
			t.Fatalf("CreateMailbox: %v", err)
		}
		if outcome.Kind != CreateMailboxOutcomeInvalidName {
			t.Errorf("kind: %q want invalid_name", outcome.Kind)
		}
		if outcome.Reason != "too long" {
			t.Errorf("reason: %q want too long", outcome.Reason)
		}
	})

	t.Run("CreateMailbox_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: createMailboxReply{Outcome: "frobnicated"},
		}
		actor := bytes32(0xC2)
		_, err := CreateMailbox(ctx, caller, actor[:], "X")
		if err == nil || !strings.Contains(err.Error(), "unknown outcome") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("CreateMailbox_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-create-fail")}
		actor := bytes32(0xC3)
		_, err := CreateMailbox(ctx, caller, actor[:], "X")
		if err == nil || !strings.Contains(err.Error(), "synthetic-create-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("DeleteMailbox_NotEmptyOutcome", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteMailboxReply{Outcome: "not_empty"},
		}
		actor := bytes32(0xC4)
		outcome, err := DeleteMailbox(ctx, caller, actor[:], "Busy")
		if err != nil {
			t.Fatalf("DeleteMailbox: %v", err)
		}
		if caller.gotMethod != MethodDeleteMailbox {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if outcome != DeleteMailboxOutcomeNotEmpty {
			t.Errorf("outcome: %q want not_empty", outcome)
		}
		var got struct {
			ActorID []byte `cbor:"actor_id"`
			Name    string `cbor:"name"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(got.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", got.ActorID)
		}
		if got.Name != "Busy" {
			t.Errorf("name: %q want Busy", got.Name)
		}
	})

	t.Run("DeleteMailbox_DeletedOutcome", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteMailboxReply{Outcome: "deleted"},
		}
		actor := bytes32(0xC5)
		outcome, err := DeleteMailbox(ctx, caller, actor[:], "X")
		if err != nil {
			t.Fatalf("DeleteMailbox: %v", err)
		}
		if outcome != DeleteMailboxOutcomeDeleted {
			t.Errorf("outcome: %q want deleted", outcome)
		}
	})

	t.Run("DeleteMailbox_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteMailboxReply{Outcome: "frobnicated"},
		}
		actor := bytes32(0xC6)
		_, err := DeleteMailbox(ctx, caller, actor[:], "X")
		if err == nil || !strings.Contains(err.Error(), "unknown outcome") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("DeleteMailbox_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-delete-fail")}
		actor := bytes32(0xC7)
		_, err := DeleteMailbox(ctx, caller, actor[:], "X")
		if err == nil || !strings.Contains(err.Error(), "synthetic-delete-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("RenameMailbox_RenamedOutcome", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: renameMailboxReply{Outcome: "renamed"},
		}
		actor := bytes32(0xC8)
		outcome, err := RenameMailbox(ctx, caller, actor[:], "Old", "New")
		if err != nil {
			t.Fatalf("RenameMailbox: %v", err)
		}
		if caller.gotMethod != MethodRenameMailbox {
			t.Errorf("method: %q", caller.gotMethod)
		}
		if outcome.Kind != RenameMailboxOutcomeRenamed {
			t.Errorf("kind: %q want renamed", outcome.Kind)
		}
		var got struct {
			ActorID []byte `cbor:"actor_id"`
			OldName string `cbor:"old_name"`
			NewName string `cbor:"new_name"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &got); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(got.ActorID, actor[:]) {
			t.Errorf("actor_id: %x", got.ActorID)
		}
		if got.OldName != "Old" {
			t.Errorf("old_name: %q", got.OldName)
		}
		if got.NewName != "New" {
			t.Errorf("new_name: %q", got.NewName)
		}
	})

	t.Run("RenameMailbox_TargetReservedOutcome", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: renameMailboxReply{Outcome: "target_reserved"},
		}
		actor := bytes32(0xC9)
		outcome, err := RenameMailbox(ctx, caller, actor[:], "Foo", "Archive")
		if err != nil {
			t.Fatalf("RenameMailbox: %v", err)
		}
		if outcome.Kind != RenameMailboxOutcomeTargetReserved {
			t.Errorf("kind: %q want target_reserved", outcome.Kind)
		}
	})

	t.Run("RenameMailbox_InvalidNameCarriesReason", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: renameMailboxReply{
				Outcome: "invalid_name",
				Reason:  "contains NUL",
			},
		}
		actor := bytes32(0xCA)
		outcome, err := RenameMailbox(ctx, caller, actor[:], "Foo", "bad\x00name")
		if err != nil {
			t.Fatalf("RenameMailbox: %v", err)
		}
		if outcome.Kind != RenameMailboxOutcomeInvalidName {
			t.Errorf("kind: %q want invalid_name", outcome.Kind)
		}
		if outcome.Reason != "contains NUL" {
			t.Errorf("reason: %q", outcome.Reason)
		}
	})

	t.Run("RenameMailbox_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: renameMailboxReply{Outcome: "frobnicated"},
		}
		actor := bytes32(0xCB)
		_, err := RenameMailbox(ctx, caller, actor[:], "X", "Y")
		if err == nil || !strings.Contains(err.Error(), "unknown outcome") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("RenameMailbox_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-rename-fail")}
		actor := bytes32(0xCC)
		_, err := RenameMailbox(ctx, caller, actor[:], "X", "Y")
		if err == nil || !strings.Contains(err.Error(), "synthetic-rename-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── Phase E.3 — put_event_ciphertext / delete_event ─────────────

	t.Run("PutEventCiphertext_CreatedRoundTrip", func(t *testing.T) {
		eventID := bytes32(0xE1)
		caller := &recordingCaller{
			replyBody: putEventCiphertextReply{
				Outcome: string(PutEventCreated),
				EventID: eventID[:],
				ETag:    "etag-1",
				Modseq:  42,
			},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		uidHash := bytes32(0xCC)
		body := []byte("CIPHERBODY")
		hint := []byte("CIPHERHINT")
		got, err := PutEventCiphertext(ctx, caller, actor[:], cal[:], uidHash[:], body, hint, 1700000000, uint32(len(body)), nil)
		if err != nil {
			t.Fatalf("PutEventCiphertext: %v", err)
		}
		if caller.gotMethod != MethodPutEventCiphertext {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodPutEventCiphertext)
		}
		var body2 struct {
			ActorID            []byte  `cbor:"actor_id"`
			CalendarID         []byte  `cbor:"calendar_id"`
			UIDHash            []byte  `cbor:"uid_hash"`
			EncryptedBody      []byte  `cbor:"encrypted_body"`
			EncryptedIndexHint []byte  `cbor:"encrypted_index_hint"`
			Timestamp          int64   `cbor:"timestamp"`
			CiphertextSize     uint32  `cbor:"ciphertext_size"`
			IfMatch            *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.CalendarID, cal[:]) || !bytesEq(body2.UIDHash, uidHash[:]) {
			t.Errorf("ids: got actor=%x cal=%x uid=%x", body2.ActorID, body2.CalendarID, body2.UIDHash)
		}
		if !bytesEq(body2.EncryptedBody, body) || !bytesEq(body2.EncryptedIndexHint, hint) {
			t.Errorf("payloads: got body=%x hint=%x", body2.EncryptedBody, body2.EncryptedIndexHint)
		}
		if body2.Timestamp != 1700000000 || body2.CiphertextSize != uint32(len(body)) {
			t.Errorf("meta: got ts=%d size=%d", body2.Timestamp, body2.CiphertextSize)
		}
		if body2.IfMatch != nil {
			t.Errorf("if_match: got %q, want absent (nil)", *body2.IfMatch)
		}
		if got.Outcome != PutEventCreated {
			t.Errorf("outcome: got %q, want %q", got.Outcome, PutEventCreated)
		}
		if !bytesEq(got.EventID, eventID[:]) || got.ETag != "etag-1" || got.Modseq != 42 {
			t.Errorf("result: got %+v", got)
		}
	})

	t.Run("PutEventCiphertext_IfMatchEncoded", func(t *testing.T) {
		eventID := bytes32(0xE2)
		caller := &recordingCaller{
			replyBody: putEventCiphertextReply{
				Outcome: string(PutEventUpdated),
				EventID: eventID[:],
				ETag:    "etag-2",
				Modseq:  43,
			},
		}
		etag := "previous-etag"
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		uidHash := bytes32(0xCC)
		got, err := PutEventCiphertext(ctx, caller, actor[:], cal[:], uidHash[:], []byte("b"), []byte("h"), 1, 1, &etag)
		if err != nil {
			t.Fatalf("PutEventCiphertext: %v", err)
		}
		var body2 struct {
			IfMatch *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.IfMatch == nil || *body2.IfMatch != etag {
			t.Errorf("if_match: got %v, want &%q", body2.IfMatch, etag)
		}
		if got.Outcome != PutEventUpdated {
			t.Errorf("outcome: got %q, want updated", got.Outcome)
		}
	})

	t.Run("PutEventCiphertext_PreconditionFailedCarriesCurrentETag", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: putEventCiphertextReply{
				Outcome:     string(PutEventPreconditionFailed),
				CurrentETag: "server-etag",
			},
		}
		actor := bytes32(0xAA)
		got, err := PutEventCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err != nil {
			t.Fatalf("PutEventCiphertext: %v", err)
		}
		if got.Outcome != PutEventPreconditionFailed {
			t.Errorf("outcome: got %q, want precondition_failed", got.Outcome)
		}
		if got.CurrentETag != "server-etag" {
			t.Errorf("current_etag: got %q, want server-etag", got.CurrentETag)
		}
		if got.EventID != nil || got.ETag != "" || got.Modseq != 0 {
			t.Errorf("precondition_failed must zero out success-only fields: got %+v", got)
		}
	})

	t.Run("PutEventCiphertext_CalendarNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: putEventCiphertextReply{Outcome: string(PutEventCalendarNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := PutEventCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err != nil {
			t.Fatalf("PutEventCiphertext: %v", err)
		}
		if got.Outcome != PutEventCalendarNotFound {
			t.Errorf("outcome: got %q, want calendar_not_found", got.Outcome)
		}
	})

	t.Run("PutEventCiphertext_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: putEventCiphertextReply{Outcome: "future-shape"},
		}
		actor := bytes32(0xAA)
		_, err := PutEventCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("PutEventCiphertext_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-put-fail")}
		actor := bytes32(0xAA)
		_, err := PutEventCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err == nil || !strings.Contains(err.Error(), "synthetic-put-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("DeleteEvent_DeletedRoundTrip", func(t *testing.T) {
		eventID := bytes32(0xE3)
		caller := &recordingCaller{
			replyBody: deleteEventReply{
				Outcome: string(DeleteEventDeleted),
				EventID: eventID[:],
				Modseq:  44,
			},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		uidHash := bytes32(0xCC)
		got, err := DeleteEvent(ctx, caller, actor[:], cal[:], uidHash[:], nil)
		if err != nil {
			t.Fatalf("DeleteEvent: %v", err)
		}
		if caller.gotMethod != MethodDeleteEvent {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body2 struct {
			ActorID    []byte  `cbor:"actor_id"`
			CalendarID []byte  `cbor:"calendar_id"`
			UIDHash    []byte  `cbor:"uid_hash"`
			IfMatch    *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.CalendarID, cal[:]) || !bytesEq(body2.UIDHash, uidHash[:]) {
			t.Errorf("ids: got actor=%x cal=%x uid=%x", body2.ActorID, body2.CalendarID, body2.UIDHash)
		}
		if body2.IfMatch != nil {
			t.Errorf("if_match: got %q, want absent", *body2.IfMatch)
		}
		if got.Outcome != DeleteEventDeleted {
			t.Errorf("outcome: got %q, want deleted", got.Outcome)
		}
		if !bytesEq(got.EventID, eventID[:]) || got.Modseq != 44 {
			t.Errorf("result: got %+v", got)
		}
	})

	t.Run("DeleteEvent_IfMatchEncoded", func(t *testing.T) {
		eventID := bytes32(0xE4)
		caller := &recordingCaller{
			replyBody: deleteEventReply{Outcome: string(DeleteEventDeleted), EventID: eventID[:], Modseq: 45},
		}
		etag := "del-etag"
		actor := bytes32(0xAA)
		_, err := DeleteEvent(ctx, caller, actor[:], actor[:], actor[:], &etag)
		if err != nil {
			t.Fatalf("DeleteEvent: %v", err)
		}
		var body2 struct {
			IfMatch *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.IfMatch == nil || *body2.IfMatch != etag {
			t.Errorf("if_match: got %v, want &%q", body2.IfMatch, etag)
		}
	})

	t.Run("DeleteEvent_NotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteEventReply{Outcome: string(DeleteEventNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := DeleteEvent(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err != nil {
			t.Fatalf("DeleteEvent: %v", err)
		}
		if got.Outcome != DeleteEventNotFound {
			t.Errorf("outcome: got %q, want not_found", got.Outcome)
		}
	})

	t.Run("DeleteEvent_PreconditionFailedCarriesCurrentETag", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteEventReply{
				Outcome:     string(DeleteEventPreconditionFailed),
				CurrentETag: "server-etag",
			},
		}
		actor := bytes32(0xAA)
		got, err := DeleteEvent(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err != nil {
			t.Fatalf("DeleteEvent: %v", err)
		}
		if got.Outcome != DeleteEventPreconditionFailed {
			t.Errorf("outcome: got %q, want precondition_failed", got.Outcome)
		}
		if got.CurrentETag != "server-etag" {
			t.Errorf("current_etag: got %q", got.CurrentETag)
		}
	})

	t.Run("DeleteEvent_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteEventReply{Outcome: "future-shape"},
		}
		actor := bytes32(0xAA)
		_, err := DeleteEvent(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("DeleteEvent_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-delete-fail")}
		actor := bytes32(0xAA)
		_, err := DeleteEvent(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err == nil || !strings.Contains(err.Error(), "synthetic-delete-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── Phase E.3 — query_events / sync_calendar_since (REPORT) ─────

	t.Run("QueryEvents_OkRoundTrip", func(t *testing.T) {
		eventID := bytes32(0xE5)
		uidHash := bytes32(0xE6)
		caller := &recordingCaller{
			replyBody: queryEventsReply{
				Outcome: string(QueryEventsOk),
				Events: []EventEntry{
					{
						EventID:            eventID[:],
						UIDHash:            uidHash[:],
						EncryptedBody:      []byte("CIPHERBODY"),
						EncryptedIndexHint: []byte("CIPHERHINT"),
						ETag:               "etag-q1",
						Modseq:             100,
						CiphertextSize:     10,
						InternalDate:       1700000000,
					},
				},
				HighestModseq: 100,
				More:          false,
			},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		got, err := QueryEvents(ctx, caller, actor[:], cal[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryEvents: %v", err)
		}
		if caller.gotMethod != MethodQueryEvents {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodQueryEvents)
		}
		var body2 struct {
			ActorID      []byte `cbor:"actor_id"`
			CalendarID   []byte `cbor:"calendar_id"`
			SinceModseq  *int64 `cbor:"since_modseq"`
			AfterEventID []byte `cbor:"after_event_id"`
			Limit        uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.CalendarID, cal[:]) {
			t.Errorf("ids: got actor=%x cal=%x", body2.ActorID, body2.CalendarID)
		}
		if body2.SinceModseq != nil {
			t.Errorf("since_modseq: got %v, want absent (nil)", *body2.SinceModseq)
		}
		if body2.AfterEventID != nil {
			t.Errorf("after_event_id: got %x, want absent (nil)", body2.AfterEventID)
		}
		if body2.Limit != 0 {
			t.Errorf("limit: got %d, want 0", body2.Limit)
		}
		if got.Outcome != QueryEventsOk {
			t.Errorf("outcome: got %q, want ok", got.Outcome)
		}
		if len(got.Events) != 1 || !bytesEq(got.Events[0].EventID, eventID[:]) || got.Events[0].ETag != "etag-q1" {
			t.Errorf("events: got %+v", got.Events)
		}
		if got.HighestModseq != 100 || got.More {
			t.Errorf("highestmodseq/more: got %d/%v", got.HighestModseq, got.More)
		}
	})

	t.Run("QueryEvents_SincePresentResumeCursor", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: queryEventsReply{
				Outcome: string(QueryEventsOk),
				Events:  []EventEntry{},
				More:    true,
			},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		after := bytes32(0xCC)
		since := int64(42)
		_, err := QueryEvents(ctx, caller, actor[:], cal[:], &since, after[:], 100)
		if err != nil {
			t.Fatalf("QueryEvents: %v", err)
		}
		var body2 struct {
			SinceModseq  *int64 `cbor:"since_modseq"`
			AfterEventID []byte `cbor:"after_event_id"`
			Limit        uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.SinceModseq == nil || *body2.SinceModseq != 42 {
			t.Errorf("since_modseq: got %v, want &42", body2.SinceModseq)
		}
		if !bytesEq(body2.AfterEventID, after[:]) {
			t.Errorf("after_event_id: got %x, want %x", body2.AfterEventID, after[:])
		}
		if body2.Limit != 100 {
			t.Errorf("limit: got %d, want 100", body2.Limit)
		}
	})

	t.Run("QueryEvents_CalendarNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: queryEventsReply{Outcome: string(QueryEventsCalendarNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := QueryEvents(ctx, caller, actor[:], actor[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryEvents: %v", err)
		}
		if got.Outcome != QueryEventsCalendarNotFound {
			t.Errorf("outcome: got %q, want calendar_not_found", got.Outcome)
		}
		if len(got.Events) != 0 || got.HighestModseq != 0 {
			t.Errorf("not-found should have zero payload: %+v", got)
		}
	})

	t.Run("QueryEvents_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: queryEventsReply{Outcome: "future-shape"},
		}
		actor := bytes32(0xAA)
		_, err := QueryEvents(ctx, caller, actor[:], actor[:], nil, nil, 0)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("QueryEvents_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-query-fail")}
		actor := bytes32(0xAA)
		_, err := QueryEvents(ctx, caller, actor[:], actor[:], nil, nil, 0)
		if err == nil || !strings.Contains(err.Error(), "synthetic-query-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("SyncCalendarSince_OkRoundTrip", func(t *testing.T) {
		changedID := bytes32(0xE7)
		expungedID := bytes32(0xE8)
		caller := &recordingCaller{
			replyBody: syncCalendarSinceReply{
				Outcome: string(SyncCalendarSinceOk),
				Changed: []EventEntry{
					{EventID: changedID[:], ETag: "etag-c1", Modseq: 50},
				},
				Expunged: []ExpungedEntry{
					{EventID: expungedID[:], Modseq: 51},
				},
				NewSyncToken: "51",
				More:         false,
			},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		got, err := SyncCalendarSince(ctx, caller, actor[:], cal[:], "0", 0, "")
		if err != nil {
			t.Fatalf("SyncCalendarSince: %v", err)
		}
		if caller.gotMethod != MethodSyncCalendarSince {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodSyncCalendarSince)
		}
		var body2 struct {
			ActorID    []byte `cbor:"actor_id"`
			CalendarID []byte `cbor:"calendar_id"`
			SyncToken  string `cbor:"sync_token"`
			Limit      uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.SyncToken != "0" {
			t.Errorf("sync_token: got %q, want %q", body2.SyncToken, "0")
		}
		if got.Outcome != SyncCalendarSinceOk {
			t.Errorf("outcome: got %q, want ok", got.Outcome)
		}
		if len(got.Changed) != 1 || got.Changed[0].ETag != "etag-c1" {
			t.Errorf("changed: got %+v", got.Changed)
		}
		if len(got.Expunged) != 1 || got.Expunged[0].Modseq != 51 {
			t.Errorf("expunged: got %+v", got.Expunged)
		}
		if got.NewSyncToken != "51" || got.More {
			t.Errorf("token/more: got %q/%v", got.NewSyncToken, got.More)
		}
	})

	t.Run("SyncCalendarSince_EmptyTokenNormalizesToZero", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: syncCalendarSinceReply{
				Outcome:      string(SyncCalendarSinceOk),
				NewSyncToken: "100",
			},
		}
		actor := bytes32(0xAA)
		_, err := SyncCalendarSince(ctx, caller, actor[:], actor[:], "", 0, "")
		if err != nil {
			t.Fatalf("SyncCalendarSince: %v", err)
		}
		var body2 struct {
			SyncToken string `cbor:"sync_token"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.SyncToken != "0" {
			t.Errorf("empty token MUST normalize to %q on the wire; got %q", "0", body2.SyncToken)
		}
	})

	t.Run("SyncCalendarSince_CalendarNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: syncCalendarSinceReply{Outcome: string(SyncCalendarSinceCalendarNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := SyncCalendarSince(ctx, caller, actor[:], actor[:], "0", 0, "")
		if err != nil {
			t.Fatalf("SyncCalendarSince: %v", err)
		}
		if got.Outcome != SyncCalendarSinceCalendarNotFound {
			t.Errorf("outcome: got %q, want calendar_not_found", got.Outcome)
		}
	})

	t.Run("SyncCalendarSince_StaleForwardsServerModseqAndMuaID", func(t *testing.T) {
		// Nest returns Stale when the client's sync_token is ahead of the
		// calendar's highestmodseq (post-DR-restore case, spec § D6 (γ)).
		// The wrapper must surface Stale + ServerModseq and forward mua_id
		// on the wire so nest can log it for the divergence row.
		caller := &recordingCaller{
			replyBody: syncCalendarSinceReply{
				Outcome:      string(SyncCalendarSinceStale),
				ServerModseq: 7,
			},
		}
		actor := bytes32(0xAA)
		got, err := SyncCalendarSince(ctx, caller, actor[:], actor[:], "99", 0, "Apple Calendar/14.0")
		if err != nil {
			t.Fatalf("SyncCalendarSince: %v", err)
		}
		if got.Outcome != SyncCalendarSinceStale {
			t.Errorf("outcome: got %q, want stale", got.Outcome)
		}
		if got.ServerModseq != 7 {
			t.Errorf("ServerModseq: got %d, want 7", got.ServerModseq)
		}
		// mua_id must have been forwarded on the wire.
		var body struct {
			MuaID *string `cbor:"mua_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.MuaID == nil || *body.MuaID != "Apple Calendar/14.0" {
			t.Errorf("mua_id: got %v, want %q", body.MuaID, "Apple Calendar/14.0")
		}
	})

	t.Run("SyncCalendarSince_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: syncCalendarSinceReply{Outcome: "future-shape"},
		}
		actor := bytes32(0xAA)
		_, err := SyncCalendarSince(ctx, caller, actor[:], actor[:], "0", 0, "")
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("SyncCalendarSince_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-sync-fail")}
		actor := bytes32(0xAA)
		_, err := SyncCalendarSince(ctx, caller, actor[:], actor[:], "0", 0, "")
		if err == nil || !strings.Contains(err.Error(), "synthetic-sync-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── provision_calendar (Phase E.4 PROPPATCH path) ──────────────
	//
	// The wrapper now carries `update_metadata: bool` alongside the
	// MKCOL payload. `false` is the existing lazy-Personal + MKCOL path
	// (outcomes Created/AlreadyExists/Conflict). `true` is the PROPPATCH
	// path (outcomes Updated/NotFound). Both modes hit the same nest RPC
	// `fauna.bridges.provision_calendar`; the bool routes nest's handler
	// between insert and update.

	t.Run("ProvisionCalendar_MkcolCreatedRoundTrip", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: provisionCalendarReply{Outcome: string(ProvisionCalendarCreated)},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		meta := []byte("SEALED-METADATA")
		got, err := ProvisionCalendar(ctx, caller, actor[:], cal[:], meta, false)
		if err != nil {
			t.Fatalf("ProvisionCalendar: %v", err)
		}
		if caller.gotMethod != MethodProvisionCalendar {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodProvisionCalendar)
		}
		var body struct {
			ActorID           []byte `cbor:"actor_id"`
			CalendarID        []byte `cbor:"calendar_id"`
			EncryptedMetadata []byte `cbor:"encrypted_metadata"`
			UpdateMetadata    bool   `cbor:"update_metadata"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || !bytesEq(body.CalendarID, cal[:]) {
			t.Errorf("ids: got actor=%x cal=%x", body.ActorID, body.CalendarID)
		}
		if !bytesEq(body.EncryptedMetadata, meta) {
			t.Errorf("encrypted_metadata: got %x, want %x", body.EncryptedMetadata, meta)
		}
		if body.UpdateMetadata {
			t.Errorf("update_metadata: got true, want false on MKCOL path")
		}
		if got != ProvisionCalendarCreated {
			t.Errorf("outcome: got %q, want %q", got, ProvisionCalendarCreated)
		}
	})

	t.Run("ProvisionCalendar_PropPatchUpdatedRoundTrip", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: provisionCalendarReply{Outcome: string(ProvisionCalendarUpdated)},
		}
		actor := bytes32(0xAA)
		cal := bytes32(0xBB)
		meta := []byte("RESEALED-METADATA")
		got, err := ProvisionCalendar(ctx, caller, actor[:], cal[:], meta, true)
		if err != nil {
			t.Fatalf("ProvisionCalendar: %v", err)
		}
		var body struct {
			UpdateMetadata bool `cbor:"update_metadata"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !body.UpdateMetadata {
			t.Errorf("update_metadata: got false, want true on PROPPATCH path")
		}
		if got != ProvisionCalendarUpdated {
			t.Errorf("outcome: got %q, want %q", got, ProvisionCalendarUpdated)
		}
	})

	t.Run("ProvisionCalendar_PropPatchNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: provisionCalendarReply{Outcome: string(ProvisionCalendarNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := ProvisionCalendar(ctx, caller, actor[:], actor[:], []byte("meta"), true)
		if err != nil {
			t.Fatalf("ProvisionCalendar: %v", err)
		}
		if got != ProvisionCalendarNotFound {
			t.Errorf("outcome: got %q, want %q", got, ProvisionCalendarNotFound)
		}
	})

	t.Run("ProvisionCalendar_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: provisionCalendarReply{Outcome: "future-shape"},
		}
		actor := bytes32(0xAA)
		_, err := ProvisionCalendar(ctx, caller, actor[:], actor[:], []byte("meta"), false)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("ProvisionCalendar_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-provision-fail")}
		actor := bytes32(0xAA)
		_, err := ProvisionCalendar(ctx, caller, actor[:], actor[:], []byte("meta"), true)
		if err == nil || !strings.Contains(err.Error(), "synthetic-provision-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("FetchRecipientForwardConfig_SetRoundTrip", func(t *testing.T) {
		target := "bob@example.com"
		caller := &recordingCaller{
			replyBody: fetchRecipientForwardConfigReply{ForwardAllTo: &target},
		}
		actor := bytes32(0xC1)
		got, err := FetchRecipientForwardConfig(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchRecipientForwardConfig: %v", err)
		}
		if caller.gotMethod != MethodFetchRecipientForwardConfig {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: got %x, want %x", body.ActorID, actor[:])
		}
		if got != target {
			t.Errorf("forward_all_to: got %q, want %q", got, target)
		}
	})

	t.Run("FetchRecipientForwardConfig_DisabledReturnsEmpty", func(t *testing.T) {
		// forward_all_to absent / null (Option::None) → "" (disabled).
		caller := &recordingCaller{replyBody: fetchRecipientForwardConfigReply{ForwardAllTo: nil}}
		actor := bytes32(0xC2)
		got, err := FetchRecipientForwardConfig(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("FetchRecipientForwardConfig: %v", err)
		}
		if got != "" {
			t.Errorf("forward_all_to: got %q, want empty (disabled)", got)
		}
	})

	t.Run("ForwardMessage_BodyShapeAndReply", func(t *testing.T) {
		caller := &recordingCaller{replyBody: forwardMessageReply{ID: 4242}}
		actor := bytes32(0xF0)
		raw := []byte("X-Fauna-Forwarded-By: actor=...\r\nSubject: hi\r\n\r\nbody")
		id, err := ForwardMessage(
			ctx, caller, actor[:], "<msg-1@src>", "alice@example.org",
			"bob@example.com", raw, "forward-all", ForwardCopyModeCopy,
		)
		if err != nil {
			t.Fatalf("ForwardMessage: %v", err)
		}
		if caller.gotMethod != MethodForwardMessage {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body struct {
			ActorID            []byte `cbor:"actor_id"`
			OriginalMsgID      string `cbor:"original_msgid"`
			OriginalSender     string `cbor:"original_sender"`
			Destination        string `cbor:"destination"`
			RawMessage         []byte `cbor:"raw_message"`
			RuleIDOrForwardAll string `cbor:"rule_id_or_forward_all"`
			CopyMode           string `cbor:"copy_mode"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: got %x, want %x", body.ActorID, actor[:])
		}
		if body.OriginalMsgID != "<msg-1@src>" || body.OriginalSender != "alice@example.org" ||
			body.Destination != "bob@example.com" || body.RuleIDOrForwardAll != "forward-all" {
			t.Errorf("string fields: got %+v", body)
		}
		if !bytesEq(body.RawMessage, raw) {
			t.Errorf("raw_message mismatch")
		}
		// ForwardCopyMode serde-renames to snake_case; Copy → "copy".
		if body.CopyMode != "copy" {
			t.Errorf("copy_mode: got %q, want \"copy\"", body.CopyMode)
		}
		if id != 4242 {
			t.Errorf("reply id: got %d, want 4242", id)
		}
	})

	t.Run("ForwardMessage_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-forward-fail")}
		actor := bytes32(0xF1)
		_, err := ForwardMessage(
			ctx, caller, actor[:], "<m@s>", "a@b.org", "c@d.com",
			[]byte("x"), "forward-all", ForwardCopyModeCopy,
		)
		if err == nil || !strings.Contains(err.Error(), "synthetic-forward-fail") {
			t.Fatalf("err: %v", err)
		}
	})
}

// TestWhoamiAgainstWSServer exercises the full Whoami path against a
// httptest WebSocket server that decodes a real RequestFrame, decodes
// the empty whoami body, and writes back a synthetic ReplyFrame with
// an encoded WhoamiReply. This is the integration counterpart to the
// fake-Caller subtest above: it proves the wrapper composes correctly
// with the real *Client (env encode + reply decode), not just with
// the recordingCaller seam.
func TestWhoamiAgainstWSServer(t *testing.T) {
	type frame = []byte
	expected := WhoamiReply{
		Role:             "mta",
		BridgeID:         "mta-int-1",
		Status:           "approved",
		Ed25519PubkeyHex: strings.Repeat("ee", 32),
		X25519PubkeyHex:  strings.Repeat("ff", 32),
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/api/v1/auth/challenge", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"nonce":      hex.EncodeToString(make([]byte, 32)),
			"expires_in": 300,
			"expires_at": time.Now().Unix() + 300,
		})
	})
	mux.HandleFunc("/api/v1/auth/verify", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"token":      "tok",
			"expires_in": 3600,
			"expires_at": time.Now().Unix() + 3600,
		})
	})
	mux.HandleFunc("/api/v1/ws/", func(w http.ResponseWriter, r *http.Request) {
		proto := r.Header.Get("Sec-WebSocket-Protocol")
		if !strings.Contains(proto, "fauna.v1") || !strings.Contains(proto, "bearer.") {
			http.Error(w, "subprotocol", http.StatusUnauthorized)
			return
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			Subprotocols: []string{"fauna.v1"},
		})
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		conn.SetReadLimit(16 << 20)
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_, data, err := conn.Read(ctx)
		if err != nil {
			return
		}
		f, err := DecodeFrame(data)
		if err != nil {
			return
		}
		req, ok := f.(*RequestFrame)
		if !ok || req.Kind != MethodWhoami {
			return
		}
		// Verify the request body is an empty CBOR map.
		var body map[string]any
		if err := cbor.Unmarshal(req.Payload, &body); err != nil || len(body) != 0 {
			return
		}
		var rep frame
		rep, err = EncodeReplyForTest(req.CorrelationID, expected, true)
		if err != nil {
			return
		}
		wctx, wcancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer wcancel()
		_ = conn.Write(wctx, websocket.MessageBinary, rep)
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	pub, priv := newTestKey(t)
	auth := NewAuthClient(http.DefaultClient, srv.URL, pub, priv)
	auth.acquireToken = staticAcquire("mock-bearer-token")
	c, err := Dial(ctx, ClientConfig{
		NestEndpoint: srv.URL,
		AuthClient:   auth,
	})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()

	got, err := Whoami(ctx, c)
	if err != nil {
		t.Fatalf("Whoami: %v", err)
	}
	if got != expected {
		t.Errorf("Whoami reply: got %+v, want %+v", got, expected)
	}
}

// bytesEq compares two byte slices for equality. Avoids pulling in
// bytes.Equal so the test file's import list stays tight.
func bytesEq(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// bytes32 makes a 32-byte array filled with the given byte.
func bytes32(v byte) [32]byte {
	var a [32]byte
	for i := range a {
		a[i] = v
	}
	return a
}

// TestNilContainersEncodeAsEmptyNotNull pins the systemic wire-correctness
// contract generalized from the UID SEARCH ALL fix into the
// codec-level NilContainersAsEmpty flip (internal/dagcbor/codec.go): a nil
// Go slice/[]byte that the bridge marshals for a *required* Rust Vec /
// Vec<u8> field MUST hit the wire as the canonical empty container — `[]`
// (0x80) for a list — never CBOR null (0xf6), because strict dag-cbor decode
// on nest rejects null for a list (the UID SEARCH ALL / plain-EXPUNGE
// failure mode). Conversely an Option<ByteBuf> request cursor (None ≠ empty)
// MUST stay null (0xf6), never an empty byte string (0x40 = Some(empty));
// such fields are Go pointers so a nil pointer still encodes null regardless
// of NilContainersAsEmpty. See docs/goal/architecture/serialization.md.
func TestNilContainersEncodeAsEmptyNotNull(t *testing.T) {
	ctx := context.Background()

	t.Run("StoreFlags_NilFlagsEncodesEmptyArrayNotNull", func(t *testing.T) {
		// IMAP `STORE FLAGS ()` clears every flag and reaches StoreFlags with
		// an empty flag set. Rust StoreFlagsRequest.flags: Vec<String>
		// strict-decodes and rejects null.
		caller := &recordingCaller{replyBody: StoreFlagsReply{HighestModSeq: 1}}
		actor := bytes32(0x51)
		_, err := StoreFlags(ctx, caller, StoreFlagsParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
			UIDs:    []uint32{7},
			Op:      StoreFlagsOpSet,
			Flags:   nil, // STORE FLAGS () — clear all flags
		})
		if err != nil {
			t.Fatalf("StoreFlags: %v", err)
		}
		assertWireFieldFirstByte(t, caller.gotBody, "flags", 0x80) // empty array, not 0xf6 null
	})

	t.Run("Expunge_NilUIDsEncodesEmptyArrayNotNull", func(t *testing.T) {
		// Plain EXPUNGE (no UID set) reaches Expunge with nil UIDs (see the
		// ExpungeParams doc). Rust ExpungeRequest.uids: Vec<u32> rejects null.
		caller := &recordingCaller{replyBody: ExpungeReply{}}
		actor := bytes32(0x52)
		_, err := Expunge(ctx, caller, ExpungeParams{
			ActorID: actor[:],
			Mailbox: "INBOX",
			UIDs:    nil, // plain EXPUNGE
		})
		if err != nil {
			t.Fatalf("Expunge: %v", err)
		}
		assertWireFieldFirstByte(t, caller.gotBody, "uids", 0x80) // empty array, not 0xf6 null
	})

	t.Run("QueryEvents_NilAfterEventIDStaysNullNotEmptyBytes", func(t *testing.T) {
		// after_event_id is Option<ByteBuf>: None = "start a fresh page", which
		// MUST encode as null (0xf6). Under NilContainersAsEmpty a bare []byte
		// would wrongly become an empty byte string (0x40 = Some(empty)); the
		// struct field is *[]byte so a nil pointer still encodes null.
		caller := &recordingCaller{replyBody: queryEventsReply{Outcome: string(QueryEventsOk)}}
		actor := bytes32(0xA1)
		cal := bytes32(0xB1)
		_, err := QueryEvents(ctx, caller, actor[:], cal[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryEvents: %v", err)
		}
		assertWireFieldFirstByte(t, caller.gotBody, "after_event_id", 0xf6) // null, not 0x40 empty bytes
	})
}

func TestResolveRecipient(t *testing.T) {
	ctx := context.Background()
	actor := bytes.Repeat([]byte{0x7a}, 32)
	admin := bytes.Repeat([]byte{0x09}, 32)

	t.Run("resolved_with_headers_controls_and_role_flag", func(t *testing.T) {
		rate := int64(100)
		caller := &recordingCaller{
			// Wire shape: serde tag="outcome" flat map (snake_case keys).
			replyBody: map[string]any{
				"outcome":  "resolved",
				"actor_id": actor,
				"headers_to_stamp": []map[string]any{
					{"name": "X-Fauna-Address-Catchall", "value": "true"},
				},
				"control_overrides": map[string]any{"rate_limit_per_hour": rate},
				"is_role_address":   true,
			},
		}
		got, err := ResolveRecipient(ctx, caller, "postmaster", "example.com", "sender.test", "seller@amazon.com")
		if err != nil {
			t.Fatalf("ResolveRecipient: %v", err)
		}
		if caller.gotMethod != MethodResolveRecipient {
			t.Errorf("method: got %q, want %q", caller.gotMethod, MethodResolveRecipient)
		}
		// Request carries local_part/domain/sender_domain/sender_address.
		var body struct {
			LocalPart     string `cbor:"local_part"`
			Domain        string `cbor:"domain"`
			SenderDomain  string `cbor:"sender_domain"`
			SenderAddress string `cbor:"sender_address"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		// The full envelope sender must cross the wire — the nest's guardian
		// mail gate keys on the whole address, not just its domain.
		if body.SenderAddress != "seller@amazon.com" {
			t.Errorf("sender_address: got %q, want %q", body.SenderAddress, "seller@amazon.com")
		}
		if body.LocalPart != "postmaster" || body.Domain != "example.com" || body.SenderDomain != "sender.test" {
			t.Errorf("body: got %+v", body)
		}
		if got.Outcome != ResolveResolved {
			t.Fatalf("outcome: got %q", got.Outcome)
		}
		if !bytes.Equal(got.ActorID, actor) {
			t.Errorf("actor_id: got %x, want %x", got.ActorID, actor)
		}
		if !got.IsRoleAddress {
			t.Errorf("is_role_address should be true")
		}
		if len(got.HeadersToStamp) != 1 || got.HeadersToStamp[0].Name != "X-Fauna-Address-Catchall" {
			t.Errorf("headers_to_stamp: got %+v", got.HeadersToStamp)
		}
		if got.ControlOverrides.RateLimitPerHour == nil || *got.ControlOverrides.RateLimitPerHour != 100 {
			t.Errorf("control_overrides.rate_limit_per_hour: got %+v", got.ControlOverrides.RateLimitPerHour)
		}
	})

	t.Run("forward", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":            "forward",
				"forward_target":     "oldaccount@example.com",
				"forwarder_actor_id": admin,
			},
		}
		got, err := ResolveRecipient(ctx, caller, "info", "example.com", "", "")
		if err != nil {
			t.Fatalf("ResolveRecipient: %v", err)
		}
		if got.Outcome != ResolveForward {
			t.Fatalf("outcome: got %q", got.Outcome)
		}
		if got.ForwardTarget != "oldaccount@example.com" || !bytes.Equal(got.ForwarderActorID, admin) {
			t.Errorf("forward fields: got target=%q forwarder=%x", got.ForwardTarget, got.ForwarderActorID)
		}
	})

	t.Run("reject_is_a_normal_result_not_an_error", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{
				"outcome":   "reject",
				"smtp_code": uint16(550),
				"reason":    "User unknown",
			},
		}
		got, err := ResolveRecipient(ctx, caller, "nobody", "example.com", "", "")
		if err != nil {
			t.Fatalf("reject must not be an error: %v", err)
		}
		if got.Outcome != ResolveReject || got.SMTPCode != 550 || got.Reason != "User unknown" {
			t.Errorf("reject: got %+v", got)
		}
	})

	t.Run("forward_missing_target_is_an_error", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: map[string]any{"outcome": "forward", "forwarder_actor_id": admin},
		}
		if _, err := ResolveRecipient(ctx, caller, "info", "example.com", "", ""); err == nil {
			t.Fatalf("expected error for forward with empty target")
		}
	})

	t.Run("unknown_outcome_is_an_error", func(t *testing.T) {
		caller := &recordingCaller{replyBody: map[string]any{"outcome": "teapot"}}
		if _, err := ResolveRecipient(ctx, caller, "x", "example.com", "", ""); err == nil {
			t.Fatalf("expected error for unknown outcome")
		}
	})
}

// assertWireFieldFirstByte decodes body as a string-keyed CBOR map and
// asserts the named field is present and its first encoded byte equals want
// (0x80 empty array / 0xf6 null / 0x40 empty byte string).
func assertWireFieldFirstByte(t *testing.T, body []byte, field string, want byte) {
	t.Helper()
	var raw map[string]cbor.RawMessage
	if err := cbor.Unmarshal(body, &raw); err != nil {
		t.Fatalf("decode body map: %v", err)
	}
	got, ok := raw[field]
	if !ok {
		t.Fatalf("body missing %q key; got % x", field, body)
	}
	if len(got) != 1 || got[0] != want {
		t.Errorf("%s: want first byte 0x%02x, got % x", field, want, []byte(got))
	}
}

// assertWireFieldAbsent fails unless `field` is OMITTED from the encoded body
// map — the `omitempty` contract for a nil-pointer Option<…> (a missing key
// decodes to None on the Rust side, where a present null/empty would be the
// Some(empty) footgun).
func assertWireFieldAbsent(t *testing.T, body []byte, field string) {
	t.Helper()
	var raw map[string]cbor.RawMessage
	if err := cbor.Unmarshal(body, &raw); err != nil {
		t.Fatalf("decode body map: %v", err)
	}
	if _, ok := raw[field]; ok {
		t.Errorf("%s: want absent, but present in body % x", field, body)
	}
}

// TestAutoScheduleMailboxLessWrappers covers the three wrappers the MDA's
// server-side auto-schedule mailbox-less rail uses (caldav-server.md
// § Server-side auto-schedule, C4): the BridgeMda-only delivery RPC plus the
// two reads (actor.by_handle, keypackage.fetch) the gateway classifies an
// attendee with.
func TestAutoScheduleMailboxLessWrappers(t *testing.T) {
	ctx := context.Background()

	t.Run("DeliverSealedScheduling_sameNest", func(t *testing.T) {
		onBehalf := bytes.Repeat([]byte{0x11}, 32)
		welcome := bytes.Repeat([]byte{0x22}, 48)
		appEnv := bytes.Repeat([]byte{0x33}, 64)
		caller := &recordingCaller{
			replyBody: deliverSealedSchedulingReply{InboxID: 7, Seq: 3},
		}
		inboxID, seq, err := DeliverSealedScheduling(
			ctx, caller, onBehalf, "alice@fauna.test", strings.Repeat("ab", 32),
			"", // same-nest ⇒ peer_domain omitted
			strings.Repeat("cd", 32), welcome, appEnv,
		)
		if err != nil {
			t.Fatalf("DeliverSealedScheduling: %v", err)
		}
		if caller.gotMethod != MethodDeliverSealedScheduling {
			t.Errorf("method: got %q, want %q", caller.gotMethod, MethodDeliverSealedScheduling)
		}
		// peer_domain must be ABSENT (nil *string + omitempty) so nest's
		// Option<String> decodes to None ⇒ same-nest.
		assertWireFieldAbsent(t, caller.gotBody, "peer_domain")
		// The opaque sealed fields ride as CBOR byte-strings (serde_bytes) — the
		// round-trip below proves it (decoding a CBOR int-array into []byte fails).
		var body struct {
			OnBehalfOfActor  []byte `cbor:"on_behalf_of_actor"`
			OriginalSender   string `cbor:"original_sender"`
			RecipientActorID string `cbor:"recipient_actor_id"`
			ChannelID        string `cbor:"channel_id"`
			WelcomeBytes     []byte `cbor:"welcome_bytes"`
			AppEnvelope      []byte `cbor:"app_envelope"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytes.Equal(body.OnBehalfOfActor, onBehalf) || !bytes.Equal(body.WelcomeBytes, welcome) || !bytes.Equal(body.AppEnvelope, appEnv) {
			t.Errorf("byte fields round-trip mismatch")
		}
		if body.OriginalSender != "alice@fauna.test" || body.RecipientActorID != strings.Repeat("ab", 32) || body.ChannelID != strings.Repeat("cd", 32) {
			t.Errorf("string fields: got %+v", body)
		}
		if inboxID != 7 || seq != 3 {
			t.Errorf("reply unpack: got inbox_id=%d seq=%d, want 7/3", inboxID, seq)
		}
	})

	t.Run("DeliverSealedScheduling_crossNest", func(t *testing.T) {
		caller := &recordingCaller{replyBody: deliverSealedSchedulingReply{InboxID: 0, Seq: 1}}
		_, _, err := DeliverSealedScheduling(
			ctx, caller, bytes.Repeat([]byte{0x11}, 32), "alice@fauna.test",
			strings.Repeat("ab", 32), "peer.example", strings.Repeat("cd", 32),
			[]byte{0x01}, []byte{0x02},
		)
		if err != nil {
			t.Fatalf("DeliverSealedScheduling: %v", err)
		}
		var body struct {
			PeerDomain string `cbor:"peer_domain"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.PeerDomain != "peer.example" {
			t.Errorf("peer_domain: got %q, want peer.example", body.PeerDomain)
		}
	})

	t.Run("ActorByHandle_found", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: actorByHandleReply{
				ActorID:     strings.Repeat("ef", 32),
				Domain:      "fauna.test",
				Addressable: true,
			},
		}
		got, err := ActorByHandle(ctx, caller, "alice")
		if err != nil {
			t.Fatalf("ActorByHandle: %v", err)
		}
		if caller.gotMethod != MethodActorByHandle {
			t.Errorf("method: got %q, want %q", caller.gotMethod, MethodActorByHandle)
		}
		var body struct {
			Handle string `cbor:"handle"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.Handle != "alice" {
			t.Errorf("handle: got %q, want alice", body.Handle)
		}
		if got.ActorID != strings.Repeat("ef", 32) || got.Domain != "fauna.test" || !got.Addressable {
			t.Errorf("reply unpack: got %+v", got)
		}
	})

	t.Run("ActorByHandle_notFound", func(t *testing.T) {
		payload, err := cbor.Marshal(struct {
			Code string `cbor:"code"`
		}{Code: "fauna.actor.not_found"})
		if err != nil {
			t.Fatalf("marshal err payload: %v", err)
		}
		caller := &recordingCaller{returnErr: &ServerError{Payload: payload}}
		_, err = ActorByHandle(ctx, caller, "ghost")
		if !errors.Is(err, ErrActorNotFound) {
			t.Errorf("want ErrActorNotFound, got %v", err)
		}
	})

	t.Run("ActorByHandle_transportError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("connection reset")}
		_, err := ActorByHandle(ctx, caller, "alice")
		if err == nil || errors.Is(err, ErrActorNotFound) {
			t.Errorf("want a non-notfound transport error, got %v", err)
		}
	})

	t.Run("KeypackageFetch_present", func(t *testing.T) {
		kp := bytes.Repeat([]byte{0x55}, 96)
		caller := &recordingCaller{replyBody: keypackageFetchReply{KeyPackage: &kp}}
		got, err := KeypackageFetch(ctx, caller, strings.Repeat("ab", 32), "")
		if err != nil {
			t.Fatalf("KeypackageFetch: %v", err)
		}
		if caller.gotMethod != MethodKeypackageFetch {
			t.Errorf("method: got %q, want %q", caller.gotMethod, MethodKeypackageFetch)
		}
		// same-nest ⇒ nest_url omitted.
		assertWireFieldAbsent(t, caller.gotBody, "nest_url")
		var body struct {
			ActorID string `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.ActorID != strings.Repeat("ab", 32) {
			t.Errorf("actor_id: got %q", body.ActorID)
		}
		if !bytes.Equal(got, kp) {
			t.Errorf("key package round-trip mismatch")
		}
	})

	t.Run("KeypackageFetch_absent", func(t *testing.T) {
		// key_package None ⇒ (nil, nil): no KP available, the classifier falls
		// back to the email rail.
		caller := &recordingCaller{replyBody: keypackageFetchReply{KeyPackage: nil}}
		got, err := KeypackageFetch(ctx, caller, strings.Repeat("ab", 32), "")
		if err != nil {
			t.Fatalf("KeypackageFetch: %v", err)
		}
		if got != nil {
			t.Errorf("want nil key package, got % x", got)
		}
	})

	t.Run("KeypackageFetch_crossNest", func(t *testing.T) {
		kp := []byte{0x01}
		caller := &recordingCaller{replyBody: keypackageFetchReply{KeyPackage: &kp}}
		if _, err := KeypackageFetch(ctx, caller, strings.Repeat("ab", 32), "https://peer.example"); err != nil {
			t.Fatalf("KeypackageFetch: %v", err)
		}
		var body struct {
			NestURL string `cbor:"nest_url"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.NestURL != "https://peer.example" {
			t.Errorf("nest_url: got %q", body.NestURL)
		}
	})
}

func TestFetchCapabilityGrants(t *testing.T) {
	ctx := context.Background()

	t.Run("emptyBody_and_unpacksGrants", func(t *testing.T) {
		grantA := []byte{0x01, 0x02, 0x03}
		grantB := []byte{0xAA, 0xBB}
		caller := &recordingCaller{
			replyBody: fetchCapabilityGrantsReply{Grants: [][]byte{grantA, grantB}},
		}

		grants, err := FetchCapabilityGrants(ctx, caller)
		if err != nil {
			t.Fatalf("FetchCapabilityGrants: %v", err)
		}
		if caller.gotMethod != MethodFetchCapabilityGrants {
			t.Errorf("method: got %q, want fauna.capabilities.fetch", caller.gotMethod)
		}
		// The request body carries no spoofable field — encodes to an empty map.
		var body map[string]any
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(body) != 0 {
			t.Errorf("body: got %v, want empty map", body)
		}
		if len(grants) != 2 || !bytes.Equal(grants[0], grantA) || !bytes.Equal(grants[1], grantB) {
			t.Errorf("grants: got %v, want [%v %v]", grants, grantA, grantB)
		}
	})

	t.Run("emptyReply_isNotAnError", func(t *testing.T) {
		// A successful fetch that lists zero grants (all revoked/expired) is a
		// valid, authoritative "go dark" verdict — NOT a transport error.
		caller := &recordingCaller{replyBody: fetchCapabilityGrantsReply{Grants: nil}}
		grants, err := FetchCapabilityGrants(ctx, caller)
		if err != nil {
			t.Fatalf("FetchCapabilityGrants: %v", err)
		}
		if len(grants) != 0 {
			t.Errorf("grants: got %v, want empty", grants)
		}
	})

	t.Run("transportError_propagates", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("boom")}
		if _, err := FetchCapabilityGrants(ctx, caller); err == nil {
			t.Fatal("expected transport error to propagate")
		}
	})
}

func TestRescoreWorklist(t *testing.T) {
	ctx := context.Background()

	t.Run("sendsLimit_and_decodesUnits", func(t *testing.T) {
		unit := RescoreUnit{
			ContentID:    bytes.Repeat([]byte{0x11}, 32),
			ContentKind:  "mail",
			OwnerActorID: bytes.Repeat([]byte{0x22}, 32),
			Factor:       "clamav",
			FromVersion:  1,
			ToVersion:    3,
		}
		caller := &recordingCaller{replyBody: rescoreWorklistReply{Units: []RescoreUnit{unit}}}

		units, err := RescoreWorklist(ctx, caller, 128)
		if err != nil {
			t.Fatalf("RescoreWorklist: %v", err)
		}
		if caller.gotMethod != MethodRescoreWorklist {
			t.Errorf("method: got %q, want fauna.capabilities.rescore_worklist", caller.gotMethod)
		}
		var body struct {
			Limit uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.Limit != 128 {
			t.Errorf("limit: got %d, want 128", body.Limit)
		}
		if len(units) != 1 || units[0].Factor != "clamav" || units[0].ToVersion != 3 ||
			!bytes.Equal(units[0].ContentID, unit.ContentID) ||
			!bytes.Equal(units[0].OwnerActorID, unit.OwnerActorID) {
			t.Errorf("units round-trip mismatch: %+v", units)
		}
	})

	t.Run("emptyReply_meansNothingOwed", func(t *testing.T) {
		caller := &recordingCaller{replyBody: rescoreWorklistReply{}}
		units, err := RescoreWorklist(ctx, caller, 0)
		if err != nil {
			t.Fatalf("RescoreWorklist: %v", err)
		}
		if len(units) != 0 {
			t.Errorf("units: got %v, want empty", units)
		}
	})
}

func TestSubmitScores(t *testing.T) {
	ctx := context.Background()

	t.Run("sendsRows_and_decodesWritten", func(t *testing.T) {
		row := SubmitScoreRow{
			ContentID:    bytes.Repeat([]byte{0x11}, 32),
			ContentKind:  "mail",
			OwnerActorID: bytes.Repeat([]byte{0x22}, 32),
			ScoredAt:     1751800000,
			Entries: []ScoreEntry{
				{Factor: "clamav", Score: 0, Tier: 2, ScorerVersion: 3},
			},
		}
		caller := &recordingCaller{replyBody: submitScoresReply{Written: 1, Ok: true}}

		written, err := SubmitScores(ctx, caller, []SubmitScoreRow{row})
		if err != nil {
			t.Fatalf("SubmitScores: %v", err)
		}
		if caller.gotMethod != MethodSubmitScores {
			t.Errorf("method: got %q, want fauna.capabilities.submit_scores", caller.gotMethod)
		}
		if written != 1 {
			t.Errorf("written: got %d, want 1", written)
		}
		var body struct {
			Rows []SubmitScoreRow `cbor:"rows"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if len(body.Rows) != 1 || body.Rows[0].ScoredAt != 1751800000 ||
			len(body.Rows[0].Entries) != 1 ||
			body.Rows[0].Entries[0].ScorerVersion != 3 {
			t.Errorf("rows round-trip mismatch: %+v", body.Rows)
		}
	})

	t.Run("okFalse_isAnError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: submitScoresReply{Written: 0, Ok: false}}
		if _, err := SubmitScores(ctx, caller, nil); err == nil {
			t.Fatal("ok=false must surface as an error")
		}
	})
}
